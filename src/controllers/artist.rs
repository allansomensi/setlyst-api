use crate::{
    database::{
        AppState,
        repositories::artist_repository::{ArtistRepository, ArtistRepositoryImpl},
    },
    errors::api_error::ApiError,
    models::{
        DeletePayload,
        artist::{ArtistPublic, CreateArtistPayload, UpdateArtistPayload},
        auth::access::AccessControl,
        user::Role,
    },
    validations::{existence::artist_exists, uniqueness::is_artist_unique},
};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use std::sync::Arc;
use tracing::info;
use uuid::Uuid;
use validator::Validate;

#[utoipa::path(get,
    path = "/api/v1/artists/count",
    tags = ["Artists"],
    security((), ("jwt_token" = ["jwt_token"])),
    responses((status = 200, body = i64)))]
pub async fn count_artists(
    State(state): State<Arc<AppState>>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    access.require_any_role(&[Role::Admin, Role::Moderator])?;
    let count = ArtistRepositoryImpl::count(&state).await?;
    Ok(Json(count))
}

#[utoipa::path(get,
    path = "/api/v1/artists",
    tags = ["Artists"],
    security((), ("jwt_token" = ["jwt_token"])),
    responses((status = 200, body = Vec<ArtistPublic>)))]
pub async fn find_all_artists(
    State(state): State<Arc<AppState>>,
    _access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    let artists = ArtistRepositoryImpl::find_all(&state).await?;
    Ok(Json(artists))
}

#[utoipa::path(get,
    path = "/api/v1/artists/{id}",
    tags = ["Artists"],
    security((), ("jwt_token" = ["jwt_token"])),
    responses((status = 200, body = ArtistPublic), (status = 404, description = "Not Found")))]
pub async fn find_artist_by_id(
    Path(id): Path<Uuid>,
    State(state): State<Arc<AppState>>,
    _access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    match ArtistRepositoryImpl::find_by_id(&state, id).await? {
        Some(artist) => Ok(Json(artist)),
        None => Err(ApiError::NotFound),
    }
}

#[utoipa::path(post,
    path = "/api/v1/artists",
    tags = ["Artists"],
    request_body = CreateArtistPayload,
    security((), ("jwt_token" = ["jwt_token"])),
    responses((status = 201, body = Uuid)))]
pub async fn create_artist(
    State(state): State<Arc<AppState>>,
    access: AccessControl,
    Json(payload): Json<CreateArtistPayload>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_any_role(&[Role::Admin, Role::Moderator])?;
    payload.validate()?;
    is_artist_unique(&state, &payload.name).await?;

    let artist = ArtistRepositoryImpl::create(&state, &payload).await?;
    info!("Artist created: {}", artist.id);
    Ok((StatusCode::CREATED, Json(artist.id)))
}

#[utoipa::path(put,
    path = "/api/v1/artists",
    tags = ["Artists"],
    request_body = UpdateArtistPayload,
    security((), ("jwt_token" = ["jwt_token"])),
    responses((status = 200, body = Uuid)))]
pub async fn update_artist(
    State(state): State<Arc<AppState>>,
    access: AccessControl,
    Json(payload): Json<UpdateArtistPayload>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_any_role(&[Role::Admin, Role::Moderator])?;
    payload.validate()?;
    artist_exists(&state, payload.id).await?;

    let id = ArtistRepositoryImpl::update(&state, &payload).await?;
    Ok(Json(id))
}

#[utoipa::path(delete,
    path = "/api/v1/artists",
    tags = ["Artists"],
    request_body = DeletePayload,
    security((), ("jwt_token" = ["jwt_token"])),
    responses((status = 204, description = "No Content")))]
pub async fn delete_artist(
    State(state): State<Arc<AppState>>,
    access: AccessControl,
    Json(payload): Json<DeletePayload>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_any_role(&[Role::Admin, Role::Moderator])?;
    artist_exists(&state, payload.id).await?;

    ArtistRepositoryImpl::delete(&state, &payload).await?;
    Ok(StatusCode::NO_CONTENT)
}
