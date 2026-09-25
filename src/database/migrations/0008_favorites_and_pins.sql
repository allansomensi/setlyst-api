-- Favorites and items pinned to the home screen. Purely personal: a band
-- member can favorite or pin a band's setlist without affecting anyone
-- else's view, and neither ever grants any permission.

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

CREATE TYPE pin_item_type AS ENUM ('setlist', 'band', 'song', 'tour', 'gig');

CREATE TABLE user_pins (
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    item_type pin_item_type NOT NULL,
    item_id UUID NOT NULL,
    position INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMP NOT NULL,
    PRIMARY KEY (user_id, item_type, item_id)
);

CREATE INDEX idx_user_pins_item ON user_pins (item_type, item_id);
