-- Expands the song_genre enum with a much larger set of subgenres (rock
-- family in particular, as requested), so users aren't stuck picking
-- "Rock" or "Other" for everything that isn't already covered.
--
-- ALTER TYPE ... ADD VALUE cannot run in the same transaction as a
-- statement that *uses* the new value, but simply adding values (as we do
-- here) is safe inside the migration's own transaction on PostgreSQL 12+.
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'SoftRock';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'ClassicRock';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'PopRock';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'PowerBallad';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'FolkRock';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'ArenaRock';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'GarageRock';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'IndieRock';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'PostRock';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'SurfRock';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'GlamRock';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'StonerRock';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'SouthernRock';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'BluesRock';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'RockAndRoll';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'AlternativeRock';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'IndustrialRock';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'NuMetal';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'BlackMetal';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'DoomMetal';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'GrooveMetal';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'Metalcore';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'Deathcore';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'Grindcore';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'IndustrialMetal';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'GothicMetal';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'FolkMetal';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'PostPunk';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'PopPunk';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'SkaPunk';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'HardcorePunk';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'NewWave';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'Dance';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'EDM';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'DrumAndBass';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'Dubstep';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'Trance';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'Ambient';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'Chillout';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'Synthpop';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'Industrial';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'Trap';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'Drill';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'Afrobeat';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'Grime';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'FunkCarioca';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'Piseiro';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'Brega';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'Frevo';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'Arrocha';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'WorldMusic';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'Flamenco';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'Tango';
ALTER TYPE song_genre ADD VALUE IF NOT EXISTS 'Fado';

-- Setlist "markers": visual, non-song entries that live alongside songs in
-- a setlist's running order — either a named block/section header (e.g.
-- "Bloco Baladas") or a break/pause slot (optional label + duration in
-- minutes). Deliberately a separate table rather than columns on
-- `setlist_songs`, since a marker isn't a song and has no `song_id`.
--
-- `position` shares the same per-setlist ordering space as
-- `setlist_songs.position`: relative order across both tables is all that
-- matters (gaps are expected and fine), so existing queries that sort
-- `setlist_songs` by position are unaffected.
CREATE TYPE setlist_marker_type AS ENUM ('block', 'break');

CREATE TABLE setlist_markers (
    id UUID PRIMARY KEY,
    setlist_id UUID NOT NULL REFERENCES setlists(id) ON DELETE CASCADE,
    marker_type setlist_marker_type NOT NULL,
    -- Block name (required for 'block' rows) or break label (optional,
    -- frontend falls back to a translated "Break" placeholder when null).
    label VARCHAR(255),
    -- Only meaningful for 'break' rows.
    duration_minutes INTEGER,
    position INTEGER NOT NULL,
    created_at TIMESTAMP NOT NULL
);

CREATE INDEX idx_setlist_markers_setlist_id ON setlist_markers(setlist_id);

-- Centralizes the "total duration" calculation (songs + break minutes) so
-- every place that reads a setlist (personal list, band list, single
-- lookup, public share lookup) reports the same number, now that breaks
-- also count toward it.
CREATE FUNCTION setlist_total_duration(p_setlist_id UUID) RETURNS INTEGER AS $$
    SELECT (
        COALESCE(
            (SELECT SUM(so.duration) FROM setlist_songs ss
             JOIN songs so ON so.id = ss.song_id
             WHERE ss.setlist_id = p_setlist_id), 0
        )
        + COALESCE(
            (SELECT SUM(sm.duration_minutes) * 60 FROM setlist_markers sm
             WHERE sm.setlist_id = p_setlist_id AND sm.marker_type = 'break'), 0
        )
    )::integer;
$$ LANGUAGE sql STABLE;
