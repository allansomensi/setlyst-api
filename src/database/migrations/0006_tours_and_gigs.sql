-- Tours and gigs. Both are personal (`band_id` NULL) or belong to a band,
-- same convention as setlists, and both go to the trash when deleted (see
-- 0004_catalog.sql).

-- A named run of gigs with a start and end date.
CREATE TABLE tours (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    band_id UUID REFERENCES bands(id) ON DELETE CASCADE,
    name VARCHAR(120) NOT NULL,
    description TEXT,
    start_date DATE NOT NULL,
    end_date DATE NOT NULL,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    updated_by UUID REFERENCES users(id) ON DELETE SET NULL,
    deleted_at TIMESTAMP,
    deleted_by UUID REFERENCES users(id) ON DELETE SET NULL,
    trash_batch UUID,
    CONSTRAINT tours_dates_check CHECK (end_date >= start_date)
);

CREATE INDEX idx_tours_user_id ON tours (user_id);
CREATE INDEX idx_tours_band_id ON tours (band_id);
CREATE INDEX idx_tours_deleted ON tours (deleted_at) WHERE deleted_at IS NOT NULL;
CREATE INDEX idx_tours_updated_by ON tours (updated_by) WHERE updated_by IS NOT NULL;
CREATE INDEX idx_tours_deleted_by ON tours (deleted_by) WHERE deleted_by IS NOT NULL;

CREATE TYPE gig_status AS ENUM ('confirmed', 'cancelled', 'completed');

CREATE TABLE gigs (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    band_id UUID REFERENCES bands(id) ON DELETE CASCADE,
    -- A gig can exist without a setlist yet (e.g. booked before the
    -- setlist is put together). Deleting the setlist later should not
    -- erase the gig's own history, so this only detaches it.
    setlist_id UUID REFERENCES setlists(id) ON DELETE SET NULL,
    tour_id UUID REFERENCES tours(id) ON DELETE SET NULL,
    venue VARCHAR(255) NOT NULL,
    -- Free-text location (address, city, or a maps link) — independent of
    -- `venue`, which is just the venue's name (e.g. "Bar do Zé").
    location TEXT,
    scheduled_at TIMESTAMP NOT NULL,
    status gig_status NOT NULL DEFAULT 'confirmed',
    notes TEXT,
    -- Same public-sharing convention as setlists.share_token: NULL means
    -- sharing is off; an unguessable token resolves the gig (and its
    -- linked setlist) through the unauthenticated /public/gigs/{token}
    -- routes. A locked share can't be re-enabled until staff unlocks it.
    share_token VARCHAR(64) UNIQUE,
    share_locked_at TIMESTAMP,
    share_locked_by UUID REFERENCES users(id) ON DELETE SET NULL,
    share_lock_reason VARCHAR(500),
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    updated_by UUID REFERENCES users(id) ON DELETE SET NULL,
    deleted_at TIMESTAMP,
    deleted_by UUID REFERENCES users(id) ON DELETE SET NULL,
    trash_batch UUID
);

CREATE INDEX idx_gigs_user_id ON gigs(user_id);
CREATE INDEX idx_gigs_band_id ON gigs(band_id);
CREATE INDEX idx_gigs_setlist_id ON gigs(setlist_id);
CREATE INDEX idx_gigs_tour_id ON gigs (tour_id) WHERE tour_id IS NOT NULL;
CREATE INDEX idx_gigs_scheduled_at ON gigs(scheduled_at);
CREATE INDEX idx_gigs_share_token ON gigs(share_token);
CREATE INDEX idx_gigs_deleted ON gigs (deleted_at) WHERE deleted_at IS NOT NULL;
CREATE INDEX idx_gigs_updated_by ON gigs (updated_by) WHERE updated_by IS NOT NULL;
CREATE INDEX idx_gigs_deleted_by ON gigs (deleted_by) WHERE deleted_by IS NOT NULL;
CREATE INDEX idx_gigs_share_locked_by ON gigs (share_locked_by) WHERE share_locked_by IS NOT NULL;
