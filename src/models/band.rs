use crate::validations::band::{validate_band_description, validate_band_name, validate_logo_url};
use chrono::{NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::prelude::{FromRow, Type};
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

/// A member's standing within a single band.
///
/// Distinct from [`crate::models::user::Role`], which is the user's
/// site-wide role. Band roles only apply within the scope of one band.
#[derive(
    ToSchema, PartialEq, Eq, PartialOrd, Ord, Debug, Clone, Copy, Serialize, Deserialize, Type,
)]
#[serde(rename_all(serialize = "lowercase", deserialize = "lowercase"))]
#[sqlx(type_name = "band_role", rename_all = "lowercase")]
pub enum BandRole {
    Member,
    Moderator,
    Admin,
    Owner,
}

impl BandRole {
    /// Returns `true` when this role grants at least the privileges of `min`.
    pub fn satisfies(&self, min: BandRole) -> bool {
        *self >= min
    }
}

impl std::fmt::Display for BandRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            BandRole::Owner => "owner",
            BandRole::Admin => "admin",
            BandRole::Moderator => "moderator",
            BandRole::Member => "member",
        };
        f.write_str(s)
    }
}

#[derive(ToSchema, Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Band {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub logo_url: Option<String>,
    /// The account that created the band — `None` once that account is
    /// deleted. Ownership is tracked by the `owner` membership, not here.
    pub created_by: Option<Uuid>,
    #[sqlx(default)]
    pub updated_by: Option<Uuid>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

/// A [`Band`] enriched with data scoped to the requesting user.
///
/// Mirrors `Band`'s columns directly (rather than nesting it) to stay
/// consistent with how other list queries in this codebase compute
/// extra aggregate columns alongside a table's own fields.
#[derive(ToSchema, Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct BandWithMembership {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub logo_url: Option<String>,
    pub created_by: Option<Uuid>,
    #[sqlx(default)]
    pub updated_by: Option<Uuid>,
    #[sqlx(default)]
    pub updated_by_username: Option<String>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
    pub member_count: i64,
    pub my_role: BandRole,
    /// Whether the *caller* has favorited this band — personal, never
    /// affects anyone else's view or any permission.
    pub is_favorite: bool,
    /// The band's repertoire (see `Setlist::is_repertoire`).
    #[sqlx(default)]
    pub repertoire_id: Option<Uuid>,
    /// Up-votes that accept a song suggestion automatically (`None` =
    /// never automatic).
    #[sqlx(default)]
    pub suggestion_auto_accept_votes: Option<i32>,
    /// Suggestions still open for voting.
    #[sqlx(default)]
    pub open_suggestions: i64,
    /// Whether the *caller* pinned this band to their home screen.
    #[sqlx(default)]
    pub is_pinned: bool,
    /// What the *caller* may do in this band, computed with the same rules
    /// the API enforces.
    #[sqlx(flatten)]
    pub my_permissions: MyBandPermissions,
}

/// The caller's effective band permissions: `owner`/`admin` always have
/// every one; `member`/`moderator` follow the band's permission matrix
/// (`band_role_permissions`, denied when no row exists).
#[derive(ToSchema, Debug, Clone, Copy, Default, PartialEq, Eq, FromRow, Serialize, Deserialize)]
pub struct MyBandPermissions {
    pub manage_setlists: bool,
    pub manage_songs: bool,
    pub export_pdf: bool,
}

impl Band {
    pub fn new(name: &str, slug: String, description: Option<String>, created_by: Uuid) -> Self {
        let now = Utc::now().naive_utc();
        Self {
            id: Uuid::new_v4(),
            name: name.to_string(),
            slug,
            description,
            logo_url: None,
            created_by: Some(created_by),
            updated_by: None,
            created_at: now,
            updated_at: now,
        }
    }
}

#[derive(Deserialize, Serialize, ToSchema, Validate)]
pub struct CreateBandPayload {
    #[validate(custom(function = "validate_band_name"))]
    pub name: String,
    #[validate(custom(function = "validate_band_description"))]
    pub description: Option<String>,
}

/// Nullable fields use `Option<Option<T>>`: absent = unchanged, `null` =
/// clear (see [`crate::models::patch`]).
#[derive(Deserialize, Serialize, ToSchema, Validate, Default)]
pub struct UpdateBandPayload {
    #[validate(custom(function = "validate_band_name"))]
    pub name: Option<String>,
    #[serde(default, deserialize_with = "crate::models::patch::double_option")]
    #[validate(custom(function = "validate_band_description"))]
    pub description: Option<Option<String>>,
    #[serde(default, deserialize_with = "crate::models::patch::double_option")]
    #[validate(custom(function = "validate_logo_url"))]
    pub logo_url: Option<Option<String>>,
    /// Up-votes that accept a song suggestion automatically (1 to 100);
    /// `null` turns automatic acceptance off.
    #[serde(default, deserialize_with = "crate::models::patch::double_option")]
    #[validate(range(
        min = 1,
        max = 100,
        message = "The vote threshold must be between 1 and 100."
    ))]
    pub suggestion_auto_accept_votes: Option<Option<i32>>,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, ToSchema)]
pub struct BandMember {
    pub id: Uuid,
    pub band_id: Uuid,
    pub user_id: Uuid,
    pub role: BandRole,
    /// Free-text identification label (e.g. "Guitarrista", "Baixista") —
    /// purely cosmetic, unrelated to `role` and never affects permissions.
    pub title: Option<String>,
    pub joined_at: NaiveDateTime,
    // Joined from `users` for display purposes.
    pub username: String,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
}

#[derive(Deserialize, Serialize, ToSchema, Validate)]
pub struct UpdateBandMemberTitlePayload {
    #[validate(length(max = 50, message = "Title must be at most 50 chars."))]
    pub title: Option<String>,
}

/// A specific action within a band that can be independently permitted or
/// denied for the `member` and `moderator` roles via
/// [`BandRolePermissions`]. `admin` and `owner` always have every
/// permission and are never restricted, so they never appear here.
#[derive(ToSchema, PartialEq, Eq, Debug, Clone, Copy, Serialize, Deserialize, Type, Hash)]
#[serde(rename_all(serialize = "snake_case", deserialize = "snake_case"))]
#[sqlx(type_name = "band_permission", rename_all = "snake_case")]
pub enum BandPermission {
    /// Add/remove/reorder songs, blocks and breaks in the band's setlists,
    /// and edit or delete those setlists.
    ManageSetlists,
    /// Edit or delete the band's own copy of a song (title, lyrics, BPM,
    /// key, etc.) once it has been added to a band setlist.
    ManageSongs,
    /// Export a band setlist to PDF.
    ExportPdf,
}

/// One row of a band's permission matrix: whether `role` is allowed to
/// perform `permission`. Only `member`/`moderator` rows are meaningful —
/// `admin`/`owner` are always allowed everything and are never stored.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize, ToSchema)]
pub struct BandRolePermission {
    pub role: BandRole,
    pub permission: BandPermission,
    pub allowed: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, ToSchema, Validate)]
pub struct BandRolePermissionEntry {
    #[validate(custom(function = "validate_configurable_role"))]
    pub role: BandRole,
    pub permission: BandPermission,
    pub allowed: bool,
}

fn validate_configurable_role(role: &BandRole) -> Result<(), validator::ValidationError> {
    match role {
        BandRole::Member | BandRole::Moderator => Ok(()),
        BandRole::Admin | BandRole::Owner => {
            let mut error = validator::ValidationError::new("non_configurable_role");
            error.message = Some(std::borrow::Cow::from(
                "admin and owner permissions cannot be customized — they always have every permission.",
            ));
            Err(error)
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, ToSchema, Validate)]
pub struct UpdateBandRolePermissionsPayload {
    #[validate(length(
        min = 1,
        max = 20,
        message = "Between 1 and 20 permission entries are required."
    ))]
    #[validate(nested)]
    pub permissions: Vec<BandRolePermissionEntry>,
}

#[derive(Deserialize, Serialize, ToSchema, Validate)]
pub struct UpdateBandMemberRolePayload {
    pub role: BandRole,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, ToSchema)]
pub struct BandInvite {
    pub id: Uuid,
    pub band_id: Uuid,
    pub code: String,
    pub role: BandRole,
    pub created_by: Uuid,
    pub max_uses: Option<i32>,
    pub uses_count: i32,
    pub expires_at: Option<NaiveDateTime>,
    pub revoked_at: Option<NaiveDateTime>,
    pub created_at: NaiveDateTime,
}

#[derive(Deserialize, Serialize, ToSchema, Validate)]
pub struct CreateBandInvitePayload {
    pub role: Option<BandRole>,
    #[validate(range(min = 1, max = 1000, message = "Max uses must be between 1 and 1000."))]
    pub max_uses: Option<i32>,
    /// Up to one year; 7 days when omitted.
    #[validate(range(
        min = 1,
        max = 8760,
        message = "Expiry must be between 1 hour and 1 year."
    ))]
    pub expires_in_hours: Option<i64>,
}

/// Staff adding a user straight into a band (no invite).
#[derive(Deserialize, Serialize, ToSchema, Validate)]
pub struct AdminAddBandMemberPayload {
    pub user_id: Uuid,
    /// Defaults to `member`. `owner` is not accepted — use transfer-ownership.
    pub role: Option<BandRole>,
}

#[derive(Deserialize, Serialize, ToSchema, Validate)]
pub struct TransferOwnershipPayload {
    pub new_owner_id: Uuid,
}
