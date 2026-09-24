-- Launch hardening: e-mail outbox priorities, indexes for account deletion
-- and background jobs, and a database guarantee of one owner per band.
--
-- sqlx runs every migration inside a transaction, so the indexes are built
-- without CONCURRENTLY. At launch the tables are small; on a large
-- installation, create them CONCURRENTLY by hand first (the IF NOT EXISTS
-- below then makes this migration a no-op for them).

-- ---------------------------------------------------------------------
-- E-mail outbox
-- ---------------------------------------------------------------------

-- Lower is sent first: 0 = one-time codes and security notices, 3 =
-- account and billing messages, 5 = everything else (in-app notification
-- copies), 9 = bulk (announcements, release notes). A bulk send of
-- thousands of messages must never delay a password-reset code.
ALTER TABLE email_outbox ADD COLUMN IF NOT EXISTS priority SMALLINT NOT NULL DEFAULT 5;

UPDATE email_outbox SET priority = CASE
    WHEN template IN ('email_verification_code', 'password_reset_code', 'email_change_code',
                      'password_changed', 'two_factor_enabled', 'two_factor_disabled',
                      'email_changed_notice', 'account_deleted') THEN 0
    WHEN template IN ('welcome', 'trial_ending', 'subscription_changed') THEN 3
    WHEN template IN ('announcement', 'release_notes') THEN 9
    ELSE 5
END
WHERE status = 'pending';

-- The worker claims due messages by priority, then age.
DROP INDEX IF EXISTS idx_email_outbox_pending;
CREATE INDEX IF NOT EXISTS idx_email_outbox_pending_priority
    ON email_outbox (priority, scheduled_at) WHERE status = 'pending';

-- Stale `sending` locks are recovered on every worker run.
CREATE INDEX IF NOT EXISTS idx_email_outbox_sending
    ON email_outbox (locked_at) WHERE status = 'sending';

-- Per-recipient cap on one-time codes (`outbox::enqueue`).
CREATE INDEX IF NOT EXISTS idx_email_outbox_recipient
    ON email_outbox (LOWER(to_email), template, created_at);

-- Global hourly cap on non-security mail (the worker counts what it sent
-- in the last hour).
CREATE INDEX IF NOT EXISTS idx_email_outbox_sent_bulk
    ON email_outbox (sent_at) WHERE status = 'sent' AND priority > 0;

-- ---------------------------------------------------------------------
-- Foreign keys without an index
-- ---------------------------------------------------------------------
-- Deleting an account sets these to NULL (or cascades); without an index
-- every deletion scans the whole table, inside one transaction. Partial,
-- since the columns are mostly NULL.

CREATE INDEX IF NOT EXISTS idx_audit_logs_impersonator ON audit_logs (impersonator_id) WHERE impersonator_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_users_banned_by ON users (banned_by) WHERE banned_by IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_users_created_by ON users (created_by) WHERE created_by IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_users_updated_by ON users (updated_by) WHERE updated_by IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_songs_updated_by ON songs (updated_by) WHERE updated_by IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_songs_deleted_by ON songs (deleted_by) WHERE deleted_by IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_songs_artist_id ON songs (artist_id);
CREATE INDEX IF NOT EXISTS idx_setlists_updated_by ON setlists (updated_by) WHERE updated_by IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_setlists_deleted_by ON setlists (deleted_by) WHERE deleted_by IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_setlists_share_locked_by ON setlists (share_locked_by) WHERE share_locked_by IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_artists_updated_by ON artists (updated_by) WHERE updated_by IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_artists_deleted_by ON artists (deleted_by) WHERE deleted_by IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_gigs_updated_by ON gigs (updated_by) WHERE updated_by IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_gigs_deleted_by ON gigs (deleted_by) WHERE deleted_by IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_gigs_share_locked_by ON gigs (share_locked_by) WHERE share_locked_by IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_bands_created_by ON bands (created_by) WHERE created_by IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_bands_updated_by ON bands (updated_by) WHERE updated_by IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_tours_updated_by ON tours (updated_by) WHERE updated_by IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_tours_deleted_by ON tours (deleted_by) WHERE deleted_by IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_band_invites_created_by ON band_invites (created_by);
CREATE INDEX IF NOT EXISTS idx_band_notes_author ON band_notes (author_id) WHERE author_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_band_notes_updated_by ON band_notes (updated_by) WHERE updated_by IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_suggestions_suggested_by ON band_song_suggestions (suggested_by, band_id, created_at) WHERE suggested_by IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_suggestions_resolved_by ON band_song_suggestions (resolved_by) WHERE resolved_by IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_suggestions_song ON band_song_suggestions (song_id) WHERE song_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_suggestion_votes_user ON band_song_suggestion_votes (user_id);

CREATE INDEX IF NOT EXISTS idx_moderation_flags_reported_by ON moderation_flags (reported_by) WHERE reported_by IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_moderation_flags_resolved_by ON moderation_flags (resolved_by) WHERE resolved_by IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_moderation_flags_band ON moderation_flags (band_id) WHERE band_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_credit_ledger_actor ON credit_ledger (actor_id) WHERE actor_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_subscription_events_actor ON subscription_events (actor_id) WHERE actor_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_announcements_created_by ON announcements (created_by) WHERE created_by IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_announcements_updated_by ON announcements (updated_by) WHERE updated_by IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_release_notes_created_by ON release_notes (created_by) WHERE created_by IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_release_notes_updated_by ON release_notes (updated_by) WHERE updated_by IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_promo_codes_created_by ON promo_codes (created_by) WHERE created_by IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_promotions_created_by ON promotions (created_by) WHERE created_by IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_plans_updated_by ON plans (updated_by) WHERE updated_by IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_platform_settings_updated_by ON platform_settings (updated_by) WHERE updated_by IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_subscriptions_updated_by ON subscriptions (updated_by) WHERE updated_by IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_user_quotas_updated_by ON user_quotas (updated_by) WHERE updated_by IS NOT NULL;

-- Retention jobs.
CREATE INDEX IF NOT EXISTS idx_notifications_created ON notifications (created_at);
CREATE INDEX IF NOT EXISTS idx_band_invites_inactive ON band_invites (COALESCE(revoked_at, expires_at)) WHERE revoked_at IS NOT NULL OR expires_at IS NOT NULL;

-- ---------------------------------------------------------------------
-- One owner per band
-- ---------------------------------------------------------------------
-- Racing ownership transfers could leave two owners. Should that already
-- have happened, the member who has owned the band longest (earliest to
-- join) stays owner and the others become admins, so the constraint below
-- can be added.
UPDATE band_members bm SET role = 'admin'
WHERE bm.role = 'owner'
  AND EXISTS (
      SELECT 1 FROM band_members other
      WHERE other.band_id = bm.band_id AND other.role = 'owner'
        AND (other.joined_at, other.id) < (bm.joined_at, bm.id)
  );

-- A unique partial index, as an exclusion constraint so it can be checked
-- at commit (DEFERRABLE INITIALLY DEFERRED): handing a band over inside a
-- transaction (account deletion promotes the successor first and removes
-- the old owner's membership afterwards) briefly has two owners. Its
-- backing index is named after the constraint.
ALTER TABLE band_members
    ADD CONSTRAINT idx_band_members_single_owner
    EXCLUDE USING btree (band_id WITH =) WHERE (role = 'owner')
    DEFERRABLE INITIALLY DEFERRED;
