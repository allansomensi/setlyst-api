-- Nullable: NULL means public sharing is off. When set, this token resolves
-- the setlist through the unauthenticated /public/setlists/{token} routes.
-- Long and random by construction (see utils::share_token) — unguessable is
-- the entire security model here, there's no other access check on the
-- public routes.
ALTER TABLE setlists ADD COLUMN share_token VARCHAR(64) UNIQUE;
CREATE INDEX idx_setlists_share_token ON setlists(share_token);
