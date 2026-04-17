CREATE TYPE song_tonality AS ENUM (
    'C', 'C#', 'Db', 'D', 'D#', 'Eb', 'E', 'E#', 'F', 'F#', 'Gb', 'G', 'G#', 'Ab', 'A', 'A#', 'Bb', 'B', 'B#',
    'Cm', 'C#m', 'Dbm', 'Dm', 'D#m', 'Ebm', 'Em', 'E#m', 'Fm', 'F#m', 'Gbm', 'Gm', 'G#m', 'Abm', 'Am', 'A#m', 'Bbm', 'Bm', 'B#m'
);

CREATE TYPE song_genre AS ENUM (
    'Acoustic', 'Alternative', 'Axe', 'Blues', 'BossaNova', 'Choro', 'Classical', 'Country', 
    'DeathMetal', 'Disco', 'Electronic', 'Emo', 'Folk', 'Forro', 'Funk', 'Gaucho', 'Gospel', 
    'Grunge', 'HardRock', 'HeavyMetal', 'HipHop', 'House', 'Indie', 'Jazz', 'KPop', 'Latin', 
    'LoFi', 'Metal', 'MPB', 'Pagode', 'Pop', 'PowerMetal', 'ProgressiveRock', 'PsychedelicRock', 
    'Punk', 'Reggae', 'Reggaeton', 'RnB', 'Rock', 'Samba', 'Sertanejo', 'Ska', 'Soul', 
    'SymphonicMetal', 'Techno', 'ThrashMetal', 'Other'
);

CREATE TABLE songs (
    id UUID PRIMARY KEY,
    title VARCHAR(255) NOT NULL,
    artist_id UUID NOT NULL REFERENCES artists(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    tempo INTEGER,
    lyrics TEXT,
    tonality song_tonality,
    genre song_genre,
    duration INTEGER,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    UNIQUE(title, artist_id, user_id)
);

CREATE INDEX idx_songs_user_id ON songs(user_id);