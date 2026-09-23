-- Content: new song fields (energy, time signature, capo, tuning,
-- performance notes), reference links on songs and setlists, the trash
-- (soft delete), tours, the band repertoire, song suggestions with votes,
-- band reminders and pinned items on the home screen.

-- ---------------------------------------------------------------------
-- Songs: optional performance metadata and reference links
-- ---------------------------------------------------------------------

ALTER TABLE songs
    -- Perceived energy, 1 (very low) to 5 (very high). Drives the setlist
    -- flow analysis.
    ADD COLUMN energy SMALLINT CHECK (energy BETWEEN 1 AND 5),
    -- '4/4', '3/4', '6/8'... validated by the API.
    ADD COLUMN time_signature VARCHAR(8),
    ADD COLUMN capo SMALLINT CHECK (capo BETWEEN 0 AND 11),
    ADD COLUMN tuning VARCHAR(40),
    ADD COLUMN performance_notes TEXT,
    -- [{"url": "https://...", "label": "Studio version"}]. Only links to
    -- known providers are accepted (YouTube, Spotify, Google Drive...).
    ADD COLUMN links JSONB NOT NULL DEFAULT '[]';

ALTER TABLE setlists
    ADD COLUMN links JSONB NOT NULL DEFAULT '[]';

-- ---------------------------------------------------------------------
-- Trash. A deleted song, artist, setlist, gig or tour keeps its row with
-- `deleted_at` set and is hidden everywhere else; it can be restored until
-- it is purged (manually, or automatically after the retention period).
-- `trash_batch` groups rows deleted together (an artist and its songs) so
-- they are restored together.
-- ---------------------------------------------------------------------

ALTER TABLE songs
    ADD COLUMN deleted_at TIMESTAMP,
    ADD COLUMN deleted_by UUID REFERENCES users(id) ON DELETE SET NULL,
    ADD COLUMN trash_batch UUID;

ALTER TABLE artists
    ADD COLUMN deleted_at TIMESTAMP,
    ADD COLUMN deleted_by UUID REFERENCES users(id) ON DELETE SET NULL,
    ADD COLUMN trash_batch UUID;

ALTER TABLE setlists
    ADD COLUMN deleted_at TIMESTAMP,
    ADD COLUMN deleted_by UUID REFERENCES users(id) ON DELETE SET NULL,
    ADD COLUMN trash_batch UUID;

ALTER TABLE gigs
    ADD COLUMN deleted_at TIMESTAMP,
    ADD COLUMN deleted_by UUID REFERENCES users(id) ON DELETE SET NULL,
    ADD COLUMN trash_batch UUID;

CREATE INDEX idx_songs_deleted ON songs (deleted_at) WHERE deleted_at IS NOT NULL;
CREATE INDEX idx_artists_deleted ON artists (deleted_at) WHERE deleted_at IS NOT NULL;
CREATE INDEX idx_setlists_deleted ON setlists (deleted_at) WHERE deleted_at IS NOT NULL;
CREATE INDEX idx_gigs_deleted ON gigs (deleted_at) WHERE deleted_at IS NOT NULL;

-- Uniqueness only applies to live rows: a song in the trash must not
-- block creating a new one with the same title.
DROP INDEX idx_songs_personal_unique;
CREATE UNIQUE INDEX idx_songs_personal_unique ON songs (title, artist_id, user_id)
    WHERE band_id IS NULL AND deleted_at IS NULL;

DROP INDEX idx_songs_band_fork_unique;
CREATE UNIQUE INDEX idx_songs_band_fork_unique ON songs (band_id, forked_from)
    WHERE band_id IS NOT NULL AND forked_from IS NOT NULL AND deleted_at IS NULL;

DROP INDEX idx_artists_personal_unique;
CREATE UNIQUE INDEX idx_artists_personal_unique ON artists (name, user_id)
    WHERE band_id IS NULL AND deleted_at IS NULL;

DROP INDEX idx_artists_band_unique;
CREATE UNIQUE INDEX idx_artists_band_unique ON artists (band_id, name)
    WHERE band_id IS NOT NULL AND deleted_at IS NULL;

-- Songs in the trash no longer count towards a setlist's duration.
CREATE OR REPLACE FUNCTION setlist_total_duration(p_setlist_id UUID) RETURNS INTEGER AS $$
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

-- ---------------------------------------------------------------------
-- Tours: a named run of gigs with a start and end date.
-- ---------------------------------------------------------------------

CREATE TABLE tours (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- NULL = personal tour, same convention as setlists and gigs.
    band_id UUID REFERENCES bands(id) ON DELETE CASCADE,
    name VARCHAR(120) NOT NULL,
    description TEXT,
    start_date DATE NOT NULL,
    end_date DATE NOT NULL,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    updated_by UUID REFERENCES users(id) ON DELETE SET NULL,
    deleted_at TIMESTAMP,
    deleted_by UUID REFERENCES users(id) ON DELETE SET NULL,
    trash_batch UUID,
    CONSTRAINT tours_dates_check CHECK (end_date >= start_date)
);

CREATE INDEX idx_tours_user_id ON tours (user_id);
CREATE INDEX idx_tours_band_id ON tours (band_id);
CREATE INDEX idx_tours_deleted ON tours (deleted_at) WHERE deleted_at IS NOT NULL;

ALTER TABLE gigs
    ADD COLUMN tour_id UUID REFERENCES tours(id) ON DELETE SET NULL;

CREATE INDEX idx_gigs_tour_id ON gigs (tour_id) WHERE tour_id IS NOT NULL;

-- ---------------------------------------------------------------------
-- Band repertoire: one special setlist per band that collects every song
-- the band plays. Songs added to any setlist of the band are added to it
-- automatically. It can't be deleted or renamed.
-- ---------------------------------------------------------------------

ALTER TABLE setlists
    ADD COLUMN is_repertoire BOOLEAN NOT NULL DEFAULT FALSE;

CREATE UNIQUE INDEX idx_setlists_band_repertoire ON setlists (band_id) WHERE is_repertoire;

-- Create the repertoire of every existing band and fill it with the
-- band's songs, alphabetically.
WITH band_owner AS (
    SELECT b.id AS band_id,
           COALESCE(
               (SELECT bm.user_id FROM band_members bm
                WHERE bm.band_id = b.id AND bm.role = 'owner' LIMIT 1),
               (SELECT bm.user_id FROM band_members bm
                WHERE bm.band_id = b.id ORDER BY bm.role DESC, bm.joined_at ASC LIMIT 1)
           ) AS owner_id
    FROM bands b
)
INSERT INTO setlists (id, title, description, user_id, band_id, is_repertoire, created_at, updated_at)
SELECT gen_random_uuid(), 'Repertoire', NULL, bo.owner_id, bo.band_id, TRUE,
       NOW() AT TIME ZONE 'utc', NOW() AT TIME ZONE 'utc'
FROM band_owner bo
WHERE bo.owner_id IS NOT NULL;

INSERT INTO setlist_songs (setlist_id, song_id, position)
SELECT r.id, s.id, ROW_NUMBER() OVER (PARTITION BY r.id ORDER BY LOWER(s.title), s.id)
FROM setlists r
JOIN songs s ON s.band_id = r.band_id
WHERE r.is_repertoire;

-- ---------------------------------------------------------------------
-- Song suggestions: a band member proposes a song for one of the band's
-- setlists (the repertoire by default); members vote; someone allowed to
-- manage setlists accepts or rejects it (or it is accepted automatically
-- once it reaches the band's vote threshold).
-- ---------------------------------------------------------------------

ALTER TABLE bands
    -- NULL = suggestions are never accepted automatically.
    ADD COLUMN suggestion_auto_accept_votes INTEGER
        CHECK (suggestion_auto_accept_votes IS NULL OR suggestion_auto_accept_votes BETWEEN 1 AND 100);

CREATE TYPE suggestion_status AS ENUM ('open', 'accepted', 'rejected', 'withdrawn');

CREATE TABLE band_song_suggestions (
    id UUID PRIMARY KEY,
    band_id UUID NOT NULL REFERENCES bands(id) ON DELETE CASCADE,
    -- Target setlist (a setlist of the same band).
    setlist_id UUID NOT NULL REFERENCES setlists(id) ON DELETE CASCADE,
    -- The suggester's personal song, or a copy the band already owns.
    -- NULL once that song is permanently deleted.
    song_id UUID REFERENCES songs(id) ON DELETE SET NULL,
    -- Snapshot for display, so the suggestion stays readable.
    song_title VARCHAR(255) NOT NULL,
    artist_name VARCHAR(255) NOT NULL,
    suggested_by UUID REFERENCES users(id) ON DELETE SET NULL,
    note VARCHAR(500),
    status suggestion_status NOT NULL DEFAULT 'open',
    resolved_by UUID REFERENCES users(id) ON DELETE SET NULL,
    resolved_at TIMESTAMP,
    resolution_note VARCHAR(500),
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL
);

CREATE INDEX idx_band_song_suggestions_band ON band_song_suggestions (band_id, status, created_at DESC);
CREATE UNIQUE INDEX idx_band_song_suggestions_open
    ON band_song_suggestions (setlist_id, song_id)
    WHERE status = 'open' AND song_id IS NOT NULL;

CREATE TABLE band_song_suggestion_votes (
    suggestion_id UUID NOT NULL REFERENCES band_song_suggestions(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    value SMALLINT NOT NULL CHECK (value IN (-1, 1)),
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    PRIMARY KEY (suggestion_id, user_id)
);

-- ---------------------------------------------------------------------
-- Band reminders: short notes shown on the band page.
-- ---------------------------------------------------------------------

CREATE TYPE band_note_color AS ENUM ('default', 'yellow', 'green', 'blue', 'red', 'purple');

CREATE TABLE band_notes (
    id UUID PRIMARY KEY,
    band_id UUID NOT NULL REFERENCES bands(id) ON DELETE CASCADE,
    author_id UUID REFERENCES users(id) ON DELETE SET NULL,
    content VARCHAR(2000) NOT NULL,
    color band_note_color NOT NULL DEFAULT 'default',
    is_pinned BOOLEAN NOT NULL DEFAULT FALSE,
    -- Optional date the reminder refers to (rehearsal, deadline...).
    due_at TIMESTAMP,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    updated_by UUID REFERENCES users(id) ON DELETE SET NULL
);

CREATE INDEX idx_band_notes_band ON band_notes (band_id, is_pinned DESC, created_at DESC);

-- ---------------------------------------------------------------------
-- Items pinned to the home screen. Purely personal.
-- ---------------------------------------------------------------------

CREATE TYPE pin_item_type AS ENUM ('setlist', 'band', 'song', 'tour', 'gig');

CREATE TABLE user_pins (
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    item_type pin_item_type NOT NULL,
    item_id UUID NOT NULL,
    position INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMP NOT NULL,
    PRIMARY KEY (user_id, item_type, item_id)
);

CREATE INDEX idx_user_pins_item ON user_pins (item_type, item_id);
