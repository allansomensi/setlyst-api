//! A song's manual harmonic analysis (`/songs/{id}/analysis`). Same
//! access as the song: whoever sees it reads the analysis, whoever may
//! manage it changes it.

use crate::{
    database::AppState,
    errors::api_error::ApiError,
    models::{
        auth::access::AccessControl,
        song_analysis::{SaveSongAnalysisPayload, SongAnalysis},
    },
};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use tracing::info;
use uuid::Uuid;

#[utoipa::path(
    get,
    path = "/api/v1/songs/{id}/analysis",
    tags = ["Songs"],
    summary = "A song's harmonic analysis.",
    description = "Anyone who can see the song (its owner; any member of the band for a band's song). `null` while the song has none.",
    params(("id" = Uuid, Path, description = "The song ID")),
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "The analysis, or `null`.", body = Option<SongAnalysis>),
        (status = 404, description = "Song not found.")
    )
)]
pub async fn find_song_analysis(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    state
        .song_repo
        .find_by_id(id, user_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let analysis = state.song_analysis_repo.find(id).await?;
    Ok(Json(analysis))
}

#[utoipa::path(
    put,
    path = "/api/v1/songs/{id}/analysis",
    tags = ["Songs"],
    summary = "Save a song's harmonic analysis.",
    description = "Creates or replaces the whole document; same permission as editing the song. `content` must be a JSON object of at most 256 KiB, nested at most 8 levels deep (`VALIDATION_ERROR`). Send the `updated_at` of the analysis you edited as `base_updated_at`: if it changed since, the save is refused (`ANALYSIS_CONFLICT`, 409, `meta.updated_at` is the stored version). Without it the save overwrites whatever is there.",
    params(("id" = Uuid, Path, description = "The song ID")),
    request_body = SaveSongAnalysisPayload,
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Saved analysis.", body = SongAnalysis),
        (status = 400, description = "Invalid document."),
        (status = 403, description = "Not allowed to manage this song."),
        (status = 404, description = "Song not found."),
        (status = 409, description = "Changed since `base_updated_at` (`ANALYSIS_CONFLICT`).")
    )
)]
pub async fn save_song_analysis(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
    Json(payload): Json<SaveSongAnalysisPayload>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    payload.validate()?;
    state.song_repo.can_manage(id, user_id).await?;
    let analysis = state
        .song_analysis_repo
        .save(id, &payload.content, payload.base_updated_at, user_id)
        .await?;
    info!(%user_id, song_id = %id, "Song analysis saved");
    Ok(Json(analysis))
}

#[utoipa::path(
    delete,
    path = "/api/v1/songs/{id}/analysis",
    tags = ["Songs"],
    summary = "Delete a song's harmonic analysis.",
    description = "Same permission as editing the song. Succeeds when the song has none.",
    params(("id" = Uuid, Path, description = "The song ID")),
    security(("jwt_token" = [])),
    responses(
        (status = 204, description = "Deleted."),
        (status = 403, description = "Not allowed to manage this song."),
        (status = 404, description = "Song not found.")
    )
)]
pub async fn delete_song_analysis(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    state.song_repo.can_manage(id, user_id).await?;
    state.song_analysis_repo.delete(id).await?;
    info!(%user_id, song_id = %id, "Song analysis deleted");
    Ok(StatusCode::NO_CONTENT)
}
