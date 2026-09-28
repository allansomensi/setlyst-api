-- Setlist collaboration: inviting other accounts to a personal setlist,
-- and recording who added each song.

-- ---------------------------------------------------------------------
-- Collaborators
-- ---------------------------------------------------------------------
--
-- A personal setlist can be shared with other accounts without creating
-- a band — a one-off show with a guest singer, say. The owner (and any
-- `manager`) invites someone by username; the invite only grants access
-- once accepted (`accepted_at`).
--
--  - viewer:  sees the setlist and plays it in Live Mode;
--  - editor:  also adds, removes and reorders songs, blocks and breaks,
--             and sets the key each song is played in;
--  - manager: also edits the setlist's details and invites, promotes and
--             removes viewers and editors (only the owner grants or takes
--             away `manager`).
--
-- Deleting, sharing publicly and the collaborator list's owner stay with
-- the owner. Band setlists never have collaborators: the band's members
-- and roles already cover that.
--
-- Declared low-to-high on purpose, like `band_role`: enum values compare
-- by declaration order.
CREATE TYPE setlist_collaborator_role AS ENUM ('viewer', 'editor', 'manager');

CREATE TABLE setlist_collaborators (
    setlist_id UUID NOT NULL REFERENCES setlists(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role setlist_collaborator_role NOT NULL DEFAULT 'editor',
    invited_by UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMP NOT NULL,
    -- NULL while the invite is pending: a pending collaborator has no
    -- access at all.
    accepted_at TIMESTAMP,
    PRIMARY KEY (setlist_id, user_id)
);

-- "Setlists shared with me" and "my pending invites".
CREATE INDEX idx_setlist_collaborators_user ON setlist_collaborators (user_id, accepted_at);
CREATE INDEX idx_setlist_collaborators_invited_by ON setlist_collaborators (invited_by)
    WHERE invited_by IS NOT NULL;

-- ---------------------------------------------------------------------
-- Who added each song
-- ---------------------------------------------------------------------
--
-- Shown next to each song of a shared setlist (collaborators, bands).
-- Songs already in a personal setlist can only have been added by its
-- owner; for band setlists it isn't known, and stays NULL.

ALTER TABLE setlist_songs ADD COLUMN added_by UUID REFERENCES users(id) ON DELETE SET NULL;
ALTER TABLE setlist_songs ADD COLUMN added_at TIMESTAMP;

CREATE INDEX idx_setlist_songs_added_by ON setlist_songs (added_by) WHERE added_by IS NOT NULL;

UPDATE setlist_songs ss SET added_by = st.user_id
FROM setlists st
WHERE st.id = ss.setlist_id AND st.band_id IS NULL;

-- ---------------------------------------------------------------------
-- Notifications
-- ---------------------------------------------------------------------

-- Someone invited the recipient to collaborate on a setlist.
ALTER TYPE notification_type ADD VALUE 'setlist_invitation';
