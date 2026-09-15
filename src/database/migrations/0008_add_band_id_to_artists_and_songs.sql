-- Lets a band own an independent copy of an artist/song, decoupled from any
-- single member's personal catalog. Without this, a band setlist would rely
-- on `ON DELETE CASCADE` chains rooted in one member's `artists`/`songs`
-- rows: if that member deletes the song (or their account), it silently
-- disappears from every band setlist that used it too.
--
-- `user_id` on these rows is kept as an audit trail ("who forked this into
-- the band"), not as the access-control owner — same convention already
-- used by `setlists.band_id`. Management permission for band-owned rows is
-- decided by the caller's band role, not by `user_id` matching.
ALTER TABLE artists ADD COLUMN band_id UUID REFERENCES bands(id) ON DELETE CASCADE;
ALTER TABLE songs ADD COLUMN band_id UUID REFERENCES bands(id) ON DELETE CASCADE;

CREATE INDEX idx_artists_band_id ON artists(band_id);
CREATE INDEX idx_songs_band_id ON songs(band_id);
