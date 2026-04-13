pub mod artist;
pub mod auth;
pub mod status;
pub mod user;

#[derive(serde::Deserialize, serde::Serialize, utoipa::ToSchema)]
pub struct DeletePayload {
    pub id: uuid::Uuid,
}
