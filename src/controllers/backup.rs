use crate::{
    database::AppState,
    errors::api_error::{ApiError, codes},
    models::{auth::access::AccessControl, backup::BackupFile},
    services::entitlements::{Feature, has_feature},
    utils::rate_limit::presets,
};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::IntoResponse,
};
use serde_json::json;
use std::{sync::LazyLock, time::Duration};
use tokio::sync::{Semaphore, SemaphorePermit};
use tracing::{debug, error, info, warn};

/// Bulk imports and exports (backups, the whole ChordPro library) running
/// at once, process-wide: each holds a pooled connection for a long time
/// and builds tens of megabytes in memory.
pub const BULK_PERMITS: usize = 2;
/// How long a bulk request waits for a free slot before `SERVICE_BUSY`.
const BULK_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(10);

static BULK_SLOTS: LazyLock<Semaphore> = LazyLock::new(|| Semaphore::new(BULK_PERMITS));

/// A slot for one bulk import or export, or `SERVICE_BUSY` (503,
/// `meta.retry_after_seconds`) once every slot stayed taken for
/// [`BULK_ACQUIRE_TIMEOUT`].
pub async fn bulk_slot() -> Result<SemaphorePermit<'static>, ApiError> {
    match tokio::time::timeout(BULK_ACQUIRE_TIMEOUT, BULK_SLOTS.acquire()).await {
        Ok(Ok(permit)) => Ok(permit),
        _ => {
            warn!("Bulk import/export slots are saturated; answering SERVICE_BUSY");
            Err(ApiError::rule_with_meta(
                StatusCode::SERVICE_UNAVAILABLE,
                codes::SERVICE_BUSY,
                "Too many imports and exports are running right now. Please try again in a few seconds.",
                json!({ "retry_after_seconds": BULK_ACQUIRE_TIMEOUT.as_secs() }),
            ))
        }
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/backup/export",
    tags = ["Backup"],
    summary = "Export a full data backup.",
    description = "Returns a downloadable JSON file (format version 2) containing the \
                   authenticated user's personal artists, songs (with energy, time signature, \
                   capo, tuning, performance notes and links), setlists (with links), gigs and \
                   tours. Items in the trash are left out. The file can be used to restore \
                   data into any account via the import endpoint.\n\n\
                   At most 10 per hour (`TOO_MANY_ATTEMPTS`, 429); at most 2 bulk imports or \
                   exports run at once platform-wide (`SERVICE_BUSY`, 503). Refused under \
                   impersonation (`IMPERSONATION_READ_ONLY`).",
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

    // Staff viewing an account read-only never walk away with its content.
    if access.impersonator().is_some() {
        return Err(ApiError::impersonation_read_only());
    }
    presets::limit(&presets::BACKUP_EXPORT, user_id)?;
    let _slot = bulk_slot().await?;

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
                   - Setlists, gigs and tours are always created as new entries.\n\
                   - Song positions inside setlists are preserved exactly.\n\n\
                   Version 1 and 2 files are accepted. Everything is validated first \
                   (lengths, ranges, links — `INVALID_LINK`; at most 10 tags per song; song \
                   positions within 0..=1000000), and every quota is enforced, including the \
                   per-setlist item limit and tours (`QUOTA_EXCEEDED`): first with every record \
                   of the file counted as new (before anything is written), then on the merged \
                   totals. Tours are only imported with the plan feature `tours`; otherwise \
                   they are skipped and counted in `skipped_tours`. The entire operation is \
                   atomic — a failure at any step leaves the account completely unchanged.\n\n\
                   Needs a verified e-mail address (`EMAIL_NOT_VERIFIED`, 403). \
                   One import per account at a time (`IMPORT_IN_PROGRESS`, 409), at most 3 \
                   per hour (`TOO_MANY_ATTEMPTS`, 429); at most 2 bulk imports or exports run at \
                   once platform-wide (`SERVICE_BUSY`, 503). Bodies up to 10 MB.",
    request_body = BackupFile,
    security(("jwt_token" = [])),
    responses(
        (status = 201, description = "Backup imported successfully.",
         body = crate::models::backup::ImportSummary),
        (status = 400, description = "Invalid or malformed backup file."),
        (status = 401, description = "Unauthorized."),
        (status = 403, description = "Quota exceeded, or e-mail not verified."),
        (status = 409, description = "Another import is running for this account (`IMPORT_IN_PROGRESS`)."),
        (status = 429, description = "Too many imports."),
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
        gigs = payload.gigs.len(),
        "Processing request to import backup"
    );

    crate::services::account::require_verified_email(&state, user_id).await?;
    presets::limit(&presets::BACKUP_IMPORT, user_id)?;

    let limits = state.quota_repo.effective_limits(user_id).await?;
    let allow_tours =
        payload.tours.is_empty() || has_feature(&state, user_id, Feature::Tours).await?;

    let _slot = bulk_slot().await?;
    match state
        .backup_repo
        .import(user_id, payload, limits, allow_tours)
        .await
    {
        Ok(summary) => {
            info!(
                %user_id,
                artists_imported = summary.artists_imported,
                songs_imported = summary.songs_imported,
                setlists_imported = summary.setlists_imported,
                gigs_imported = summary.gigs_imported,
                tours_imported = summary.tours_imported,
                skipped_tours = summary.skipped_tours,
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
