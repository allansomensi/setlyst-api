use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

pub mod artist;
pub mod auth;
pub mod setlist;
pub mod song;
pub mod status;
pub mod user;

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

#[derive(Serialize, Deserialize, ToSchema, Debug)]
pub struct PaginationMeta {
    pub total_items: i64,
    pub current_page: i64,
    pub per_page: i64,
    pub total_pages: i64,
}
