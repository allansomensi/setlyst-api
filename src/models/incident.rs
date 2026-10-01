//! Incidents and scheduled maintenance published on the status page.

use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, Type};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;
use validator::{Validate, ValidationError};

/// Parts of the platform an incident can affect.
pub const INCIDENT_COMPONENTS: [&str; 6] = ["web", "api", "sync", "email", "payments", "exports"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type, ToSchema)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "incident_kind", rename_all = "snake_case")]
pub enum IncidentKind {
    Incident,
    Maintenance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type, ToSchema)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "incident_impact", rename_all = "snake_case")]
pub enum IncidentImpact {
    None,
    Minor,
    Major,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type, ToSchema)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "incident_status", rename_all = "snake_case")]
pub enum IncidentStatus {
    /// Maintenance announced ahead of time.
    Scheduled,
    Investigating,
    Identified,
    Monitoring,
    Resolved,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, ToSchema)]
pub struct IncidentUpdate {
    pub id: Uuid,
    pub incident_id: Uuid,
    pub status: IncidentStatus,
    pub body: String,
    pub created_at: NaiveDateTime,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, ToSchema)]
pub struct IncidentRow {
    pub id: Uuid,
    pub kind: IncidentKind,
    pub title: String,
    pub impact: IncidentImpact,
    pub status: IncidentStatus,
    pub components: Vec<String>,
    pub scheduled_for: Option<NaiveDateTime>,
    pub scheduled_until: Option<NaiveDateTime>,
    pub started_at: Option<NaiveDateTime>,
    pub resolved_at: Option<NaiveDateTime>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

/// An incident with its timeline (newest update first).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Incident {
    #[serde(flatten)]
    pub incident: IncidentRow,
    pub updates: Vec<IncidentUpdate>,
}

/// `GET /public/incidents`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PublicIncidents {
    /// Not resolved yet (including upcoming maintenance), newest first.
    pub active: Vec<Incident>,
    /// Resolved in the last 30 days, newest first.
    pub recent: Vec<Incident>,
}

fn validate_components(components: &[String]) -> Result<(), ValidationError> {
    if components
        .iter()
        .all(|c| INCIDENT_COMPONENTS.contains(&c.as_str()))
    {
        Ok(())
    } else {
        let mut error = ValidationError::new("component");
        error.message = Some(
            format!(
                "Components must be among {}.",
                INCIDENT_COMPONENTS.join(", ")
            )
            .into(),
        );
        Err(error)
    }
}

fn validate_text(body: &str) -> Result<(), ValidationError> {
    let length = body.trim().chars().count();
    if length == 0 || length > 2_000 {
        let mut error = ValidationError::new("length");
        error.message = Some("Must be between 1 and 2 000 characters.".into());
        return Err(error);
    }
    Ok(())
}

fn validate_title(title: &str) -> Result<(), ValidationError> {
    let length = title.trim().chars().count();
    if !(3..=150).contains(&length) {
        let mut error = ValidationError::new("length");
        error.message = Some("Must be between 3 and 150 characters.".into());
        return Err(error);
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize, ToSchema, Validate)]
pub struct CreateIncidentPayload {
    pub kind: IncidentKind,
    #[validate(custom(function = "validate_title"))]
    pub title: String,
    pub impact: IncidentImpact,
    /// `scheduled` for upcoming maintenance; an incident starts as
    /// `investigating` (the default) or later.
    pub status: Option<IncidentStatus>,
    #[serde(default)]
    #[validate(custom(function = "validate_components"))]
    pub components: Vec<String>,
    pub scheduled_for: Option<NaiveDateTime>,
    pub scheduled_until: Option<NaiveDateTime>,
    /// The first update of the timeline.
    #[validate(custom(function = "validate_text"))]
    pub message: String,
}

/// Edits to an incident's own fields (typos, impact, components, the
/// maintenance window). Status changes go through updates.
#[derive(Debug, Clone, Deserialize, ToSchema, Validate)]
pub struct UpdateIncidentPayload {
    #[validate(custom(function = "validate_title"))]
    pub title: Option<String>,
    pub impact: Option<IncidentImpact>,
    #[validate(custom(function = "validate_components"))]
    pub components: Option<Vec<String>>,
    #[serde(default, deserialize_with = "crate::models::patch::double_option")]
    #[schema(value_type = Option<NaiveDateTime>)]
    pub scheduled_for: Option<Option<NaiveDateTime>>,
    #[serde(default, deserialize_with = "crate::models::patch::double_option")]
    #[schema(value_type = Option<NaiveDateTime>)]
    pub scheduled_until: Option<Option<NaiveDateTime>>,
}

#[derive(Debug, Clone, Deserialize, ToSchema, Validate)]
pub struct PostIncidentUpdatePayload {
    pub status: IncidentStatus,
    #[validate(custom(function = "validate_text"))]
    pub body: String,
}

#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct IncidentListQuery {
    pub page: Option<i64>,
    pub per_page: Option<i64>,
    /// `true`: only unresolved ones.
    pub active: Option<bool>,
}
