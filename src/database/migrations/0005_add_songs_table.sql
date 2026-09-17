CREATE TYPE song_tonality AS ENUM (
    'C', 'C#', 'Db', 'D', 'D#', 'Eb', 'E', 'E#', 'F', 'F#', 'Gb', 'G', 'G#', 'Ab', 'A', 'A#', 'Bb', 'B', 'B#',
    'Cm', 'C#m', 'Dbm', 'Dm', 'D#m', 'Ebm', 'Em', 'E#m', 'Fm', 'F#m', 'Gbm', 'Gm', 'G#m', 'Abm', 'Am', 'A#m', 'Bbm', 'Bm', 'B#m'
);

-- Full genre list, including the expanded rock/metal/electronic/regional
-- subgenres added after the initial release, so users aren't stuck
-- picking "Rock" or "Other" for everything that isn't a broad category.
CREATE TYPE song_genre AS ENUM (
    'Acoustic', 'Alternative', 'Axe', 'Blues', 'BossaNova', 'Choro', 'Classical', 'Country',
    'DeathMetal', 'Disco', 'Electronic', 'Emo', 'Folk', 'Forro', 'Funk', 'Gaucho', 'Gospel',
    'Grunge', 'HardRock', 'HeavyMetal', 'HipHop', 'House', 'Indie', 'Jazz', 'KPop', 'Latin',
    'LoFi', 'Metal', 'MPB', 'Pagode', 'Pop', 'PowerMetal', 'ProgressiveRock', 'PsychedelicRock',
    'Punk', 'Reggae', 'Reggaeton', 'RnB', 'Rock', 'Samba', 'Sertanejo', 'Ska', 'Soul',
    'SymphonicMetal', 'Techno', 'ThrashMetal',
    'SoftRock', 'ClassicRock', 'PopRock', 'PowerBallad', 'FolkRock', 'ArenaRock', 'GarageRock',
    'IndieRock', 'PostRock', 'SurfRock', 'GlamRock', 'StonerRock', 'SouthernRock', 'BluesRock',
    'RockAndRoll', 'AlternativeRock', 'IndustrialRock', 'NuMetal', 'BlackMetal', 'DoomMetal',
    'GrooveMetal', 'Metalcore', 'Deathcore', 'Grindcore', 'IndustrialMetal', 'GothicMetal',
    'FolkMetal', 'PostPunk', 'PopPunk', 'SkaPunk', 'HardcorePunk', 'NewWave', 'Dance', 'EDM',
    'DrumAndBass', 'Dubstep', 'Trance', 'Ambient', 'Chillout', 'Synthpop', 'Industrial', 'Trap',
    'Drill', 'Afrobeat', 'Grime', 'FunkCarioca', 'Piseiro', 'Brega', 'Frevo', 'Arrocha',
    'WorldMusic', 'Flamenco', 'Tango', 'Fado',
    'Other'
);

CREATE TABLE songs (
    id UUID PRIMARY KEY,
    title VARCHAR(255) NOT NULL,
    artist_id UUID NOT NULL REFERENCES artists(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- The band that owns this song as an independent, shared copy, or
    -- NULL for a personal song. Same rationale as `artists.band_id`.
    band_id UUID REFERENCES bands(id) ON DELETE CASCADE,
    -- The personal song this band-owned copy was forked from, if any.
    -- `ON DELETE SET NULL`, not CASCADE: if the original personal song is
    -- later deleted, the band's copy must keep existing — that's the
    -- entire point of forking. Losing the provenance link is fine;
    -- losing the content is not.
    forked_from UUID REFERENCES songs(id) ON DELETE SET NULL,
    tempo INTEGER,
    lyrics TEXT,
    tonality song_tonality,
    genre song_genre,
    duration INTEGER,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL
);

CREATE INDEX idx_songs_user_id ON songs(user_id);
CREATE INDEX idx_songs_band_id ON songs(band_id);
CREATE INDEX idx_songs_forked_from ON songs(forked_from);

-- Personal rows are unique per (title, artist_id, user_id). Band-owned
-- rows aren't constrained by title — they're kept unique per fork source
-- instead (below), since a band can legitimately end up with two
-- differently-forked songs that happen to share a title.
CREATE UNIQUE INDEX idx_songs_personal_unique ON songs (title, artist_id, user_id) WHERE band_id IS NULL;

-- Makes "reuse the existing fork instead of creating a new one" atomic
-- and race-safe: two near-simultaneous forks of the same source song
-- into the same band resolve to a single row instead of two.
CREATE UNIQUE INDEX idx_songs_band_fork_unique ON songs (band_id, forked_from)
    WHERE band_id IS NOT NULL AND forked_from IS NOT NULL;
