-- Mirrors `songs.forked_from`: when a personal artist is resolved into a
-- band-owned copy (via find_or_create_for_band), this records which
-- personal artist it was forked from. Lets platform-wide admin metrics
-- distinguish "genuinely new content" from "an independent copy that
-- exists only so band setlists don't depend on one member's account" —
-- without it, forking 30 songs into a band inflates the admin's global
-- artist/song totals by counting the same creative content twice.
ALTER TABLE artists ADD COLUMN forked_from UUID REFERENCES artists(id) ON DELETE SET NULL;

CREATE INDEX idx_artists_forked_from ON artists(forked_from) WHERE forked_from IS NOT NULL;
