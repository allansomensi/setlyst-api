-- Band collaboration: song suggestions with votes and band reminders.

-- ---------------------------------------------------------------------
-- Song suggestions: a band member proposes a song for one of the band's
-- setlists (the repertoire by default); members vote; someone allowed to
-- manage setlists accepts or rejects it (or it is accepted automatically
-- once it reaches `bands.suggestion_auto_accept_votes`).
-- ---------------------------------------------------------------------

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
CREATE INDEX idx_suggestions_suggested_by ON band_song_suggestions (suggested_by, band_id, created_at) WHERE suggested_by IS NOT NULL;
CREATE INDEX idx_suggestions_resolved_by ON band_song_suggestions (resolved_by) WHERE resolved_by IS NOT NULL;
CREATE INDEX idx_suggestions_song ON band_song_suggestions (song_id) WHERE song_id IS NOT NULL;

CREATE TABLE band_song_suggestion_votes (
    suggestion_id UUID NOT NULL REFERENCES band_song_suggestions(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    value SMALLINT NOT NULL CHECK (value IN (-1, 1)),
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    PRIMARY KEY (suggestion_id, user_id)
);

CREATE INDEX idx_suggestion_votes_user ON band_song_suggestion_votes (user_id);

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
CREATE INDEX idx_band_notes_author ON band_notes (author_id) WHERE author_id IS NOT NULL;
CREATE INDEX idx_band_notes_updated_by ON band_notes (updated_by) WHERE updated_by IS NOT NULL;
