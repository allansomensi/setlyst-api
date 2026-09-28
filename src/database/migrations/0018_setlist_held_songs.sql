-- Songs held by a setlist: what a collaborator contributed stays in the
-- setlist after they're gone.

-- ---------------------------------------------------------------------
-- Held songs
-- ---------------------------------------------------------------------
--
-- While someone collaborates on a setlist, it links straight to the songs
-- of their library (`setlist_songs`), so the fixes they make show up at
-- once. When that link ends — they leave or are removed, move the song
-- (or its artist) to the trash, or delete their account — the song would
-- vanish from someone else's show. Instead it is detached into the
-- setlist: a snapshot of the song, held here, belonging to no one's
-- library.
--
-- A held song is part of the setlist only: it isn't in anybody's song
-- list, quotas or exports, and it goes with the setlist when the setlist
-- is deleted. Anyone who can see the setlist can copy it into their own
-- library (`POST /setlists/{id}/songs/{song_id}/copy`); when the owner
-- does, the setlist uses their copy from then on and the held one goes.
--
-- Same running order as `setlist_songs` and `setlist_markers`: `position`
-- shares their ordering space.
CREATE TABLE setlist_held_songs (
    id UUID PRIMARY KEY,
    setlist_id UUID NOT NULL REFERENCES setlists(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    transpose SMALLINT NOT NULL DEFAULT 0 CHECK (transpose BETWEEN -11 AND 11),
    -- Who added it to the setlist, and when (from the `setlist_songs` row).
    added_by UUID REFERENCES users(id) ON DELETE SET NULL,
    added_at TIMESTAMP,
    -- When it stopped following the contributor's library.
    held_at TIMESTAMP NOT NULL,
    -- The song, as it was then (see `songs`). The artist is a name: the
    -- artist record belonged to the contributor's library.
    title VARCHAR(255) NOT NULL,
    artist_name VARCHAR(255) NOT NULL,
    version_label VARCHAR(60),
    tempo INTEGER,
    lyrics TEXT,
    tonality song_tonality,
    genre song_genre,
    duration INTEGER,
    energy SMALLINT CHECK (energy BETWEEN 1 AND 5),
    time_signature VARCHAR(8),
    capo SMALLINT CHECK (capo BETWEEN 0 AND 11),
    tuning VARCHAR(40),
    performance_notes TEXT,
    links JSONB NOT NULL DEFAULT '[]',
    tags TEXT[] NOT NULL DEFAULT '{}',
    created_at TIMESTAMP NOT NULL
);

CREATE INDEX idx_setlist_held_songs_setlist ON setlist_held_songs (setlist_id, position);
CREATE INDEX idx_setlist_held_songs_added_by ON setlist_held_songs (added_by)
    WHERE added_by IS NOT NULL;

-- Held songs count towards the setlist's duration like any other.
CREATE OR REPLACE FUNCTION setlist_total_duration(p_setlist_id UUID) RETURNS INTEGER AS $$
    SELECT (
        COALESCE(
            (SELECT SUM(so.duration) FROM setlist_songs ss
             JOIN songs so ON so.id = ss.song_id
             WHERE ss.setlist_id = p_setlist_id AND so.deleted_at IS NULL), 0
        )
        + COALESCE(
            (SELECT SUM(h.duration) FROM setlist_held_songs h
             WHERE h.setlist_id = p_setlist_id), 0
        )
        + COALESCE(
            (SELECT SUM(sm.duration_minutes) * 60 FROM setlist_markers sm
             WHERE sm.setlist_id = p_setlist_id AND sm.marker_type = 'break'), 0
        )
    )::integer;
$$ LANGUAGE sql STABLE;
