-- Keeping a band's copy of a song in step with the personal song it was
-- copied from.
--
-- A band copy (`songs.band_id` set, `forked_from` pointing at a member's
-- personal song) is independent on purpose: editing or deleting the
-- original never changes the band's copy behind its back. The member who
-- contributed it can still pull their latest version into the band on
-- request, and adding the song to a band setlist again brings an
-- untouched copy up to date on its own.
--
-- `source_synced_at` is when the copy last matched its original (when it
-- was copied, or last updated from it). The original has changes the copy
-- lacks when its `updated_at` is later; the band edited its copy since
-- then when the copy's own `updated_at` is later. NULL for personal songs.

ALTER TABLE songs ADD COLUMN source_synced_at TIMESTAMP;

UPDATE songs SET source_synced_at = created_at
WHERE band_id IS NOT NULL AND forked_from IS NOT NULL;
