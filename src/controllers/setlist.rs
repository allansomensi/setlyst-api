use crate::{
    database::{
        AppState,
        repositories::setlist_repository::{SetlistRepository, SetlistRepositoryImpl},
    },
    errors::api_error::ApiError,
    models::{
        DeletePayload,
        auth::access::AccessControl,
        setlist::{CreateSetlistPayload, SetlistPublic, UpdateSetlistPayload},
    },
    validations::existence::{setlist_exists, song_exists},
};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use std::sync::Arc;
use uuid::Uuid;
use validator::Validate;

#[utoipa::path(get,
    path = "/api/v1/setlists/count",
    tags = ["Setlists"],
    security((), ("jwt_token" = ["jwt_token"])),
    responses((status = 200, body = i64)))]
pub async fn count_setlists(
    State(state): State<Arc<AppState>>,
    _access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    let count = SetlistRepositoryImpl::count(&state).await?;
    Ok(Json(count))
}

#[utoipa::path(get,
    path = "/api/v1/setlists",
    tags = ["Setlists"],
    security((), ("jwt_token" = ["jwt_token"])),
    responses((status = 200, body = Vec<SetlistPublic>)))]
pub async fn find_all_setlists(
    State(state): State<Arc<AppState>>,
    _access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    let setlists = SetlistRepositoryImpl::find_all(&state).await?;
    Ok(Json(setlists))
}

#[utoipa::path(get,
    path = "/api/v1/setlists/{id}",
    tags = ["Setlists"],
    security((), ("jwt_token" = ["jwt_token"])),
    responses((status = 200, body = SetlistPublic), (status = 404, description = "Not Found")))]
pub async fn find_setlist_by_id(
    Path(id): Path<Uuid>,
    State(state): State<Arc<AppState>>,
    _access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    match SetlistRepositoryImpl::find_by_id(&state, id).await? {
        Some(setlist) => Ok(Json(setlist)),
        None => Err(ApiError::NotFound),
    }
}

#[utoipa::path(post,
    path = "/api/v1/setlists",
    tags = ["Setlists"],
    request_body = CreateSetlistPayload,
    security((), ("jwt_token" = ["jwt_token"])),
    responses((status = 201, body = Uuid)))]
pub async fn create_setlist(
    State(state): State<Arc<AppState>>,
    access: AccessControl,
    Json(payload): Json<CreateSetlistPayload>,
) -> Result<impl IntoResponse, ApiError> {
    payload.validate()?;
    for song_id in &payload.song_ids {
        song_exists(&state, *song_id).await?;
    }

    let user_id = access.user().id;
    let setlist = SetlistRepositoryImpl::create(&state, &payload, user_id).await?;
    let id = setlist.id;

    tracing::info!("Setlist created: {id}");
    Ok((StatusCode::CREATED, Json(id)))
}

#[utoipa::path(put,
    path = "/api/v1/setlists",
    tags = ["Setlists"],
    request_body = UpdateSetlistPayload,
    security((), ("jwt_token" = ["jwt_token"])),
    responses((status = 200, body = Uuid)))]
pub async fn update_setlist(
    State(state): State<Arc<AppState>>,
    _access: AccessControl,
    Json(payload): Json<UpdateSetlistPayload>,
) -> Result<impl IntoResponse, ApiError> {
    payload.validate()?;
    setlist_exists(&state, payload.id).await?;

    if let Some(song_ids) = &payload.song_ids {
        for song_id in song_ids {
            song_exists(&state, *song_id).await?;
        }
    }

    let id = SetlistRepositoryImpl::update(&state, &payload).await?;
    Ok(Json(id))
}

#[utoipa::path(delete,
    path = "/api/v1/setlists",
    tags = ["Setlists"],
    request_body = DeletePayload,
    security((), ("jwt_token" = ["jwt_token"])),
    responses((status = 204, description = "No Content")))]
pub async fn delete_setlist(
    State(state): State<Arc<AppState>>,
    _access: AccessControl,
    Json(payload): Json<DeletePayload>,
) -> Result<impl IntoResponse, ApiError> {
    setlist_exists(&state, payload.id).await?;
    SetlistRepositoryImpl::delete(&state, &payload).await?;
    Ok(StatusCode::NO_CONTENT)
}
