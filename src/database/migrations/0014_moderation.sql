-- Moderation: flags raised on profile images, usernames and band logos,
-- either automatically (word list, blocked domains, optional image
-- classification) or by user reports. Lyrics, setlist names and gig names
-- are deliberately out of scope.

CREATE TYPE moderation_target AS ENUM ('avatar', 'username', 'band_logo', 'profile');
CREATE TYPE moderation_status AS ENUM ('open', 'dismissed', 'actioned');
CREATE TYPE moderation_source AS ENUM ('automatic', 'report');

CREATE TABLE moderation_flags (
    id UUID PRIMARY KEY,
    target_type moderation_target NOT NULL,
    -- The account concerned (for a band logo: the band's owner at the
    -- time of the flag).
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    band_id UUID REFERENCES bands(id) ON DELETE CASCADE,
    -- Snapshot of the flagged value (URL or name) when the flag was raised.
    value VARCHAR(500) NOT NULL,
    -- Machine-readable reasons: 'offensive_term', 'sexual_term',
    -- 'blocked_domain', 'nsfw_image', 'violent_image', 'user_report'...
    reasons TEXT[] NOT NULL DEFAULT '{}',
    -- 0..1 confidence, when the check produces one.
    score REAL,
    details JSONB NOT NULL DEFAULT '{}',
    source moderation_source NOT NULL,
    reported_by UUID REFERENCES users(id) ON DELETE SET NULL,
    report_note VARCHAR(500),
    status moderation_status NOT NULL DEFAULT 'open',
    -- 'dismissed', 'avatar_removed', 'band_logo_removed',
    -- 'username_reset', 'user_banned'.
    resolution VARCHAR(40),
    resolution_note VARCHAR(500),
    resolved_by UUID REFERENCES users(id) ON DELETE SET NULL,
    resolved_at TIMESTAMP,
    created_at TIMESTAMP NOT NULL
);

CREATE INDEX idx_moderation_flags_status ON moderation_flags (status, created_at DESC);
CREATE INDEX idx_moderation_flags_user ON moderation_flags (user_id);

-- The same value is flagged automatically at most once while open.
CREATE UNIQUE INDEX idx_moderation_flags_open_auto
    ON moderation_flags (target_type, user_id, COALESCE(band_id, '00000000-0000-0000-0000-000000000000'::uuid), value)
    WHERE status = 'open' AND source = 'automatic';

-- A reporter can only have one open report per target.
CREATE UNIQUE INDEX idx_moderation_flags_open_report
    ON moderation_flags (target_type, user_id, reported_by)
    WHERE status = 'open' AND source = 'report';
