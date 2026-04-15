CREATE TABLE songs (
    id UUID PRIMARY KEY,
    title VARCHAR(255) NOT NULL,
    artist_id UUID NOT NULL REFERENCES artists(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    UNIQUE(title, artist_id, user_id)
);

CREATE INDEX idx_songs_user_id ON songs(user_id);