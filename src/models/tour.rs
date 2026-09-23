//! Tours: a named run of gigs between two dates, personal or of a band.

use crate::models::gig::GigStatus;
use chrono::{NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};
use sqlx::prelude::FromRow;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;
use validator::{Validate, ValidationError};

pub const MAX_TOUR_NAME_LENGTH: u64 = 120;

#[derive(ToSchema, Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Tour {
    pub id: Uuid,
    /// Creator (the band's owner after the creator's account is deleted).
    pub user_id: Uuid,
    /// `None` for a personal tour.
    pub band_id: Option<Uuid>,
    #[sqlx(default)]
    pub band_name: Option<String>,
    pub name: String,
    pub description: Option<String>,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    /// Live gigs of the tour.
    #[sqlx(default)]
    pub gig_count: i64,
    /// The first confirmed gig from now on, if any.
    #[sqlx(default)]
    pub next_gig_at: Option<NaiveDateTime>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
    #[sqlx(default)]
    pub updated_by_username: Option<String>,
    #[sqlx(default)]
    pub owner_username: Option<String>,
    /// Whether the *caller* pinned this tour to their home screen.
    #[sqlx(default)]
    pub is_pinned: bool,
}

/// The setlist of a gig inside a tour.
#[derive(ToSchema, Debug, Clone, Serialize, Deserialize)]
pub struct TourGigSetlist {
    pub id: Uuid,
    pub title: String,
    /// `true` for a band's repertoire (show a translated name instead of
    /// the stored `title`).
    pub is_repertoire: bool,
    pub song_count: i64,
    /// Seconds (songs + breaks).
    pub total_duration: i32,
}

/// A gig as listed in a tour's timeline.
#[derive(ToSchema, Debug, Clone, Serialize, Deserialize)]
pub struct GigSummary {
    pub id: Uuid,
    pub venue: String,
    pub location: Option<String>,
    pub scheduled_at: NaiveDateTime,
    pub status: GigStatus,
    pub setlist: Option<TourGigSetlist>,
}

#[derive(ToSchema, Debug, Clone, Default, Serialize, Deserialize)]
pub struct TourStats {
    pub total_gigs: i64,
    pub confirmed: i64,
    pub completed: i64,
    pub cancelled: i64,
    /// Sum of the gigs' setlist durations, in seconds (a setlist played
    /// at several gigs counts once per gig).
    pub total_setlist_duration: i64,
}

/// `GET /tours/{id}`: the tour, its gigs (soonest first) and totals.
#[derive(ToSchema, Debug, Clone, Serialize)]
pub struct TourDetail {
    #[serde(flatten)]
    pub tour: Tour,
    pub gigs: Vec<GigSummary>,
    pub stats: TourStats,
}

fn validate_name(name: &str) -> Result<(), ValidationError> {
    let len = name.trim().chars().count();
    if len == 0 || len as u64 > MAX_TOUR_NAME_LENGTH || name.chars().any(char::is_control) {
        let mut error = ValidationError::new("invalid_tour_name");
        error.message = Some(std::borrow::Cow::from(
            "The tour name must be between 1 and 120 characters, on one line.",
        ));
        return Err(error);
    }
    Ok(())
}

fn dates_error() -> ValidationError {
    let mut error = ValidationError::new("invalid_tour_dates");
    error.message = Some(std::borrow::Cow::from(
        "The end date can't be before the start date.",
    ));
    error
}

fn validate_create_dates(payload: &CreateTourPayload) -> Result<(), ValidationError> {
    if payload.end_date < payload.start_date {
        return Err(dates_error());
    }
    Ok(())
}

/// Checks the dates a tour would have after an update.
pub fn check_dates(start: NaiveDate, end: NaiveDate) -> Result<(), validator::ValidationErrors> {
    if end < start {
        let mut errors = validator::ValidationErrors::new();
        errors.add("end_date", dates_error());
        return Err(errors);
    }
    Ok(())
}

#[derive(Debug, Deserialize, Serialize, ToSchema, Validate)]
#[validate(schema(function = "validate_create_dates", skip_on_field_errors = false))]
pub struct CreateTourPayload {
    #[validate(custom(function = "validate_name"))]
    pub name: String,
    #[validate(custom(function = "crate::validations::text::validate_description"))]
    pub description: Option<String>,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    /// Create the tour under a band (requires its `manage_setlists`
    /// permission).
    pub band_id: Option<Uuid>,
}

/// Absent = unchanged; `description: null` clears it.
#[derive(Debug, Default, Deserialize, Serialize, ToSchema, Validate)]
pub struct UpdateTourPayload {
    #[validate(custom(function = "validate_name"))]
    pub name: Option<String>,
    #[serde(default, deserialize_with = "crate::models::patch::double_option")]
    #[validate(custom(function = "crate::validations::text::validate_description"))]
    pub description: Option<Option<String>>,
    pub start_date: Option<NaiveDate>,
    pub end_date: Option<NaiveDate>,
}

/// Which tours to list.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum TourStatusFilter {
    /// Tours that haven't ended yet (`end_date >= today`), soonest first.
    #[default]
    Upcoming,
    /// Tours that already ended, most recent first.
    Past,
    /// Everything, most recent start first.
    All,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TourListQuery {
    pub page: Option<i64>,
    pub per_page: Option<i64>,
    pub status: Option<TourStatusFilter>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_must_be_ordered() {
        let payload: CreateTourPayload = serde_json::from_value(serde_json::json!({
            "name": "Verão", "start_date": "2026-02-10", "end_date": "2026-02-01"
        }))
        .unwrap();
        assert!(payload.validate().is_err());
        let payload: CreateTourPayload = serde_json::from_value(serde_json::json!({
            "name": "  ", "start_date": "2026-02-01", "end_date": "2026-02-01"
        }))
        .unwrap();
        assert!(payload.validate().is_err());
        let payload: CreateTourPayload = serde_json::from_value(serde_json::json!({
            "name": "Verão", "start_date": "2026-02-01", "end_date": "2026-02-01"
        }))
        .unwrap();
        assert!(payload.validate().is_ok());
    }
}
