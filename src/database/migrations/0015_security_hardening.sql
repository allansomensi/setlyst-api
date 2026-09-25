-- Security hardening (audit follow-up).
--
-- `email_first_verified_at`: the first time the account's address was
-- ever proven, and never cleared afterwards. `email_verified_at` is
-- cleared when staff set a new address (it must be verified again), which
-- made such an account look like one whose address was *never* owned:
-- the next password recovery then treated the new proof as the "first
-- proof" and wiped the second factor and the linked sign-in providers
-- (the anti-squatting rule). A staff e-mail change followed by a password
-- recovery could therefore sign in as a two-factor-protected account.
-- With this column, the wipe only ever applies to accounts that never
-- verified any address.

ALTER TABLE users ADD COLUMN email_first_verified_at TIMESTAMP;

UPDATE users SET email_first_verified_at = email_verified_at
WHERE email_verified_at IS NOT NULL;

-- Two-factor authentication can only be enabled on a verified account:
-- an account with it on was verified at some point, whatever
-- `email_verified_at` says today.
UPDATE users SET email_first_verified_at = COALESCE(email_first_verified_at, totp_enabled_at)
WHERE totp_enabled_at IS NOT NULL;
