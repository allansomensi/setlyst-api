-- Accounts: e-mail verification, password recovery, two-factor
-- authentication, Google sign-in, brute-force protection, public profile
-- fields and consent tracking.

-- ---------------------------------------------------------------------
-- Users
-- ---------------------------------------------------------------------

ALTER TABLE users
    -- Set once the owner proves they control `email` (code sent by
    -- e-mail, or an identity provider that vouches for it). Password
    -- recovery, e-mail communications and referral rewards require it.
    ADD COLUMN email_verified_at TIMESTAMP,
    -- FALSE for accounts created through an identity provider that never
    -- chose a password. Such accounts sign in with the provider, or set a
    -- password through the recovery flow.
    ADD COLUMN password_set BOOLEAN NOT NULL DEFAULT TRUE,
    -- Public profile. The avatar is a link to an externally hosted image
    -- (nothing is stored here); it is proxied and moderated.
    ADD COLUMN avatar_url VARCHAR(500),
    ADD COLUMN avatar_updated_at TIMESTAMP,
    ADD COLUMN bio VARCHAR(280),
    ADD COLUMN location VARCHAR(80),
    ADD COLUMN instruments TEXT[] NOT NULL DEFAULT '{}',
    -- TOTP (RFC 6238). Secrets are encrypted by the application
    -- (AES-256-GCM) before they reach this table. `totp_pending_*` holds
    -- a secret that was generated but not yet confirmed with a code.
    ADD COLUMN totp_secret_enc TEXT,
    ADD COLUMN totp_enabled_at TIMESTAMP,
    ADD COLUMN totp_pending_secret_enc TEXT,
    ADD COLUMN totp_pending_created_at TIMESTAMP,
    -- Per-account brute-force protection, independent of the client IP.
    ADD COLUMN failed_login_count INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN locked_until TIMESTAMP,
    -- Consent (LGPD art. 7, I and art. 8): which version of the Terms of
    -- Use / Privacy Policy the account last accepted, and when.
    ADD COLUMN terms_accepted_at TIMESTAMP,
    ADD COLUMN terms_version VARCHAR(20),
    -- Referral programme.
    ADD COLUMN referral_code VARCHAR(16),
    ADD COLUMN referred_by UUID REFERENCES users(id) ON DELETE SET NULL;

-- Every existing account gets a referral code. New accounts get one from
-- the application.
UPDATE users
SET referral_code = UPPER(SUBSTRING(REPLACE(gen_random_uuid()::text, '-', '') FROM 1 FOR 10))
WHERE referral_code IS NULL;

CREATE UNIQUE INDEX idx_users_referral_code ON users (referral_code);
CREATE INDEX idx_users_referred_by ON users (referred_by) WHERE referred_by IS NOT NULL;

-- E-mail addresses must be unique regardless of case. Older databases may
-- already hold addresses that only differ by case; in that case the index
-- is not created (the application still enforces uniqueness on writes)
-- and a notice is logged so the duplicates can be reviewed by hand.
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM users
        WHERE email IS NOT NULL
        GROUP BY LOWER(email)
        HAVING COUNT(*) > 1
    ) THEN
        CREATE UNIQUE INDEX idx_users_email_lower ON users (LOWER(email)) WHERE email IS NOT NULL;
    ELSE
        RAISE NOTICE 'users.email has case-insensitive duplicates; idx_users_email_lower was not created';
        CREATE INDEX idx_users_email_lower ON users (LOWER(email)) WHERE email IS NOT NULL;
    END IF;
END $$;

-- ---------------------------------------------------------------------
-- One-time codes sent by e-mail
-- ---------------------------------------------------------------------

CREATE TYPE verification_purpose AS ENUM (
    'email_verification',
    'password_reset',
    'email_change'
);

CREATE TABLE verification_codes (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    purpose verification_purpose NOT NULL,
    -- HMAC-SHA256 of the code, keyed with a server secret. The plain code
    -- only ever exists in the e-mail.
    code_hash VARCHAR(128) NOT NULL,
    -- The address the code was sent to (for `email_change`, the new one).
    target_email VARCHAR(254) NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    expires_at TIMESTAMP NOT NULL,
    consumed_at TIMESTAMP,
    ip_address VARCHAR(64),
    created_at TIMESTAMP NOT NULL
);

CREATE INDEX idx_verification_codes_user ON verification_codes (user_id, purpose, created_at DESC);
CREATE INDEX idx_verification_codes_expires ON verification_codes (expires_at);

-- ---------------------------------------------------------------------
-- Second step of a sign-in (two-factor authentication)
-- ---------------------------------------------------------------------

CREATE TABLE login_challenges (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- SHA-256 of the opaque challenge token handed to the client.
    token_hash VARCHAR(128) NOT NULL UNIQUE,
    -- How the first factor was satisfied: 'password' or 'google'.
    method VARCHAR(16) NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    expires_at TIMESTAMP NOT NULL,
    consumed_at TIMESTAMP,
    ip_address VARCHAR(64),
    created_at TIMESTAMP NOT NULL
);

CREATE INDEX idx_login_challenges_user ON login_challenges (user_id);
CREATE INDEX idx_login_challenges_expires ON login_challenges (expires_at);

CREATE TABLE totp_recovery_codes (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    code_hash VARCHAR(128) NOT NULL,
    used_at TIMESTAMP,
    created_at TIMESTAMP NOT NULL
);

CREATE INDEX idx_totp_recovery_codes_user ON totp_recovery_codes (user_id);

-- ---------------------------------------------------------------------
-- External identity providers (Google)
-- ---------------------------------------------------------------------

CREATE TABLE oauth_identities (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    provider VARCHAR(20) NOT NULL,
    -- The provider's stable account identifier (`sub`).
    subject VARCHAR(255) NOT NULL,
    email VARCHAR(254),
    created_at TIMESTAMP NOT NULL,
    last_used_at TIMESTAMP,
    UNIQUE (provider, subject),
    UNIQUE (user_id, provider)
);

-- ---------------------------------------------------------------------
-- Band invite codes: longer codes (up to 32 chars) for new invites.
-- ---------------------------------------------------------------------

ALTER TABLE band_invites ALTER COLUMN code TYPE VARCHAR(32);
