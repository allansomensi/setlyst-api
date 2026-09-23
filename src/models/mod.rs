use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

pub mod admin;
pub mod artist;
pub mod audit;
pub mod auth;
pub mod backup;
pub mod band;
pub mod gig;
pub mod metrics;
pub mod notification;
pub mod patch;
pub mod quota;
pub mod setlist;
pub mod song;
pub mod status;
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
    /// (`page >= 1`, `1 <= per_page <= 100`, default 20).
    pub fn resolve(&self) -> (i64, i64) {
        (
            self.page.unwrap_or(1).max(1),
            self.per_page.unwrap_or(20).clamp(1, 100),
        )
    }
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
