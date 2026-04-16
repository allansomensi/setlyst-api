CREATE TABLE setlists (
    id UUID PRIMARY KEY,
    title VARCHAR(255) NOT NULL,
    description TEXT,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL
);

CREATE TABLE setlist_songs (
    setlist_id UUID REFERENCES setlists(id) ON DELETE CASCADE,
    song_id UUID REFERENCES songs(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    PRIMARY KEY (setlist_id, song_id)
);

CREATE INDEX idx_setlists_user_id ON setlists(user_id);
CREATE INDEX idx_setlist_songs_song_id ON setlist_songs(song_id);