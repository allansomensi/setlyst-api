-- Setlists: their songs and markers, public sharing, the trash and the
-- band repertoire.

CREATE TABLE setlists (
    id UUID PRIMARY KEY,
    title VARCHAR(255) NOT NULL,
    description TEXT,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- The band this setlist belongs to, or NULL for a personal setlist.
    band_id UUID REFERENCES bands(id) ON DELETE CASCADE,
    -- The band repertoire: one special setlist per band that collects
    -- every song the band plays. Songs added to any setlist of the band
    -- are added to it automatically. It can't be deleted or renamed.
    is_repertoire BOOLEAN NOT NULL DEFAULT FALSE,
    -- [{"url": "https://...", "label": "..."}], same as `songs.links`.
    links JSONB NOT NULL DEFAULT '[]',
    -- NULL means public sharing is off. When set, this token resolves the
    -- setlist through the unauthenticated /public/setlists/{token} routes.
    -- Long and random by construction (see utils::share_token) —
    -- unguessable is the entire security model here.
    share_token VARCHAR(64) UNIQUE,
    -- Public-link moderation. A locked share can't be re-enabled by its
    -- owner until staff unlocks it.
    share_locked_at TIMESTAMP,
    share_locked_by UUID REFERENCES users(id) ON DELETE SET NULL,
    share_lock_reason VARCHAR(500),
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    -- Who changed the record last. `user_id` already records the creator.
    updated_by UUID REFERENCES users(id) ON DELETE SET NULL,
    -- Trash (see 0004_catalog.sql).
    deleted_at TIMESTAMP,
    deleted_by UUID REFERENCES users(id) ON DELETE SET NULL,
    trash_batch UUID
);

CREATE INDEX idx_setlists_user_id ON setlists(user_id);
CREATE INDEX idx_setlists_band_id ON setlists(band_id);
CREATE INDEX idx_setlists_share_token ON setlists(share_token);
CREATE INDEX idx_setlists_deleted ON setlists (deleted_at) WHERE deleted_at IS NOT NULL;
CREATE INDEX idx_setlists_updated_by ON setlists (updated_by) WHERE updated_by IS NOT NULL;
CREATE INDEX idx_setlists_deleted_by ON setlists (deleted_by) WHERE deleted_by IS NOT NULL;
CREATE INDEX idx_setlists_share_locked_by ON setlists (share_locked_by) WHERE share_locked_by IS NOT NULL;
CREATE UNIQUE INDEX idx_setlists_band_repertoire ON setlists (band_id) WHERE is_repertoire;

CREATE TABLE setlist_songs (
    setlist_id UUID REFERENCES setlists(id) ON DELETE CASCADE,
    song_id UUID REFERENCES songs(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    PRIMARY KEY (setlist_id, song_id)
);

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
-- lookup, public share lookup) reports the same number. Songs in the
-- trash don't count.
CREATE FUNCTION setlist_total_duration(p_setlist_id UUID) RETURNS INTEGER AS $$
    SELECT (
        COALESCE(
            (SELECT SUM(so.duration) FROM setlist_songs ss
             JOIN songs so ON so.id = ss.song_id
             WHERE ss.setlist_id = p_setlist_id AND so.deleted_at IS NULL), 0
        )
        + COALESCE(
            (SELECT SUM(sm.duration_minutes) * 60 FROM setlist_markers sm
             WHERE sm.setlist_id = p_setlist_id AND sm.marker_type = 'break'), 0
        )
    )::integer;
$$ LANGUAGE sql STABLE;
