-- User accounts: identity, credentials, moderation state, public profile,
-- consent, referral and payment-provider references, plus preferences,
-- username history and the username quarantine.

CREATE TYPE user_role AS ENUM ('user', 'moderator', 'admin');
CREATE TYPE user_status AS ENUM ('active', 'inactive');

CREATE TABLE users (
    id UUID PRIMARY KEY,
    username VARCHAR(30) NOT NULL,
    -- Up to 254 characters (RFC 5321), like every other e-mail column.
    email VARCHAR(254) UNIQUE,
    password_hash VARCHAR(255) NOT NULL,
    first_name VARCHAR(50),
    last_name VARCHAR(50),
    role user_role NOT NULL,
    status user_status NOT NULL,
    -- Set on every successful login; NULL means the account has never
    -- logged in yet — used to suppress the "Welcome back" toast on a
    -- brand new account's first sign-in.
    last_login_at TIMESTAMP,
    -- Set whenever the username changes, to enforce a 90-day cooldown
    -- between changes. NULL means it has never been changed since the
    -- account was created.
    username_changed_at TIMESTAMP,
    -- Last change of the platform role. Staff accounts must enable
    -- two-factor authentication; the short grace period after becoming
    -- staff counts from here (from `created_at` when NULL).
    role_changed_at TIMESTAMP,

    -- Credentials and sessions.
    -- Bumped whenever every existing session must stop working: password
    -- change, admin reset, "sign out everywhere". Carried in the JWT as
    -- `ver`; a token whose version doesn't match the row is rejected.
    token_version INTEGER NOT NULL DEFAULT 0,
    -- Set when the account must pick a new password before doing anything
    -- else: an admin-issued temporary password, or a password that no
    -- longer satisfies the password policy (detected at sign-in).
    must_change_password BOOLEAN NOT NULL DEFAULT FALSE,
    password_changed_at TIMESTAMP,
    -- FALSE for accounts created through an identity provider that never
    -- chose a password. Such accounts sign in with the provider, or set a
    -- password through the recovery flow.
    password_set BOOLEAN NOT NULL DEFAULT TRUE,
    -- Set once the owner proves they control `email` (code sent by
    -- e-mail, or an identity provider that vouches for it). Password
    -- recovery, e-mail communications and referral rewards require it.
    email_verified_at TIMESTAMP,
    -- TOTP (RFC 6238). Secrets are encrypted by the application
    -- (AES-256-GCM) before they reach this table. `totp_pending_*` holds
    -- a secret that was generated but not yet confirmed with a code.
    totp_secret_enc TEXT,
    totp_enabled_at TIMESTAMP,
    totp_pending_secret_enc TEXT,
    totp_pending_created_at TIMESTAMP,
    -- The last TOTP time step accepted, so a code (valid for up to 90
    -- seconds with the drift window) can't be replayed once it has been
    -- used.
    totp_last_step BIGINT,
    -- Per-account brute-force protection, independent of the client IP.
    failed_login_count INTEGER NOT NULL DEFAULT 0,
    locked_until TIMESTAMP,

    -- A suspension. `banned_at` set and `banned_until` NULL means
    -- permanent; a `banned_until` in the past means the suspension has
    -- simply expired. Distinct from `status = 'inactive'` (deactivation),
    -- which has no end date and is typically the account owner's or an
    -- admin's administrative decision rather than a sanction.
    banned_at TIMESTAMP,
    banned_until TIMESTAMP,
    ban_reason VARCHAR(500),
    banned_by UUID REFERENCES users(id) ON DELETE SET NULL,

    -- Public profile. The avatar is a link to an externally hosted image
    -- (nothing is stored here); it is proxied and moderated.
    avatar_url VARCHAR(500),
    avatar_updated_at TIMESTAMP,
    bio VARCHAR(280),
    location VARCHAR(80),
    instruments TEXT[] NOT NULL DEFAULT '{}',

    -- Consent (LGPD art. 7, I and art. 8): which version of the Terms of
    -- Use / Privacy Policy the account last accepted, and when. The full
    -- history lives in `legal_acceptances`.
    terms_accepted_at TIMESTAMP,
    terms_version VARCHAR(20),
    -- When the account declared being 18 or older (or 16/17 with a
    -- guardian's authorization) at sign-up (LGPD art. 14).
    age_attested_at TIMESTAMP,

    -- Referral programme. Every account gets a code from the application.
    referral_code VARCHAR(16),
    referred_by UUID REFERENCES users(id) ON DELETE SET NULL,

    -- The Stripe customer of the account, created on its first checkout.
    stripe_customer_id VARCHAR(255),
    -- Part of the customer-creation idempotency key: bumped when the stored
    -- customer turns out to be deleted at Stripe, so a new one is created
    -- instead of replaying the dead one for 24 hours.
    stripe_customer_generation INTEGER NOT NULL DEFAULT 0,

    -- Audit: who created the account (NULL = self-registration or the
    -- superuser script) and who last changed it.
    created_by UUID REFERENCES users(id) ON DELETE SET NULL,
    updated_by UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL
);

-- Case-insensitive: "Augusto" and "augusto" must be the same account,
-- never two different ones. The same goes for e-mail addresses.
CREATE UNIQUE INDEX idx_users_username_lower ON users (LOWER(username));
CREATE UNIQUE INDEX idx_users_email_lower ON users (LOWER(email)) WHERE email IS NOT NULL;

CREATE UNIQUE INDEX idx_users_referral_code ON users (referral_code);
CREATE INDEX idx_users_referred_by ON users (referred_by) WHERE referred_by IS NOT NULL;
CREATE UNIQUE INDEX idx_users_stripe_customer ON users (stripe_customer_id)
    WHERE stripe_customer_id IS NOT NULL;

-- The unverified-account purge looks accounts up by verification state
-- and age.
CREATE INDEX idx_users_unverified_created ON users (created_at) WHERE email_verified_at IS NULL;

-- Foreign keys to `users` itself: deleting an account sets these to NULL;
-- without an index every deletion scans the whole table. Partial, since
-- the columns are mostly NULL.
CREATE INDEX idx_users_banned_by ON users (banned_by) WHERE banned_by IS NOT NULL;
CREATE INDEX idx_users_created_by ON users (created_by) WHERE created_by IS NOT NULL;
CREATE INDEX idx_users_updated_by ON users (updated_by) WHERE updated_by IS NOT NULL;

-- ---------------------------------------------------------------------
-- Preferences
-- ---------------------------------------------------------------------

CREATE TYPE user_theme AS ENUM ('light', 'dark', 'system');

CREATE TABLE user_preferences (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL UNIQUE REFERENCES users(id) ON DELETE CASCADE,
    language VARCHAR(10) NOT NULL DEFAULT 'en',
    theme user_theme NOT NULL DEFAULT 'system',
    live_mode_font_size INTEGER NOT NULL DEFAULT 100,
    -- Persisted UI settings (live mode defaults, PDF defaults, list sizes,
    -- "what's new" read state...). Owned by the web client; the API only
    -- guarantees it's a bounded JSON object.
    ui_settings JSONB NOT NULL DEFAULT '{}',
    -- Communication preferences (which categories reach the user by
    -- e-mail and in the app). Shape validated by the API; missing keys
    -- fall back to the defaults defined in `models::communication`.
    communication JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- ---------------------------------------------------------------------
-- Usernames
-- ---------------------------------------------------------------------

-- Every past username a user has held, visible to platform admins only —
-- e.g. to trace an account across a rename, or investigate impersonation
-- via a recently-vacated name.
CREATE TABLE username_history (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    old_username VARCHAR(30) NOT NULL,
    changed_at TIMESTAMP NOT NULL
);

CREATE INDEX idx_username_history_user_id ON username_history(user_id);

-- Names given up by a rename or an account deletion stay reserved for 90
-- days, for everyone but their previous owner, so nobody can pick up a
-- known band member's old name to impersonate them. No foreign key: the
-- previous owner may be gone.
CREATE TABLE released_usernames (
    username_lower VARCHAR(30) PRIMARY KEY,
    previous_owner UUID,
    released_at TIMESTAMP NOT NULL
);
