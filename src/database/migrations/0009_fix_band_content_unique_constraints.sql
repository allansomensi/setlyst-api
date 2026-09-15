-- The `band_id` column added in migration 0008 broke the original
-- `UNIQUE(name, user_id)` / `UNIQUE(title, artist_id, user_id)` constraints:
-- forking a song into a band keeps `user_id` as an audit trail (who
-- contributed it), so the very first fork of a song whose artist the
-- contributor already owns personally collided with their own personal
-- artist row — the constraint never accounted for `band_id`, so almost any
-- "add song to a band setlist" failed outright.
--
-- Partial unique indexes replace the blanket constraints: personal rows
-- keep the exact same uniqueness guarantee as before (scoped to
-- `band_id IS NULL`), while band-owned rows are scoped to the band instead
-- of the creator, so they never collide with anyone's personal catalog.
ALTER TABLE artists DROP CONSTRAINT IF EXISTS artists_name_user_id_key;
CREATE UNIQUE INDEX idx_artists_personal_unique ON artists (name, user_id) WHERE band_id IS NULL;
CREATE UNIQUE INDEX idx_artists_band_unique ON artists (band_id, name) WHERE band_id IS NOT NULL;

ALTER TABLE songs DROP CONSTRAINT IF EXISTS songs_title_artist_id_user_id_key;
CREATE UNIQUE INDEX idx_songs_personal_unique ON songs (title, artist_id, user_id) WHERE band_id IS NULL;
