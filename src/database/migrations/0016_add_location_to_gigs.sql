-- Free-text location for a gig (address, city, or a maps link) — distinct
-- from `venue`, which is just the venue's name (e.g. "Bar do Zé"). Both
-- are optional independently: a gig can have a venue name without a
-- pinned-down location yet, or vice versa.
ALTER TABLE gigs ADD COLUMN location TEXT;
