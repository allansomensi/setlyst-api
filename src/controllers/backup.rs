use crate::{
    database::AppState,
    errors::api_error::ApiError,
    models::{auth::access::AccessControl, backup::BackupFile},
};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::IntoResponse,
};
use tracing::{debug, error, info};

#[utoipa::path(
    get,
    path = "/api/v1/backup/export",
    tags = ["Backup"],
    summary = "Export a full data backup.",
    description = "Returns a downloadable JSON file containing all artists, songs, and \
                   setlists belonging to the authenticated user. The file can be used to \
                   restore data into any account via the import endpoint.",
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Backup file generated successfully.",
         content_type = "application/json",
         body = BackupFile),
        (status = 401, description = "Unauthorized."),
        (status = 500, description = "An error occurred while generating the backup.")
    )
)]
pub async fn export_backup(
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();

    debug!(%user_id, "Processing request to export backup");

    match state.backup_repo.export(user_id).await {
        Ok(backup) => {
            let filename = format!(
                "setlyst-backup-{}.json",
                chrono::Utc::now().format("%Y%m%d%H%M%S")
            );

            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json; charset=utf-8"),
            );
            if let Ok(disposition) =
                HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
            {
                headers.insert(header::CONTENT_DISPOSITION, disposition);
            }

            info!(
                %user_id,
                artists = backup.artists.len(),
                songs = backup.songs.len(),
                setlists = backup.setlists.len(),
                "Backup exported successfully"
            );

            Ok((StatusCode::OK, headers, Json(backup)))
        }
        Err(e) => {
            error!(%user_id, error = %e, "Failed to export backup");
            Err(e)
        }
    }
}

#[utoipa::path(
    post,
    path = "/api/v1/backup/import",
    tags = ["Backup"],
    summary = "Import a backup file.",
    description = "Accepts a JSON backup (as produced by the export endpoint) and imports \
                   all artists, songs, and setlists into the authenticated user's account.\n\n\
                   **Merge rules:**\n\
                   - Artists already present under the same name are reused, not duplicated.\n\
                   - Songs already present with the same title and artist are reused.\n\
                   - Setlists are always created as new entries.\n\
                   - Song positions inside setlists are preserved exactly.\n\n\
                   The entire operation is atomic — a failure at any step leaves the \
                   account completely unchanged.",
    request_body = BackupFile,
    security(("jwt_token" = [])),
    responses(
        (status = 201, description = "Backup imported successfully.",
         body = crate::models::backup::ImportSummary),
        (status = 400, description = "Invalid or malformed backup file."),
        (status = 401, description = "Unauthorized."),
        (status = 500, description = "An error occurred while importing the backup.")
    )
)]
pub async fn import_backup(
    State(state): State<AppState>,
    access: AccessControl,
    Json(payload): Json<BackupFile>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();

    debug!(
        %user_id,
        backup_version = payload.version,
        artists = payload.artists.len(),
        songs = payload.songs.len(),
        setlists = payload.setlists.len(),
        "Processing request to import backup"
    );

    match state.backup_repo.import(user_id, payload).await {
        Ok(summary) => {
            info!(
                %user_id,
                artists_imported = summary.artists_imported,
                songs_imported = summary.songs_imported,
                setlists_imported = summary.setlists_imported,
                "Backup imported successfully"
            );
            Ok((StatusCode::CREATED, Json(summary)))
        }
        Err(e) => {
            error!(%user_id, error = %e, "Failed to import backup");
            Err(e)
        }
    }
}
