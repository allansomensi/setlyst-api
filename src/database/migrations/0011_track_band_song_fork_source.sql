-- Tracks which personal song a band-owned copy was forked from (see
-- Song::fork_for_band). Without this, re-adding the same personal song to a
-- band setlist had no way to recognize "this was already forked into this
-- band" and created a brand new copy — with a brand new ID — every single
-- time, which let the same song appear in a setlist any number of times
-- (each "duplicate" was technically a distinct row).
--
-- `ON DELETE SET NULL`, not CASCADE: if the original personal song is later
-- deleted, the band's copy must keep existing — that's the entire point of
-- forking. Losing the provenance link is fine; losing the content is not.
ALTER TABLE songs ADD COLUMN forked_from UUID REFERENCES songs(id) ON DELETE SET NULL;
CREATE INDEX idx_songs_forked_from ON songs(forked_from);

-- Makes "reuse the existing fork instead of creating a new one" atomic and
-- race-safe: two near-simultaneous forks of the same source song into the
-- same band resolve to a single row instead of two.
CREATE UNIQUE INDEX idx_songs_band_fork_unique ON songs (band_id, forked_from)
    WHERE band_id IS NOT NULL AND forked_from IS NOT NULL;
