-- Account security, second pass (launch hardening): step-up
-- re-authentication, re-auth and code failure limits, lockouts keyed by
-- network, pending Google links, one trial per e-mail address, age
-- declaration, legal acceptance ledger and username quarantine.

-- ---------------------------------------------------------------------
-- One-time codes
-- ---------------------------------------------------------------------

-- Step-up re-authentication of accounts without a password (Google-only):
-- a 6-digit code e-mailed to the verified address. The value can only be
-- used once this migration's transaction committed, which is fine: no
-- statement below uses it.
ALTER TYPE verification_purpose ADD VALUE IF NOT EXISTS 'reauth';

-- Wrong guesses per code (`attempts` also counts the successful use).
-- Summed over 24 hours per account and purpose, it caps how many codes an
-- attacker can guess across fresh codes.
ALTER TABLE verification_codes ADD COLUMN failed_attempts INTEGER NOT NULL DEFAULT 0;

-- ---------------------------------------------------------------------
-- Users
-- ---------------------------------------------------------------------

ALTER TABLE users
    -- When the account declared being 18 or older (or 16/17 with a
    -- guardian's authorization) at sign-up (LGPD art. 14). NULL for
    -- accounts created before the declaration existed.
    ADD COLUMN age_attested_at TIMESTAMP,
    -- Last change of the platform role. Staff accounts must enable
    -- two-factor authentication; the short grace period after becoming
    -- staff counts from here (from `created_at` when NULL).
    ADD COLUMN role_changed_at TIMESTAMP;

-- The unverified-account purge looks accounts up by verification state
-- and age.
CREATE INDEX idx_users_unverified_created ON users (created_at) WHERE email_verified_at IS NULL;

-- ---------------------------------------------------------------------
-- Sign-in
-- ---------------------------------------------------------------------

-- A Google identity that is linked to the account only once the second
-- factor of this challenge succeeded (never before: someone who can't
-- pass the 2FA must not leave a permanent credential behind).
ALTER TABLE login_challenges
    ADD COLUMN pending_link_subject VARCHAR(255),
    ADD COLUMN pending_link_email VARCHAR(254);

-- Failed password sign-ins per account *and* client network (IPv4
-- address or IPv6 /64). Someone guessing from one network locks only
-- that network out of the account; the owner signing in from elsewhere
-- is unaffected. The row with bucket '*' is the account-wide window
-- (a softer cap over every network).
CREATE TABLE login_failures (
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    bucket VARCHAR(64) NOT NULL,
    failures INTEGER NOT NULL DEFAULT 0,
    window_started_at TIMESTAMP NOT NULL,
    locked_until TIMESTAMP,
    updated_at TIMESTAMP NOT NULL,
    PRIMARY KEY (user_id, bucket)
);

-- Wrong passwords (or re-auth codes) typed to confirm a sensitive action
-- while signed in: a fixed short window (like `second_factor_attempts`)
-- plus a 24-hour count that, when exceeded, signs the account out
-- everywhere (the session is evidently in the wrong hands).
CREATE TABLE reauth_attempts (
    user_id UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    window_started_at TIMESTAMP NOT NULL,
    failures INTEGER NOT NULL DEFAULT 0,
    day_started_at TIMESTAMP NOT NULL,
    day_failures INTEGER NOT NULL DEFAULT 0
);

-- ---------------------------------------------------------------------
-- Trials: one per e-mail address, for good
-- ---------------------------------------------------------------------

-- Keyed HMAC of the canonical address (lower case, `+tag` removed, dots
-- removed for Gmail). Deliberately NOT tied to `users`: deleting the
-- account and signing up again with the same address must not restart
-- the trial. Only a keyed hash is kept, never the address.
CREATE TABLE trial_claims (
    email_hash BYTEA PRIMARY KEY,
    claimed_at TIMESTAMP NOT NULL
);

-- ---------------------------------------------------------------------
-- Legal acceptances (LGPD art. 8, § 2: the controller must be able to
-- prove consent)
-- ---------------------------------------------------------------------

-- One row per consent given or withdrawn: Terms of Use, Privacy Policy,
-- the age declaration and marketing e-mails. Kept when the account is
-- deleted (the user reference is cleared), as evidence for the period the
-- Privacy Policy states. The IP address and user agent are part of the
-- evidence and are therefore NOT stripped by the 183-day access-record
-- retention of the audit log.
CREATE TABLE legal_acceptances (
    id UUID PRIMARY KEY,
    user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    -- 'terms_of_use', 'privacy_policy', 'age_declaration', 'marketing_email'.
    document VARCHAR(32) NOT NULL,
    version VARCHAR(20) NOT NULL,
    -- FALSE records a withdrawal (e.g. marketing turned off).
    accepted BOOLEAN NOT NULL,
    -- 'register', 'google_signup', 'accept_terms', 'settings', 'unsubscribe_link'.
    source VARCHAR(32) NOT NULL,
    ip_address VARCHAR(64),
    user_agent VARCHAR(255),
    created_at TIMESTAMP NOT NULL
);

CREATE INDEX idx_legal_acceptances_user ON legal_acceptances (user_id, created_at DESC);

-- ---------------------------------------------------------------------
-- Usernames
-- ---------------------------------------------------------------------

-- Names given up by a rename or an account deletion stay reserved for 90
-- days, for everyone but their previous owner, so nobody can pick up a
-- known band member's old name to impersonate them. No foreign key: the
-- previous owner may be gone.
CREATE TABLE released_usernames (
    username_lower VARCHAR(30) PRIMARY KEY,
    previous_owner UUID,
    released_at TIMESTAMP NOT NULL
);

INSERT INTO released_usernames (username_lower, previous_owner, released_at)
SELECT DISTINCT ON (LOWER(h.old_username)) LOWER(h.old_username), h.user_id, h.changed_at
FROM username_history h
WHERE h.changed_at > (NOW() AT TIME ZONE 'utc') - INTERVAL '90 days'
  AND NOT EXISTS (SELECT 1 FROM users u WHERE LOWER(u.username) = LOWER(h.old_username))
ORDER BY LOWER(h.old_username), h.changed_at DESC;
