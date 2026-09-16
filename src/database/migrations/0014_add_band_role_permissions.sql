-- Granular per-role permissions for bands. Before this migration, the
-- only customizable permission was the single `members_can_manage_setlists`
-- boolean on `bands`, which only ever governed the `member` role (`admin`
-- and `owner` are always fully trusted; `moderator` was hardcoded to
-- "can manage setlists" everywhere it was checked).
--
-- This replaces that single flag with a permission-per-(role, action)
-- matrix that a band's admin/owner can edit for the `member` and
-- `moderator` roles. `admin` and `owner` are intentionally absent from
-- this table: they always have every permission, by design, and are
-- never restricted.
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

-- Seed every existing band with permission rows that reproduce today's
-- actual behavior, so this migration doesn't silently change access for
-- anyone: moderators could already manage setlists/songs everywhere
-- (hardcoded), and PDF export never had a check at all (anyone who could
-- view a setlist could export it), so it starts allowed for both roles.
-- Members keep whatever `members_can_manage_setlists` already said.
INSERT INTO band_role_permissions (band_id, role, permission, allowed)
SELECT b.id, r.role, p.permission,
    CASE
        WHEN r.role = 'moderator' AND p.permission IN ('manage_setlists', 'manage_songs') THEN true
        WHEN r.role = 'member' AND p.permission IN ('manage_setlists', 'manage_songs') THEN b.members_can_manage_setlists
        WHEN p.permission = 'export_pdf' THEN true
        ELSE false
    END
FROM bands b
CROSS JOIN (VALUES ('member'::band_role), ('moderator'::band_role)) AS r(role)
CROSS JOIN (
    VALUES ('manage_setlists'::band_permission), ('manage_songs'::band_permission), ('export_pdf'::band_permission)
) AS p(permission);

-- New bands get default rows too (matching the seed above), inserted by
-- the application at band-creation time — see BandRepository::create.
