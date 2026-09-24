-- Abuse hardening: e-mail caps for every template and one-time codes that
-- survive strangers' requests.

-- ---------------------------------------------------------------------
-- E-mail outbox
-- ---------------------------------------------------------------------

-- The mailbox an address really delivers to (lower case, no `+tag`, Gmail
-- dots folded), so the per-recipient caps can't be multiplied with
-- variants of one address. Old rows get the lower-cased address, which is
-- close enough for a 24-hour window.
ALTER TABLE email_outbox ADD COLUMN to_canonical VARCHAR(254);
UPDATE email_outbox SET to_canonical = LOWER(to_email) WHERE to_canonical IS NULL;
ALTER TABLE email_outbox ALTER COLUMN to_canonical SET NOT NULL;

CREATE INDEX idx_email_outbox_canonical
    ON email_outbox (to_canonical, template, created_at);

-- ---------------------------------------------------------------------
-- One-time codes
-- ---------------------------------------------------------------------

-- The code itself, encrypted at rest (AES-256-GCM under the data key), so
-- a second request for the same purpose while a code is still valid
-- re-sends *that* code instead of issuing a new one: a stranger asking
-- for a password recovery of someone else's account can no longer
-- invalidate the code the owner is about to type. Wiped as soon as the
-- code is consumed or invalidated.
ALTER TABLE verification_codes ADD COLUMN code_enc TEXT;

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
