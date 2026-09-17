-- Lets a user mark a setlist or band as a favorite — purely personal,
-- so a band member can favorite a band's setlist without affecting
-- anyone else's view, and favoriting never grants any permission.
CREATE TABLE favorite_setlists (
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    setlist_id UUID NOT NULL REFERENCES setlists(id) ON DELETE CASCADE,
    created_at TIMESTAMP NOT NULL,
    PRIMARY KEY (user_id, setlist_id)
);

CREATE INDEX idx_favorite_setlists_user_id ON favorite_setlists(user_id);

CREATE TABLE favorite_bands (
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    band_id UUID NOT NULL REFERENCES bands(id) ON DELETE CASCADE,
    created_at TIMESTAMP NOT NULL,
    PRIMARY KEY (user_id, band_id)
);

CREATE INDEX idx_favorite_bands_user_id ON favorite_bands(user_id);
