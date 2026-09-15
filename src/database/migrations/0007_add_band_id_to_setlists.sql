ALTER TABLE setlists
    ADD COLUMN band_id UUID REFERENCES bands(id) ON DELETE CASCADE;

CREATE INDEX idx_setlists_band_id ON setlists(band_id);
