use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use sqlx::prelude::{FromRow, Type};
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

/// Most collaborators (pending invites included) one setlist can have.
pub const MAX_SETLIST_COLLABORATORS: i64 = 20;

/// What a collaborator may do in someone else's personal setlist. Ordered
/// low to high (see `0017_setlist_collaboration.sql`).
///
/// - `viewer`: sees the setlist and plays it in Live Mode;
/// - `editor`: also changes its running order (songs, blocks, breaks,
///   keys);
/// - `manager`: also edits its details and manages viewers and editors.
#[derive(
    ToSchema, PartialEq, Eq, PartialOrd, Ord, Debug, Clone, Copy, Serialize, Deserialize, Type,
)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "setlist_collaborator_role", rename_all = "lowercase")]
pub enum CollaboratorRole {
    Viewer,
    Editor,
    Manager,
}

impl CollaboratorRole {
    pub fn key(&self) -> &'static str {
        match self {
            CollaboratorRole::Viewer => "viewer",
            CollaboratorRole::Editor => "editor",
            CollaboratorRole::Manager => "manager",
        }
    }
}

/// The caller's standing in a setlist, as far as collaboration goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetlistStanding {
    /// The owner of a personal setlist.
    Owner,
    /// An accepted collaborator.
    Collaborator(CollaboratorRole),
}

impl SetlistStanding {
    /// Whether this standing may manage collaborators holding `role`
    /// (invite, change or remove them): the owner manages everyone, a
    /// manager only viewers and editors.
    pub fn can_manage(&self, role: CollaboratorRole) -> bool {
        match self {
            SetlistStanding::Owner => true,
            SetlistStanding::Collaborator(CollaboratorRole::Manager) => {
                role < CollaboratorRole::Manager
            }
            SetlistStanding::Collaborator(_) => false,
        }
    }
}

/// One person a setlist is shared with (`GET /setlists/{id}/collaborators`).
#[derive(ToSchema, Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct SetlistCollaborator {
    pub user_id: Uuid,
    pub username: String,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub avatar_url: Option<String>,
    pub role: CollaboratorRole,
    /// `false` while the invite waits for an answer.
    pub accepted: bool,
    pub invited_by_username: Option<String>,
    pub created_at: NaiveDateTime,
    pub accepted_at: Option<NaiveDateTime>,
}

/// The owner, as listed with the collaborators.
#[derive(ToSchema, Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct SetlistOwner {
    pub user_id: Uuid,
    pub username: String,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub avatar_url: Option<String>,
}

/// `GET /setlists/{id}/collaborators`.
#[derive(ToSchema, Debug, Clone, Serialize, Deserialize)]
pub struct SetlistCollaborators {
    pub owner: SetlistOwner,
    /// Accepted first (managers, editors, viewers), then pending invites.
    pub collaborators: Vec<SetlistCollaborator>,
}

/// An invite waiting for the caller's answer (`GET /setlists/invitations`).
#[derive(ToSchema, Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct SetlistInvitation {
    pub setlist_id: Uuid,
    pub setlist_title: String,
    pub owner_username: String,
    pub owner_avatar_url: Option<String>,
    pub owner_id: Uuid,
    pub invited_by_username: Option<String>,
    pub role: CollaboratorRole,
    pub created_at: NaiveDateTime,
}

#[derive(Debug, Deserialize, Serialize, ToSchema, Validate)]
pub struct InviteCollaboratorPayload {
    /// The username of the account to invite.
    #[validate(length(
        min = 1,
        max = 50,
        message = "Username must be between 1 and 50 chars."
    ))]
    pub username: String,
    /// Defaults to `editor`.
    pub role: Option<CollaboratorRole>,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct UpdateCollaboratorPayload {
    pub role: CollaboratorRole,
}

/// Where the account found by `GET /setlists/{id}/collaborators/lookup`
/// stands in the setlist.
#[derive(ToSchema, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateStatus {
    /// Can be invited.
    Available,
    /// Already invited, waiting for an answer.
    Invited,
    /// Already an accepted collaborator.
    Collaborator,
    /// The setlist's owner.
    Owner,
    /// The caller themselves.
    #[serde(rename = "self")]
    Myself,
}

/// An account looked up by username before inviting it
/// (`GET /setlists/{id}/collaborators/lookup`): only what any signed-in
/// user sees of another profile (username and avatar).
#[derive(ToSchema, Debug, Clone, Serialize, Deserialize)]
pub struct CollaboratorCandidate {
    pub user_id: Uuid,
    pub username: String,
    pub avatar_url: Option<String>,
    pub status: CandidateStatus,
}

/// Query of `GET /setlists/{id}/collaborators/lookup`.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct CollaboratorLookupQuery {
    /// The username to look up (case-insensitive; a leading `@` is
    /// ignored).
    pub username: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_are_ordered() {
        assert!(CollaboratorRole::Viewer < CollaboratorRole::Editor);
        assert!(CollaboratorRole::Editor < CollaboratorRole::Manager);
    }

    #[test]
    fn who_manages_whom() {
        let owner = SetlistStanding::Owner;
        let manager = SetlistStanding::Collaborator(CollaboratorRole::Manager);
        let editor = SetlistStanding::Collaborator(CollaboratorRole::Editor);
        assert!(owner.can_manage(CollaboratorRole::Manager));
        assert!(manager.can_manage(CollaboratorRole::Editor));
        assert!(manager.can_manage(CollaboratorRole::Viewer));
        assert!(!manager.can_manage(CollaboratorRole::Manager));
        assert!(!editor.can_manage(CollaboratorRole::Viewer));
    }
}
