-- Account security: one-time codes sent by e-mail, two-factor
-- authentication, Google sign-in, brute-force and abuse limits, and the
-- legal acceptance ledger.

-- ---------------------------------------------------------------------
-- One-time codes sent by e-mail
-- ---------------------------------------------------------------------

CREATE TYPE verification_purpose AS ENUM (
    'email_verification',
    'password_reset',
    'email_change',
    -- Step-up re-authentication of accounts without a password
    -- (Google-only): a 6-digit code e-mailed to the verified address.
    'reauth'
);

CREATE TABLE verification_codes (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    purpose verification_purpose NOT NULL,
    -- HMAC-SHA256 of the code, keyed with a server secret.
    code_hash VARCHAR(128) NOT NULL,
    -- The code itself, encrypted at rest (AES-256-GCM under the data key),
    -- so a second request for the same purpose while a code is still valid
    -- re-sends *that* code instead of issuing a new one: a stranger asking
    -- for a password recovery of someone else's account can't invalidate
    -- the code the owner is about to type. Wiped as soon as the code is
    -- consumed or invalidated.
    code_enc TEXT,
    -- The address the code was sent to (for `email_change`, the new one).
    target_email VARCHAR(254) NOT NULL,
    -- Every check of this code, including the successful use.
    attempts INTEGER NOT NULL DEFAULT 0,
    -- Wrong guesses only. Summed over 24 hours per account and purpose, it
    -- caps how many codes an attacker can guess across fresh codes.
    failed_attempts INTEGER NOT NULL DEFAULT 0,
    expires_at TIMESTAMP NOT NULL,
    consumed_at TIMESTAMP,
    ip_address VARCHAR(64),
    created_at TIMESTAMP NOT NULL
);

CREATE INDEX idx_verification_codes_user ON verification_codes (user_id, purpose, created_at DESC);
CREATE INDEX idx_verification_codes_expires ON verification_codes (expires_at);

-- Every request for a code, whether it issued a new one or re-sent the
-- live one, with the network it came from (IPv4 address or IPv6 /64, or
-- `unknown`). The resend interval and the daily/hourly allowance are
-- counted per (account, purpose, requester), with a higher ceiling per
-- account, so one network can't use up an account's whole allowance.
CREATE TABLE verification_code_requests (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    purpose verification_purpose NOT NULL,
    requester VARCHAR(64) NOT NULL,
    created_at TIMESTAMP NOT NULL
);

CREATE INDEX idx_verification_code_requests_user
    ON verification_code_requests (user_id, purpose, created_at DESC);
CREATE INDEX idx_verification_code_requests_created
    ON verification_code_requests (created_at);

-- ---------------------------------------------------------------------
-- Sign-in
-- ---------------------------------------------------------------------

-- Second step of a sign-in (two-factor authentication).
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
    -- A Google identity that is linked to the account only once the
    -- second factor of this challenge succeeded (never before: someone who
    -- can't pass the 2FA must not leave a permanent credential behind).
    pending_link_subject VARCHAR(255),
    pending_link_email VARCHAR(254),
    created_at TIMESTAMP NOT NULL
);

CREATE INDEX idx_login_challenges_user ON login_challenges (user_id);
CREATE INDEX idx_login_challenges_expires ON login_challenges (expires_at);
-- Invalidating older challenges on a new sign-in looks them up by user
-- among the unconsumed ones.
CREATE INDEX idx_login_challenges_live ON login_challenges (user_id)
    WHERE consumed_at IS NULL;

CREATE TABLE totp_recovery_codes (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    code_hash VARCHAR(128) NOT NULL,
    used_at TIMESTAMP,
    created_at TIMESTAMP NOT NULL
);

CREATE INDEX idx_totp_recovery_codes_user ON totp_recovery_codes (user_id);

-- External identity providers (Google).
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
-- Brute-force limits
-- ---------------------------------------------------------------------

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

-- Second-factor checks outside sign-in (disabling 2FA, regenerating the
-- recovery codes) are limited per account: someone holding a stolen
-- session must not be able to brute-force the 6-digit code. One row per
-- account, a fixed window that starts at the first failure.
CREATE TABLE second_factor_attempts (
    user_id UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    window_started_at TIMESTAMP NOT NULL,
    failures INTEGER NOT NULL DEFAULT 0
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
