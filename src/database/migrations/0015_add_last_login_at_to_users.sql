-- Tracks when a user last logged in, so the login endpoint can tell the
-- frontend whether this is their very first login (NULL before this
-- login) — used to suppress the "Welcome back" toast on a brand new
-- account's first sign-in.
ALTER TABLE users ADD COLUMN last_login_at TIMESTAMP;
