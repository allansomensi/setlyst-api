-- Beta and plan limits for launch.
--
-- While `platform_settings.billing.enforced` is off (the beta), every
-- verified account has every feature within the platform defaults. Once
-- it's on, accounts without a plan fall to the free tier (one band, no
-- tours, no PDF or report exports) and accounts that haven't verified
-- their e-mail get very small limits either way (both tiers are built in:
-- `QuotaLimits::FREE`, `QuotaLimits::UNVERIFIED`).
--
-- Limits are sized for real use on modest infrastructure: a solo working
-- musician rarely keeps more than a few hundred songs, a busy one plays
-- 100-150 gigs a year, and a long night is ~60 songs.

-- PDF export is now a plan feature of its own (advanced options still
-- need `advanced_pdf`); every paid plan includes it. The Basic plan can
-- now create one band, as the free tier does.
UPDATE plans
SET limits = '{"songs": 250, "artists": 150, "setlists": 30, "gigs": 100, "tags": 30, "bands_owned": 1, "band_memberships": 3, "band_members": 6, "band_setlists": 30, "band_gigs": 100, "band_songs": 300, "setlist_items": 60, "tours": 0, "band_tours": 0}',
    features = features || '{"pdf_export": true, "create_bands": true}',
    updated_at = NOW() AT TIME ZONE 'utc'
WHERE code = 'basic';

UPDATE plans
SET limits = '{"songs": 800, "artists": 400, "setlists": 100, "gigs": 300, "tags": 60, "bands_owned": 2, "band_memberships": 8, "band_members": 10, "band_setlists": 100, "band_gigs": 300, "band_songs": 800, "setlist_items": 100, "tours": 10, "band_tours": 10}',
    features = features || '{"pdf_export": true}',
    updated_at = NOW() AT TIME ZONE 'utc'
WHERE code = 'intermediate';

UPDATE plans
SET limits = '{"songs": 2000, "artists": 1000, "setlists": 300, "gigs": 1000, "tags": 150, "bands_owned": 5, "band_memberships": 20, "band_members": 25, "band_setlists": 300, "band_gigs": 1000, "band_songs": 2500, "setlist_items": 150, "tours": 30, "band_tours": 30}',
    features = features || '{"pdf_export": true}',
    updated_at = NOW() AT TIME ZONE 'utc'
WHERE code = 'pro';

-- Any other plan created by staff keeps its limits but keeps exporting
-- PDFs as before.
UPDATE plans
SET features = features || '{"pdf_export": true}'
WHERE code NOT IN ('basic', 'intermediate', 'pro')
  AND NOT features ? 'pdf_export';

-- The platform defaults (the beta limits) go back to the new built-in
-- values; staff can still change them in the admin panel.
DELETE FROM platform_settings WHERE key = 'quota_defaults';
