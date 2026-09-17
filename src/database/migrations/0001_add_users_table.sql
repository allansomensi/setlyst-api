-- Core user accounts, roles, and status.
CREATE TYPE user_role AS ENUM ('user', 'moderator', 'admin');
CREATE TYPE user_status AS ENUM ('active', 'inactive');

CREATE TABLE users (
    id UUID PRIMARY KEY,
    username VARCHAR(30) NOT NULL,
    email VARCHAR(100) UNIQUE,
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
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL
);

-- Case-insensitive: "Augusto" and "augusto" must be the same account,
-- never two different ones.
CREATE UNIQUE INDEX idx_users_username_lower ON users (LOWER(username));

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
