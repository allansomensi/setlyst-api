-- Indexes the reads and cascades that had none, and the duplicates that
-- only cost writes.

-- The trash groups rows deleted together by `trash_batch`; listing,
-- restoring and purging the trash look rows up by it (six statements in
-- the trash repository), and only `deleted_at` was indexed, so every probe
-- walked every trashed row of the platform.
CREATE INDEX idx_songs_trash_batch ON songs (trash_batch) WHERE trash_batch IS NOT NULL;
CREATE INDEX idx_artists_trash_batch ON artists (trash_batch) WHERE trash_batch IS NOT NULL;

-- Foreign keys whose cascades (a setlist or band deleted, a purge, an
-- account deletion) scanned the whole table for the rows to remove.
CREATE INDEX idx_favorite_setlists_setlist_id ON favorite_setlists (setlist_id);
CREATE INDEX idx_favorite_bands_band_id ON favorite_bands (band_id);
CREATE INDEX idx_band_song_suggestions_setlist ON band_song_suggestions (setlist_id);
CREATE INDEX idx_checkout_sessions_user ON checkout_sessions (user_id);

-- The audit retention job strips access data (IP addresses, identifiers
-- typed in failed sign-ins) from entries older than six months. Only the
-- entries still carrying it are of interest, which this partial index
-- finds directly instead of re-reading every old entry four times a day.
CREATE INDEX idx_audit_logs_access_pending ON audit_logs (created_at)
    WHERE ip_address IS NOT NULL
       OR (action = 'user.login_failed' AND target_id IS NULL AND target_label IS NOT NULL);

-- Notifications are paged by (created_at, id) now that ties are broken
-- by id (a fan-out gives thousands of rows the same timestamp); the
-- index matches the order.
DROP INDEX IF EXISTS idx_notifications_user_created;
CREATE INDEX idx_notifications_user_created_id ON notifications (user_id, created_at DESC, id DESC);

-- Duplicates of the UNIQUE constraints on the same columns: every insert
-- and update maintained both.
DROP INDEX IF EXISTS idx_setlists_share_token;
DROP INDEX IF EXISTS idx_gigs_share_token;
DROP INDEX IF EXISTS idx_band_invites_code;
