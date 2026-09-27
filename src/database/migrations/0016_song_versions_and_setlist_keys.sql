-- Song versions and per-setlist keys.

-- ---------------------------------------------------------------------
-- Song versions
-- ---------------------------------------------------------------------
--
-- A musician often keeps more than one chart of the same song: a
-- simplified one, an acoustic arrangement, the band's version... Each
-- version is an ordinary song (its own lyrics, key, tempo, tags), so it
-- can be added to setlists, exported and shared like any other. What ties
-- them together is `version_of`: the original song of the family (always
-- the root, never another version), and `version_label` names the version
-- ("Simplified", "Acoustic").
--
-- `ON DELETE SET NULL`: purging the original leaves its versions as
-- standalone songs rather than deleting someone's charts with it.

ALTER TABLE songs ADD COLUMN version_label VARCHAR(60);
ALTER TABLE songs ADD COLUMN version_of UUID REFERENCES songs(id) ON DELETE SET NULL;

CREATE INDEX idx_songs_version_of ON songs (version_of) WHERE version_of IS NOT NULL;

-- A version shares the original's title and artist, so personal songs are
-- now unique per (title, artist, version label) instead.
DROP INDEX idx_songs_personal_unique;
CREATE UNIQUE INDEX idx_songs_personal_unique
    ON songs (title, artist_id, user_id, COALESCE(version_label, ''))
    WHERE band_id IS NULL AND deleted_at IS NULL;

-- ---------------------------------------------------------------------
-- Per-setlist keys
-- ---------------------------------------------------------------------
--
-- The key a song is played in *in this setlist*, as semitones away from
-- the song's written key: a singer who takes a song two semitones down
-- gets it that way every time the setlist is opened, without touching the
-- song itself (or its key in any other setlist). 0 = as written. An octave
-- either way is the whole useful range.

ALTER TABLE setlist_songs ADD COLUMN transpose SMALLINT NOT NULL DEFAULT 0
    CHECK (transpose BETWEEN -11 AND 11);
