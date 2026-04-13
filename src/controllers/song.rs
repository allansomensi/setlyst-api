use crate::{
    database::{
        AppState,
        repositories::song_repository::{SongRepository, SongRepositoryImpl},
    },
    errors::api_error::ApiError,
    models::{
        DeletePayload,
        auth::access::AccessControl,
        song::{CreateSongPayload, SongPublic, UpdateSongPayload},
    },
    validations::existence::{artist_exists, song_exists},
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
    path = "/api/v1/songs/count",
    tags = ["Songs"],
    security((), ("jwt_token" = ["jwt_token"])),
    responses((status = 200, body = i64)))]
pub async fn count_songs(
    State(state): State<Arc<AppState>>,
    _access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    let count = SongRepositoryImpl::count(&state).await?;
    Ok(Json(count))
}

#[utoipa::path(get,
    path = "/api/v1/songs",
    tags = ["Songs"],
    security((), ("jwt_token" = ["jwt_token"])),
    responses((status = 200, body = Vec<SongPublic>)))]
pub async fn find_all_songs(
    State(state): State<Arc<AppState>>,
    _access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    let songs = SongRepositoryImpl::find_all(&state).await?;
    Ok(Json(songs))
}

#[utoipa::path(get,
    path = "/api/v1/songs/{id}",
    tags = ["Songs"],
    security((), ("jwt_token" = ["jwt_token"])),
    responses((status = 200, body = SongPublic), (status = 404, description = "Not Found")))]
pub async fn find_song_by_id(
    Path(id): Path<Uuid>,
    State(state): State<Arc<AppState>>,
    _access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    match SongRepositoryImpl::find_by_id(&state, id).await? {
        Some(song) => Ok(Json(song)),
        None => Err(ApiError::NotFound),
    }
}

#[utoipa::path(post,
    path = "/api/v1/songs",
    tags = ["Songs"],
    request_body = CreateSongPayload,
    security((), ("jwt_token" = ["jwt_token"])),
    responses((status = 201, body = Uuid)))]
pub async fn create_song(
    State(state): State<Arc<AppState>>,
    access: AccessControl,
    Json(payload): Json<CreateSongPayload>,
) -> Result<impl IntoResponse, ApiError> {
    payload.validate()?;
    artist_exists(&state, payload.artist_id).await?;

    let user_id = access.user().id;

    let song = SongRepositoryImpl::create(&state, &payload, user_id).await?;
    info!("Song created: {}", song.id);
    Ok((StatusCode::CREATED, Json(song.id)))
}

#[utoipa::path(put,
    path = "/api/v1/songs",
    tags = ["Songs"],
    request_body = UpdateSongPayload,
    security((), ("jwt_token" = ["jwt_token"])),
    responses((status = 200, body = Uuid)))]
pub async fn update_song(
    State(state): State<Arc<AppState>>,
    _access: AccessControl,
    Json(payload): Json<UpdateSongPayload>,
) -> Result<impl IntoResponse, ApiError> {
    payload.validate()?;
    song_exists(&state, payload.id).await?;

    if let Some(new_artist_id) = payload.artist_id {
        artist_exists(&state, new_artist_id).await?;
    }

    let id = SongRepositoryImpl::update(&state, &payload).await?;
    Ok(Json(id))
}

#[utoipa::path(delete,
    path = "/api/v1/songs",
    tags = ["Songs"],
    request_body = DeletePayload,
    security((), ("jwt_token" = ["jwt_token"])),
    responses((status = 204, description = "No Content")))]
pub async fn delete_song(
    State(state): State<Arc<AppState>>,
    _access: AccessControl,
    Json(payload): Json<DeletePayload>,
) -> Result<impl IntoResponse, ApiError> {
    song_exists(&state, payload.id).await?;

    SongRepositoryImpl::delete(&state, &payload).await?;
    Ok(StatusCode::NO_CONTENT)
}
