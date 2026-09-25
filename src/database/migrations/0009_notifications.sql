-- Notifications are system-generated, informational events addressed to a
-- single user (never user-authored, unlike everything else in this
-- schema). `data` carries whatever structured context the frontend needs
-- to render the message in the active locale (band name, old/new role,
-- the actor who made the change, etc.) — no human-readable text is
-- stored server-side, so translations stay entirely on the frontend.
CREATE TYPE notification_type AS ENUM (
    -- The recipient's role within a specific band changed (promoted or
    -- demoted). Direction is derived from `old_role`/`new_role` in `data`.
    'band_role_changed',
    -- The recipient was removed from a band by another member.
    -- Never fired when a member leaves voluntarily.
    'band_member_removed',
    -- The recipient's site-wide role changed (e.g. promoted to admin or
    -- moderator, or demoted back to a regular user).
    'platform_role_changed',
    'band_member_added',
    'share_link_revoked',
    'announcement',
    'release_published',
    'band_suggestion_created',
    'band_suggestion_resolved',
    'moderation_action',
    'subscription_changed',
    'trial_ending',
    'credits_granted',
    'security_alert'
);

CREATE TABLE notifications (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    type notification_type NOT NULL,
    -- Structured context for rendering the notification client-side.
    data JSONB NOT NULL DEFAULT '{}',
    read_at TIMESTAMP,
    created_at TIMESTAMP NOT NULL
);

-- Powers both "list mine, newest first" and the unread-count lookup.
CREATE INDEX idx_notifications_user_created ON notifications(user_id, created_at DESC);
CREATE INDEX idx_notifications_user_unread ON notifications(user_id) WHERE read_at IS NULL;
-- Retention job.
CREATE INDEX idx_notifications_created ON notifications (created_at);
