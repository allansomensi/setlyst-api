-- Platform operations: a support desk, internal staff notes on
-- accounts and incidents for the status page. Platform-wide switches
-- (maintenance mode, sign-ups, blocked e-mail domains) live in
-- `platform_settings` under the key 'platform' and need no table.

-- ---------------------------------------------------------------------
-- Support desk
-- ---------------------------------------------------------------------
--
-- A ticket is a conversation between one account and the staff. Its
-- status says whose turn it is:
--
-- - `open`: waiting for the staff (new, or the requester replied);
-- - `pending`: waiting for the requester (staff replied);
-- - `resolved`: the staff consider it solved; a reply from the requester
--   opens it again;
-- - `closed`: finished for good, no more replies (the requester closed
--   it, or staff did).
--
-- Messages marked `internal` are staff notes, never shown to the
-- requester. Tickets go with the account (LGPD: they hold what the
-- person wrote); staff authors are kept as a snapshot of the username.

CREATE TYPE support_ticket_status AS ENUM ('open', 'pending', 'resolved', 'closed');
CREATE TYPE support_ticket_priority AS ENUM ('low', 'normal', 'high', 'urgent');
CREATE TYPE support_ticket_category AS ENUM ('account', 'billing', 'bug', 'feature', 'content', 'other');

CREATE TABLE support_tickets (
    id UUID PRIMARY KEY,
    -- Short reference shown to people ("#1042").
    number BIGINT GENERATED ALWAYS AS IDENTITY (START WITH 1001) UNIQUE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    subject VARCHAR(150) NOT NULL,
    category support_ticket_category NOT NULL DEFAULT 'other',
    status support_ticket_status NOT NULL DEFAULT 'open',
    priority support_ticket_priority NOT NULL DEFAULT 'normal',
    assignee_id UUID REFERENCES users(id) ON DELETE SET NULL,
    -- Where the requester was and what they used (page, app version,
    -- browser), as sent by the client. Informational only.
    context JSONB NOT NULL DEFAULT '{}',
    -- Satisfaction rating (1 to 5) given once the ticket is resolved or
    -- closed, with an optional comment.
    rating SMALLINT CHECK (rating BETWEEN 1 AND 5),
    rating_comment VARCHAR(500),
    message_count INTEGER NOT NULL DEFAULT 0,
    last_message_at TIMESTAMP NOT NULL,
    -- First public staff reply (response-time metrics).
    first_response_at TIMESTAMP,
    -- The requester hasn't read the latest staff reply yet.
    requester_unread BOOLEAN NOT NULL DEFAULT FALSE,
    resolved_at TIMESTAMP,
    closed_at TIMESTAMP,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL
);

CREATE INDEX idx_support_tickets_user ON support_tickets (user_id, last_message_at DESC);
CREATE INDEX idx_support_tickets_queue ON support_tickets (status, last_message_at DESC);
CREATE INDEX idx_support_tickets_assignee ON support_tickets (assignee_id) WHERE assignee_id IS NOT NULL;

CREATE TABLE support_messages (
    id UUID PRIMARY KEY,
    ticket_id UUID NOT NULL REFERENCES support_tickets(id) ON DELETE CASCADE,
    author_id UUID REFERENCES users(id) ON DELETE SET NULL,
    author_username VARCHAR(30),
    from_staff BOOLEAN NOT NULL,
    internal BOOLEAN NOT NULL DEFAULT FALSE,
    body TEXT NOT NULL,
    created_at TIMESTAMP NOT NULL
);

CREATE INDEX idx_support_messages_ticket ON support_messages (ticket_id, created_at);
CREATE INDEX idx_support_messages_author ON support_messages (author_id) WHERE author_id IS NOT NULL;

ALTER TYPE notification_type ADD VALUE 'support_reply';

-- ---------------------------------------------------------------------
-- Internal staff notes on accounts
-- ---------------------------------------------------------------------
--
-- Context staff leave for each other ("asked for a refund by e-mail on
-- 03/10", "repeat spammer, watch the avatar"). Never shown to the
-- account itself; part of its personal data export and removed with it.

CREATE TABLE user_staff_notes (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    author_id UUID REFERENCES users(id) ON DELETE SET NULL,
    author_username VARCHAR(30),
    body VARCHAR(2000) NOT NULL,
    pinned BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL
);

CREATE INDEX idx_user_staff_notes_user ON user_staff_notes (user_id, pinned DESC, created_at DESC);
CREATE INDEX idx_user_staff_notes_author ON user_staff_notes (author_id) WHERE author_id IS NOT NULL;

-- ---------------------------------------------------------------------
-- Status page incidents
-- ---------------------------------------------------------------------
--
-- Incidents (something is broken) and scheduled maintenance windows,
-- published on the public status page with a timeline of updates.

CREATE TYPE incident_kind AS ENUM ('incident', 'maintenance');
CREATE TYPE incident_impact AS ENUM ('none', 'minor', 'major', 'critical');
CREATE TYPE incident_status AS ENUM ('scheduled', 'investigating', 'identified', 'monitoring', 'resolved');

CREATE TABLE status_incidents (
    id UUID PRIMARY KEY,
    kind incident_kind NOT NULL DEFAULT 'incident',
    title VARCHAR(150) NOT NULL,
    impact incident_impact NOT NULL DEFAULT 'minor',
    status incident_status NOT NULL,
    -- Affected parts of the platform: 'web', 'api', 'sync', 'email',
    -- 'payments', 'exports'.
    components TEXT[] NOT NULL DEFAULT '{}',
    -- Maintenance window (both set for `maintenance`).
    scheduled_for TIMESTAMP,
    scheduled_until TIMESTAMP,
    started_at TIMESTAMP,
    resolved_at TIMESTAMP,
    created_by UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    CONSTRAINT status_incidents_window_check CHECK (
        scheduled_until IS NULL OR scheduled_for IS NULL OR scheduled_until > scheduled_for
    )
);

CREATE INDEX idx_status_incidents_open ON status_incidents (created_at DESC) WHERE status <> 'resolved';
CREATE INDEX idx_status_incidents_resolved ON status_incidents (resolved_at DESC) WHERE status = 'resolved';
CREATE INDEX idx_status_incidents_created_by ON status_incidents (created_by) WHERE created_by IS NOT NULL;

CREATE TABLE status_incident_updates (
    id UUID PRIMARY KEY,
    incident_id UUID NOT NULL REFERENCES status_incidents(id) ON DELETE CASCADE,
    status incident_status NOT NULL,
    body VARCHAR(2000) NOT NULL,
    author_id UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMP NOT NULL
);

CREATE INDEX idx_status_incident_updates_incident ON status_incident_updates (incident_id, created_at DESC);
CREATE INDEX idx_status_incident_updates_author ON status_incident_updates (author_id) WHERE author_id IS NOT NULL;

-- ---------------------------------------------------------------------
-- Staff console lists
-- ---------------------------------------------------------------------

-- The e-mail delivery console lists the outbox newest first, filtered by
-- status.
CREATE INDEX idx_email_outbox_status_created ON email_outbox (status, created_at DESC);
-- The user list sorts and filters by sign-up date and last sign-in.
CREATE INDEX idx_users_created_at ON users (created_at DESC);
