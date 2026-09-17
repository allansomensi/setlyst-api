CREATE TABLE setlists (
    id UUID PRIMARY KEY,
    title VARCHAR(255) NOT NULL,
    description TEXT,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- The band this setlist belongs to, or NULL for a personal setlist.
    band_id UUID REFERENCES bands(id) ON DELETE CASCADE,
    -- NULL means public sharing is off. When set, this token resolves the
    -- setlist through the unauthenticated /public/setlists/{token} routes.
    -- Long and random by construction (see utils::share_token) —
    -- unguessable is the entire security model here.
    share_token VARCHAR(64) UNIQUE,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL
);

CREATE TABLE setlist_songs (
    setlist_id UUID REFERENCES setlists(id) ON DELETE CASCADE,
    song_id UUID REFERENCES songs(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    PRIMARY KEY (setlist_id, song_id)
);

CREATE INDEX idx_setlists_user_id ON setlists(user_id);
CREATE INDEX idx_setlists_band_id ON setlists(band_id);
CREATE INDEX idx_setlists_share_token ON setlists(share_token);
CREATE INDEX idx_setlist_songs_song_id ON setlist_songs(song_id);

-- Setlist "markers": visual, non-song entries that live alongside songs in
-- a setlist's running order — either a named block/section header (e.g.
-- "Bloco Baladas") or a break/pause slot (optional label + duration in
-- minutes). A separate table rather than columns on `setlist_songs`,
-- since a marker isn't a song and has no `song_id`.
--
-- `position` shares the same per-setlist ordering space as
-- `setlist_songs.position`: relative order across both tables is all that
-- matters (gaps are expected and fine).
CREATE TYPE setlist_marker_type AS ENUM ('block', 'break');

CREATE TABLE setlist_markers (
    id UUID PRIMARY KEY,
    setlist_id UUID NOT NULL REFERENCES setlists(id) ON DELETE CASCADE,
    marker_type setlist_marker_type NOT NULL,
    -- Block name (required for 'block' rows) or break label (optional,
    -- frontend falls back to a translated "Break" placeholder when null).
    label VARCHAR(255),
    -- Only meaningful for 'break' rows.
    duration_minutes INTEGER,
    position INTEGER NOT NULL,
    created_at TIMESTAMP NOT NULL
);

CREATE INDEX idx_setlist_markers_setlist_id ON setlist_markers(setlist_id);

-- Centralizes the "total duration" calculation (songs + break minutes) so
-- every place that reads a setlist (personal list, band list, single
-- lookup, public share lookup) reports the same number.
CREATE FUNCTION setlist_total_duration(p_setlist_id UUID) RETURNS INTEGER AS $$
    SELECT (
        COALESCE(
            (SELECT SUM(so.duration) FROM setlist_songs ss
             JOIN songs so ON so.id = ss.song_id
             WHERE ss.setlist_id = p_setlist_id), 0
        )
        + COALESCE(
            (SELECT SUM(sm.duration_minutes) * 60 FROM setlist_markers sm
             WHERE sm.setlist_id = p_setlist_id AND sm.marker_type = 'break'), 0
        )
    )::integer;
$$ LANGUAGE sql STABLE;
