-- Case-insensitive usernames: "Augusto" and "augusto" must be the same
-- account, never two different ones. Replaces the case-sensitive UNIQUE
-- constraint from migration 0001 with a unique index on LOWER(username).
--
-- NOTE: if two accounts that differ only by case already exist, this
-- migration will fail on the CREATE UNIQUE INDEX step — that conflict has
-- to be resolved manually (rename or merge one of them) before it can run.
ALTER TABLE users DROP CONSTRAINT IF EXISTS users_username_key;
CREATE UNIQUE INDEX idx_users_username_lower ON users (LOWER(username));

-- Tracks when the username was last changed, to enforce a 90-day cooldown
-- between changes. NULL means it has never been changed since the account
-- was created (no cooldown applies yet).
ALTER TABLE users ADD COLUMN username_changed_at TIMESTAMP;

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
