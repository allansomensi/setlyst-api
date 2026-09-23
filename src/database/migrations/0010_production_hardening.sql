-- Production hardening: account security & moderation, audit trail,
-- per-user resource quotas, song tags, public-link moderation and
-- persisted UI settings.

-- ---------------------------------------------------------------------
-- Account security & moderation
-- ---------------------------------------------------------------------

ALTER TABLE users
    -- Bumped whenever every existing session must stop working: password
    -- change, admin reset, "sign out everywhere". Carried in the JWT as
    -- `ver`; a token whose version doesn't match the row is rejected.
    ADD COLUMN token_version INTEGER NOT NULL DEFAULT 0,
    -- Set when the account must pick a new password before doing anything
    -- else: an admin-issued temporary password, or a legacy password that
    -- no longer satisfies the password policy (detected at sign-in).
    ADD COLUMN must_change_password BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN password_changed_at TIMESTAMP,
    -- A suspension. `banned_at` set and `banned_until` NULL means
    -- permanent; a `banned_until` in the past means the suspension has
    -- simply expired. Distinct from `status = 'inactive'` (deactivation),
    -- which has no end date and is typically the account owner's or an
    -- admin's administrative decision rather than a sanction.
    ADD COLUMN banned_at TIMESTAMP,
    ADD COLUMN banned_until TIMESTAMP,
    ADD COLUMN ban_reason VARCHAR(500),
    ADD COLUMN banned_by UUID REFERENCES users(id) ON DELETE SET NULL,
    -- Audit: who created the account (NULL = self-registration or the
    -- superuser script) and who last changed it.
    ADD COLUMN created_by UUID REFERENCES users(id) ON DELETE SET NULL,
    ADD COLUMN updated_by UUID REFERENCES users(id) ON DELETE SET NULL;

-- ---------------------------------------------------------------------
-- Content audit: who changed each record last. `user_id` already records
-- the creator.
-- ---------------------------------------------------------------------

ALTER TABLE artists ADD COLUMN updated_by UUID REFERENCES users(id) ON DELETE SET NULL;
ALTER TABLE songs ADD COLUMN updated_by UUID REFERENCES users(id) ON DELETE SET NULL;
ALTER TABLE setlists ADD COLUMN updated_by UUID REFERENCES users(id) ON DELETE SET NULL;
ALTER TABLE gigs ADD COLUMN updated_by UUID REFERENCES users(id) ON DELETE SET NULL;
ALTER TABLE bands ADD COLUMN updated_by UUID REFERENCES users(id) ON DELETE SET NULL;

-- A band must survive its creator's account being deleted (ownership is
-- tracked by `band_members.role = 'owner'`, not by this column), so the
-- creator reference becomes nullable instead of blocking the deletion.
ALTER TABLE bands ALTER COLUMN created_by DROP NOT NULL;
ALTER TABLE bands DROP CONSTRAINT bands_created_by_fkey;
ALTER TABLE bands
    ADD CONSTRAINT bands_created_by_fkey
    FOREIGN KEY (created_by) REFERENCES users(id) ON DELETE SET NULL;

-- ---------------------------------------------------------------------
-- Public-link moderation. A locked share can't be re-enabled by its owner
-- until staff unlocks it.
-- ---------------------------------------------------------------------

ALTER TABLE setlists
    ADD COLUMN share_locked_at TIMESTAMP,
    ADD COLUMN share_locked_by UUID REFERENCES users(id) ON DELETE SET NULL,
    ADD COLUMN share_lock_reason VARCHAR(500);

ALTER TABLE gigs
    ADD COLUMN share_locked_at TIMESTAMP,
    ADD COLUMN share_locked_by UUID REFERENCES users(id) ON DELETE SET NULL,
    ADD COLUMN share_lock_reason VARCHAR(500);

-- ---------------------------------------------------------------------
-- Song tags: free-form, normalized (lowercase, trimmed) by the API.
-- ---------------------------------------------------------------------

CREATE TABLE song_tags (
    song_id UUID NOT NULL REFERENCES songs(id) ON DELETE CASCADE,
    tag VARCHAR(30) NOT NULL,
    created_at TIMESTAMP NOT NULL DEFAULT (NOW() AT TIME ZONE 'utc'),
    PRIMARY KEY (song_id, tag)
);

CREATE INDEX idx_song_tags_tag ON song_tags(tag);

-- ---------------------------------------------------------------------
-- Resource quotas. Platform-wide defaults live in `platform_settings`
-- (key 'quota_defaults'); `user_quotas` holds per-user overrides, where a
-- missing key means "use the default".
-- ---------------------------------------------------------------------

CREATE TABLE platform_settings (
    key VARCHAR(64) PRIMARY KEY,
    value JSONB NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    updated_by UUID REFERENCES users(id) ON DELETE SET NULL
);

CREATE TABLE user_quotas (
    user_id UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    overrides JSONB NOT NULL DEFAULT '{}',
    -- Exempts the account from every limit (staff, trusted partners).
    unlimited BOOLEAN NOT NULL DEFAULT FALSE,
    updated_at TIMESTAMP NOT NULL,
    updated_by UUID REFERENCES users(id) ON DELETE SET NULL
);

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
CREATE INDEX idx_audit_logs_target_id ON audit_logs(target_id);
CREATE INDEX idx_audit_logs_action ON audit_logs(action);

-- ---------------------------------------------------------------------
-- Persisted UI settings (live mode defaults, PDF defaults, list sizes,
-- "what's new" read state...). Owned by the web client; the API only
-- guarantees it's a bounded JSON object.
-- ---------------------------------------------------------------------

ALTER TABLE user_preferences ADD COLUMN ui_settings JSONB NOT NULL DEFAULT '{}';

-- ---------------------------------------------------------------------
-- New notification kinds.
-- ---------------------------------------------------------------------

ALTER TYPE notification_type ADD VALUE IF NOT EXISTS 'band_member_added';
ALTER TYPE notification_type ADD VALUE IF NOT EXISTS 'share_link_revoked';
