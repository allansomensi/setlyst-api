CREATE TYPE gig_status AS ENUM ('confirmed', 'cancelled', 'completed');

CREATE TABLE gigs (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- NULL means a personal/solo gig, same convention as setlists.band_id.
    band_id UUID REFERENCES bands(id) ON DELETE CASCADE,
    -- A gig can exist without a setlist yet (e.g. booked before the
    -- setlist is put together). Deleting the setlist later should not
    -- erase the gig's own history, so this only detaches it.
    setlist_id UUID REFERENCES setlists(id) ON DELETE SET NULL,
    venue VARCHAR(255) NOT NULL,
    scheduled_at TIMESTAMP NOT NULL,
    status gig_status NOT NULL DEFAULT 'confirmed',
    notes TEXT,
    -- Same public-sharing convention as setlists.share_token: NULL means
    -- sharing is off; an unguessable token resolves the gig (and its
    -- linked setlist) through the unauthenticated /public/gigs/{token}
    -- routes.
    share_token VARCHAR(64) UNIQUE,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL
);

CREATE INDEX idx_gigs_user_id ON gigs(user_id);
CREATE INDEX idx_gigs_band_id ON gigs(band_id);
CREATE INDEX idx_gigs_setlist_id ON gigs(setlist_id);
CREATE INDEX idx_gigs_scheduled_at ON gigs(scheduled_at);
CREATE INDEX idx_gigs_share_token ON gigs(share_token);
