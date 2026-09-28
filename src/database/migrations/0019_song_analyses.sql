-- Manual harmonic analysis of a song.

-- ---------------------------------------------------------------------
-- Song analyses
-- ---------------------------------------------------------------------
--
-- A musician can write down how a song works harmonically: the degree of
-- each chord (roman numerals), arrows for its resolutions, modal
-- borrowings, notes. The web app owns the document's shape; the API
-- keeps it as an opaque JSON object (at most 256 KiB, nested at most 8
-- levels deep, checked by the API), one per song.
--
-- It belongs to the song: whoever can see the song can read it, whoever
-- can manage the song can change it, and it goes with the song when the
-- song is purged. It isn't copied into a song's versions or a band's
-- copies.
--
-- `updated_at` doubles as the version the client edited: a save that
-- names another one is refused (409 `ANALYSIS_CONFLICT`), so two people
-- editing at once don't overwrite each other unknowingly.
--
-- `updated_by` is `SET NULL` when that account is deleted: the analysis
-- stays with the song.
CREATE TABLE song_analyses (
    song_id UUID PRIMARY KEY REFERENCES songs(id) ON DELETE CASCADE,
    content JSONB NOT NULL,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    updated_by UUID REFERENCES users(id) ON DELETE SET NULL
);

-- Accounts being deleted clear their `updated_by`.
CREATE INDEX idx_song_analyses_updated_by ON song_analyses (updated_by)
    WHERE updated_by IS NOT NULL;
