-- The song catalog: artists, songs and song tags, either personal or owned
-- by a band.
--
-- Trash: a deleted artist or song (like a setlist, gig or tour) keeps its
-- row with `deleted_at` set and is hidden everywhere else; it can be
-- restored until it is purged (manually, or automatically after the
-- retention period). `trash_batch` groups rows deleted together (an artist
-- and its songs) so they are restored together.

-- ---------------------------------------------------------------------
-- Artists
-- ---------------------------------------------------------------------

CREATE TABLE artists (
    id UUID PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- The band that owns this artist as an independent, shared copy, or
    -- NULL for a personal artist. Lets a band own its own copy of an
    -- artist, decoupled from any single member's personal catalog —
    -- otherwise a band setlist would depend on that member's account
    -- (deleting the artist, or the account, would break every band
    -- setlist that used it).
    band_id UUID REFERENCES bands(id) ON DELETE CASCADE,
    -- The personal artist this band-owned copy was resolved from, if any
    -- — NULL for a personal artist, or for a band artist created fresh
    -- rather than forked from an existing one. `user_id` above is kept as
    -- an audit trail ("who forked this into the band"), not as the
    -- access-control owner for band-owned rows.
    forked_from UUID REFERENCES artists(id) ON DELETE SET NULL,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    -- Who changed the record last. `user_id` already records the creator.
    updated_by UUID REFERENCES users(id) ON DELETE SET NULL,
    deleted_at TIMESTAMP,
    deleted_by UUID REFERENCES users(id) ON DELETE SET NULL,
    trash_batch UUID
);

CREATE INDEX idx_artists_user_id ON artists(user_id);
CREATE INDEX idx_artists_band_id ON artists(band_id);
CREATE INDEX idx_artists_forked_from ON artists(forked_from) WHERE forked_from IS NOT NULL;
CREATE INDEX idx_artists_deleted ON artists (deleted_at) WHERE deleted_at IS NOT NULL;
CREATE INDEX idx_artists_updated_by ON artists (updated_by) WHERE updated_by IS NOT NULL;
CREATE INDEX idx_artists_deleted_by ON artists (deleted_by) WHERE deleted_by IS NOT NULL;

-- Personal rows are unique per (name, user_id); band-owned rows are
-- unique per (band_id, name) instead, so they never collide with anyone's
-- personal catalog. Uniqueness only applies to live rows: an artist in
-- the trash must not block creating a new one with the same name.
CREATE UNIQUE INDEX idx_artists_personal_unique ON artists (name, user_id)
    WHERE band_id IS NULL AND deleted_at IS NULL;
CREATE UNIQUE INDEX idx_artists_band_unique ON artists (band_id, name)
    WHERE band_id IS NOT NULL AND deleted_at IS NULL;

-- ---------------------------------------------------------------------
-- Songs
-- ---------------------------------------------------------------------

CREATE TYPE song_tonality AS ENUM (
    'C', 'C#', 'Db', 'D', 'D#', 'Eb', 'E', 'E#', 'F', 'F#', 'Gb', 'G', 'G#', 'Ab', 'A', 'A#', 'Bb', 'B', 'B#',
    'Cm', 'C#m', 'Dbm', 'Dm', 'D#m', 'Ebm', 'Em', 'E#m', 'Fm', 'F#m', 'Gbm', 'Gm', 'G#m', 'Abm', 'Am', 'A#m', 'Bbm', 'Bm', 'B#m'
);

-- Broad categories plus the rock/metal/electronic/regional subgenres, so
-- users aren't stuck picking "Rock" or "Other" for everything that isn't
-- a broad category.
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
    -- Perceived energy, 1 (very low) to 5 (very high). Drives the setlist
    -- flow analysis.
    energy SMALLINT CHECK (energy BETWEEN 1 AND 5),
    -- '4/4', '3/4', '6/8'... validated by the API.
    time_signature VARCHAR(8),
    capo SMALLINT CHECK (capo BETWEEN 0 AND 11),
    tuning VARCHAR(40),
    performance_notes TEXT,
    -- [{"url": "https://...", "label": "Studio version"}]. Only links to
    -- known providers are accepted (YouTube, Spotify, Google Drive...).
    links JSONB NOT NULL DEFAULT '[]',
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    updated_by UUID REFERENCES users(id) ON DELETE SET NULL,
    deleted_at TIMESTAMP,
    deleted_by UUID REFERENCES users(id) ON DELETE SET NULL,
    trash_batch UUID
);

CREATE INDEX idx_songs_user_id ON songs(user_id);
CREATE INDEX idx_songs_band_id ON songs(band_id);
CREATE INDEX idx_songs_artist_id ON songs (artist_id);
CREATE INDEX idx_songs_forked_from ON songs(forked_from);
CREATE INDEX idx_songs_deleted ON songs (deleted_at) WHERE deleted_at IS NOT NULL;
CREATE INDEX idx_songs_updated_by ON songs (updated_by) WHERE updated_by IS NOT NULL;
CREATE INDEX idx_songs_deleted_by ON songs (deleted_by) WHERE deleted_by IS NOT NULL;

-- Personal rows are unique per (title, artist_id, user_id). Band-owned
-- rows aren't constrained by title — they're kept unique per fork source
-- instead (below), since a band can legitimately end up with two
-- differently-forked songs that happen to share a title. As with artists,
-- only live rows count.
CREATE UNIQUE INDEX idx_songs_personal_unique ON songs (title, artist_id, user_id)
    WHERE band_id IS NULL AND deleted_at IS NULL;

-- Makes "reuse the existing fork instead of creating a new one" atomic
-- and race-safe: two near-simultaneous forks of the same source song
-- into the same band resolve to a single row instead of two.
CREATE UNIQUE INDEX idx_songs_band_fork_unique ON songs (band_id, forked_from)
    WHERE band_id IS NOT NULL AND forked_from IS NOT NULL AND deleted_at IS NULL;

-- Free-form tags, normalized (lowercase, trimmed) by the API.
CREATE TABLE song_tags (
    song_id UUID NOT NULL REFERENCES songs(id) ON DELETE CASCADE,
    tag VARCHAR(30) NOT NULL,
    created_at TIMESTAMP NOT NULL DEFAULT (NOW() AT TIME ZONE 'utc'),
    PRIMARY KEY (song_id, tag)
);

CREATE INDEX idx_song_tags_tag ON song_tags(tag);
