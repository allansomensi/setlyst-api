-- Administration: platform settings, per-user resource quotas, the audit
-- log and content moderation.

-- ---------------------------------------------------------------------
-- Platform settings and resource quotas. Platform-wide defaults live in
-- `platform_settings` (key 'quota_defaults'; built-in values when absent);
-- `user_quotas` holds per-user overrides, where a missing key means "use
-- the default".
-- ---------------------------------------------------------------------

CREATE TABLE platform_settings (
    key VARCHAR(64) PRIMARY KEY,
    value JSONB NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    updated_by UUID REFERENCES users(id) ON DELETE SET NULL
);

CREATE INDEX idx_platform_settings_updated_by ON platform_settings (updated_by) WHERE updated_by IS NOT NULL;

CREATE TABLE user_quotas (
    user_id UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    overrides JSONB NOT NULL DEFAULT '{}',
    -- Exempts the account from every limit (staff, trusted partners).
    unlimited BOOLEAN NOT NULL DEFAULT FALSE,
    updated_at TIMESTAMP NOT NULL,
    updated_by UUID REFERENCES users(id) ON DELETE SET NULL
);

CREATE INDEX idx_user_quotas_updated_by ON user_quotas (updated_by) WHERE updated_by IS NOT NULL;

-- ---------------------------------------------------------------------
-- Audit log of security-relevant and staff actions. Actor and target
-- labels are snapshotted so entries stay readable after the referenced
-- rows are renamed or deleted.
-- ---------------------------------------------------------------------

CREATE TABLE audit_logs (
    id UUID PRIMARY KEY,
    actor_id UUID REFERENCES users(id) ON DELETE SET NULL,
    actor_username VARCHAR(30),
    -- Set when the action was performed while viewing as another user.
    impersonator_id UUID REFERENCES users(id) ON DELETE SET NULL,
    action VARCHAR(64) NOT NULL,
    target_type VARCHAR(32),
    target_id UUID,
    target_label VARCHAR(255),
    metadata JSONB NOT NULL DEFAULT '{}',
    ip_address VARCHAR(64),
    created_at TIMESTAMP NOT NULL
);

CREATE INDEX idx_audit_logs_created_at ON audit_logs(created_at DESC);
CREATE INDEX idx_audit_logs_actor_id ON audit_logs(actor_id);
CREATE INDEX idx_audit_logs_impersonator ON audit_logs (impersonator_id) WHERE impersonator_id IS NOT NULL;
CREATE INDEX idx_audit_logs_target_id ON audit_logs(target_id);
CREATE INDEX idx_audit_logs_action ON audit_logs(action);

-- ---------------------------------------------------------------------
-- Moderation: flags raised on profile images, usernames and band logos,
-- either automatically (word list, blocked domains, optional image
-- classification) or by user reports. Lyrics, setlist names and gig names
-- are deliberately out of scope.
-- ---------------------------------------------------------------------

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
CREATE INDEX idx_moderation_flags_band ON moderation_flags (band_id) WHERE band_id IS NOT NULL;
CREATE INDEX idx_moderation_flags_reported_by ON moderation_flags (reported_by) WHERE reported_by IS NOT NULL;
CREATE INDEX idx_moderation_flags_resolved_by ON moderation_flags (resolved_by) WHERE resolved_by IS NOT NULL;

-- The same value is flagged automatically at most once while open.
CREATE UNIQUE INDEX idx_moderation_flags_open_auto
    ON moderation_flags (target_type, user_id, COALESCE(band_id, '00000000-0000-0000-0000-000000000000'::uuid), value)
    WHERE status = 'open' AND source = 'automatic';

-- A reporter can only have one open report per target.
CREATE UNIQUE INDEX idx_moderation_flags_open_report
    ON moderation_flags (target_type, user_id, reported_by)
    WHERE status = 'open' AND source = 'report';
