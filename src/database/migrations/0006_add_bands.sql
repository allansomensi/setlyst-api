-- Declared low-to-high on purpose: Postgres sorts enum values by their
-- declaration order, and the member listing below relies on
-- `ORDER BY role DESC` to show the band's owner first.
CREATE TYPE band_role AS ENUM ('member', 'moderator', 'admin', 'owner');

CREATE TABLE bands (
    id UUID PRIMARY KEY,
    name VARCHAR(60) NOT NULL,
    slug VARCHAR(80) UNIQUE NOT NULL,
    description TEXT,
    logo_url VARCHAR(500),
    members_can_manage_setlists BOOLEAN NOT NULL DEFAULT TRUE,
    created_by UUID NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL
);

CREATE TABLE band_members (
    id UUID PRIMARY KEY,
    band_id UUID NOT NULL REFERENCES bands(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role band_role NOT NULL DEFAULT 'member',
    joined_at TIMESTAMP NOT NULL,
    UNIQUE (band_id, user_id)
);

CREATE TABLE band_invites (
    id UUID PRIMARY KEY,
    band_id UUID NOT NULL REFERENCES bands(id) ON DELETE CASCADE,
    code VARCHAR(12) UNIQUE NOT NULL,
    role band_role NOT NULL DEFAULT 'member',
    created_by UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    max_uses INTEGER,
    uses_count INTEGER NOT NULL DEFAULT 0,
    expires_at TIMESTAMP,
    revoked_at TIMESTAMP,
    created_at TIMESTAMP NOT NULL
);

CREATE INDEX idx_band_members_band_id ON band_members(band_id);
CREATE INDEX idx_band_members_user_id ON band_members(user_id);
CREATE INDEX idx_band_invites_band_id ON band_invites(band_id);
CREATE INDEX idx_band_invites_code ON band_invites(code);
