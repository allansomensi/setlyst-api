-- Accounts (platform, v0.12).

-- Two-factor authentication: the last TOTP time step accepted for each
-- account, so a code (valid for up to 90 seconds with the drift window)
-- can't be replayed once it has been used.
ALTER TABLE users ADD COLUMN totp_last_step BIGINT;

-- E-mail addresses may be up to 254 characters (RFC 5321). The original
-- column only allowed 100, which the new required-e-mail sign-up and the
-- Google sign-in would hit; every other table already uses 254.
ALTER TABLE users ALTER COLUMN email TYPE VARCHAR(254);
