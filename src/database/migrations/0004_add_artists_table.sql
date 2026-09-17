CREATE TABLE artists (
    id UUID PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- The band that owns this artist as an independent, shared copy, or
    -- NULL for a personal artist. Lets a band own its own copy of an
    -- artist, decoupled from any single member's personal catalog —
    -- otherwise a band setlist would depend on that member's account
    -- (deleting the artist, or the account, would break every band
    -- setlist that used it).
    band_id UUID REFERENCES bands(id) ON DELETE CASCADE,
    -- The personal artist this band-owned copy was resolved from, if any
    -- — NULL for a personal artist, or for a band artist created fresh
    -- rather than forked from an existing one. `user_id` above is kept as
    -- an audit trail ("who forked this into the band"), not as the
    -- access-control owner for band-owned rows.
    forked_from UUID REFERENCES artists(id) ON DELETE SET NULL,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL
);

CREATE INDEX idx_artists_user_id ON artists(user_id);
CREATE INDEX idx_artists_band_id ON artists(band_id);
CREATE INDEX idx_artists_forked_from ON artists(forked_from) WHERE forked_from IS NOT NULL;

-- Personal rows are unique per (name, user_id); band-owned rows are
-- unique per (band_id, name) instead, so they never collide with anyone's
-- personal catalog.
CREATE UNIQUE INDEX idx_artists_personal_unique ON artists (name, user_id) WHERE band_id IS NULL;
CREATE UNIQUE INDEX idx_artists_band_unique ON artists (band_id, name) WHERE band_id IS NOT NULL;
