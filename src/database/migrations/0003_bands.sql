-- Bands: members and their roles, invites and per-role permissions.

-- Declared low-to-high on purpose: Postgres sorts enum values by their
-- declaration order, and the member listing relies on `ORDER BY role DESC`
-- to show the band's owner first.
CREATE TYPE band_role AS ENUM ('member', 'moderator', 'admin', 'owner');

CREATE TABLE bands (
    id UUID PRIMARY KEY,
    name VARCHAR(60) NOT NULL,
    slug VARCHAR(80) UNIQUE NOT NULL,
    description TEXT,
    logo_url VARCHAR(500),
    members_can_manage_setlists BOOLEAN NOT NULL DEFAULT TRUE,
    -- Song suggestions reaching this many votes are accepted
    -- automatically. NULL = never accepted automatically.
    suggestion_auto_accept_votes INTEGER
        CHECK (suggestion_auto_accept_votes IS NULL OR suggestion_auto_accept_votes BETWEEN 1 AND 100),
    -- A band must survive its creator's account being deleted (ownership
    -- is tracked by `band_members.role = 'owner'`, not by this column).
    created_by UUID REFERENCES users(id) ON DELETE SET NULL,
    updated_by UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL
);

CREATE INDEX idx_bands_created_by ON bands (created_by) WHERE created_by IS NOT NULL;
CREATE INDEX idx_bands_updated_by ON bands (updated_by) WHERE updated_by IS NOT NULL;

CREATE TABLE band_members (
    id UUID PRIMARY KEY,
    band_id UUID NOT NULL REFERENCES bands(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role band_role NOT NULL DEFAULT 'member',
    -- Free-text identification label (e.g. "Guitarrista", "Baixista") —
    -- purely cosmetic, unrelated to `role` and never affects permissions.
    title VARCHAR(50),
    joined_at TIMESTAMP NOT NULL,
    UNIQUE (band_id, user_id),
    -- One owner per band. A unique partial index, as an exclusion
    -- constraint so it can be checked at commit (DEFERRABLE INITIALLY
    -- DEFERRED): handing a band over inside a transaction (account
    -- deletion promotes the successor first and removes the old owner's
    -- membership afterwards) briefly has two owners. Its backing index is
    -- named after the constraint.
    CONSTRAINT idx_band_members_single_owner
        EXCLUDE USING btree (band_id WITH =) WHERE (role = 'owner')
        DEFERRABLE INITIALLY DEFERRED
);

CREATE INDEX idx_band_members_band_id ON band_members(band_id);
CREATE INDEX idx_band_members_user_id ON band_members(user_id);

CREATE TABLE band_invites (
    id UUID PRIMARY KEY,
    band_id UUID NOT NULL REFERENCES bands(id) ON DELETE CASCADE,
    code VARCHAR(32) UNIQUE NOT NULL,
    role band_role NOT NULL DEFAULT 'member',
    created_by UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    max_uses INTEGER,
    uses_count INTEGER NOT NULL DEFAULT 0,
    expires_at TIMESTAMP,
    revoked_at TIMESTAMP,
    created_at TIMESTAMP NOT NULL
);

CREATE INDEX idx_band_invites_band_id ON band_invites(band_id);
CREATE INDEX idx_band_invites_code ON band_invites(code);
CREATE INDEX idx_band_invites_created_by ON band_invites (created_by);
-- Retention job.
CREATE INDEX idx_band_invites_inactive ON band_invites (COALESCE(revoked_at, expires_at))
    WHERE revoked_at IS NOT NULL OR expires_at IS NOT NULL;

-- Granular per-role permissions. `admin` and `owner` always have every
-- permission and are intentionally absent from this table — only
-- `member` and `moderator` rows are meaningful. Default rows are seeded
-- by the application at band-creation time — see BandRepository::create.
CREATE TYPE band_permission AS ENUM (
    'manage_setlists',
    'manage_songs',
    'export_pdf'
);

CREATE TABLE band_role_permissions (
    band_id UUID NOT NULL REFERENCES bands(id) ON DELETE CASCADE,
    role band_role NOT NULL,
    permission band_permission NOT NULL,
    allowed BOOLEAN NOT NULL DEFAULT FALSE,
    PRIMARY KEY (band_id, role, permission),
    -- Only member/moderator rows are meaningful — admin/owner are never
    -- restricted, so storing rows for them would be misleading.
    CONSTRAINT band_role_permissions_role_check CHECK (role IN ('member', 'moderator'))
);

CREATE INDEX idx_band_role_permissions_band_id ON band_role_permissions(band_id);
