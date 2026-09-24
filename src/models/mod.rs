use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

pub mod admin;
pub mod announcement;
pub mod artist;
pub mod audit;
pub mod auth;
pub mod backup;
pub mod band;
pub mod band_note;
pub mod billing;
pub mod communication;
pub mod finance;
pub mod gig;
pub mod link;
pub mod metrics;
pub mod moderation;
pub mod notification;
pub mod patch;
pub mod pin;
pub mod quota;
pub mod release_note;
pub mod security;
pub mod setlist;
pub mod song;
pub mod status;
pub mod suggestion;
pub mod tour;
pub mod trash;
pub mod user;
pub mod user_preferences;

#[derive(serde::Deserialize, serde::Serialize, utoipa::ToSchema)]
pub struct DeletePayload {
    pub id: uuid::Uuid,
}

#[derive(Serialize, Deserialize, ToSchema, Debug)]
pub struct PaginatedResponse<T> {
    pub data: Vec<T>,
    pub meta: PaginationMeta,
}

#[derive(Deserialize, IntoParams, Debug)]
#[into_params(parameter_in = Query)]
pub struct PaginationQuery {
    #[param(default = 1, minimum = 1, required = false)]
    pub page: Option<i64>,

    #[param(default = 20, minimum = 1, maximum = 100, required = false)]
    pub per_page: Option<i64>,
}

impl PaginationQuery {
    /// The requested page and page size, clamped to sane bounds
    /// (`1 <= page <= 1_000`, `1 <= per_page <= 100`, default 20). The
    /// page cap keeps `(page - 1) * per_page` far from overflowing.
    pub fn resolve(&self) -> (i64, i64) {
        (
            clamp_page(self.page),
            self.per_page.unwrap_or(20).clamp(1, 100),
        )
    }
}

/// Highest page number accepted anywhere: deep `OFFSET`s make Postgres read
/// and discard every row before the page, so lists stop at 1 000 pages
/// (100 000 rows at the largest page size).
pub const MAX_PAGE: i64 = 1_000;

/// A requested page number clamped to `1..=MAX_PAGE` (default 1).
pub fn clamp_page(page: Option<i64>) -> i64 {
    page.unwrap_or(1).clamp(1, MAX_PAGE)
}

/// `(page, per_page)` with the given default page size, both clamped.
pub fn resolve_page(page: Option<i64>, per_page: Option<i64>, default_per_page: i64) -> (i64, i64) {
    (
        clamp_page(page),
        per_page.unwrap_or(default_per_page).clamp(1, 100),
    )
}

impl<T> PaginatedResponse<T> {
    pub fn new(data: Vec<T>, total_items: i64, current_page: i64, per_page: i64) -> Self {
        let total_pages = if per_page > 0 {
            (total_items + per_page - 1) / per_page
        } else {
            0
        };
        Self {
            data,
            meta: PaginationMeta {
                total_items,
                current_page,
                per_page,
                total_pages,
            },
        }
    }
}

#[derive(Serialize, Deserialize, ToSchema, Debug)]
pub struct PaginationMeta {
    pub total_items: i64,
    pub current_page: i64,
    pub per_page: i64,
    pub total_pages: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pagination_is_clamped() {
        let q = PaginationQuery {
            page: Some(i64::MAX),
            per_page: Some(i64::MAX),
        };
        assert_eq!(q.resolve(), (MAX_PAGE, 100));
        let q = PaginationQuery {
            page: Some(-5),
            per_page: Some(0),
        };
        assert_eq!(q.resolve(), (1, 1));
        assert_eq!(resolve_page(None, None, 25), (1, 25));
    }
}
