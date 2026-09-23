-- Second-factor checks outside sign-in (disabling 2FA, regenerating the
-- recovery codes) are limited per account: someone holding a stolen
-- session must not be able to brute-force the 6-digit code. One row per
-- account, a fixed window that starts at the first failure.
CREATE TABLE second_factor_attempts (
    user_id UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    window_started_at TIMESTAMP NOT NULL,
    failures INTEGER NOT NULL DEFAULT 0
);

-- Invalidating older challenges on a new sign-in looks them up by user
-- among the unconsumed ones.
CREATE INDEX idx_login_challenges_live ON login_challenges (user_id)
    WHERE consumed_at IS NULL;
