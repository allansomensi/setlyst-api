//! A setlist, a gig or a tour as a file someone else can import: the
//! backup format (`kind: "setlist" | "gig" | "tour"`) holding it and
//! everything it needs — a gig its setlist, a tour its gigs and their
//! setlists, and every setlist its songs and their artists.

use crate::{
    controllers::backup::bulk_slot,
    database::AppState,
    errors::api_error::ApiError,
    export::pdf::{content_disposition, slug_filename},
    models::{
        auth::access::AccessControl,
        backup::{BackupFile, BackupGig, BackupKind, BackupTour, ImportSummary},
        gig::Gig,
        setlist::{Setlist, SetlistItem},
    },
    services::entitlements::{Feature, ensure_feature},
    utils::rate_limit::presets,
};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::IntoResponse,
};
use std::collections::HashSet;
use tracing::{debug, error, info};
use uuid::Uuid;

/// A JSON download (`attachment`, RFC 5987 file name).
fn json_attachment(filename: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    if let Ok(disposition) = HeaderValue::from_str(&content_disposition(filename)) {
        headers.insert(header::CONTENT_DISPOSITION, disposition);
    }
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers
}

/// What every export checks first: never under impersonation, and within
/// the shared per-account limit.
fn start_export(access: &AccessControl) -> Result<Uuid, ApiError> {
    if access.impersonator().is_some() {
        return Err(ApiError::impersonation_read_only());
    }
    let user_id = access.user_id();
    presets::limit(&presets::SHARED_EXPORT, user_id)?;
    Ok(user_id)
}

/// The setlist with its running order, if the caller may take it out of
/// where it lives (whoever may export it to PDF: its owner, its
/// collaborators, band members with the band's `export_pdf` permission).
/// `None` when they may not, so a gig or tour goes without it.
async fn exportable_setlist(
    state: &AppState,
    setlist_id: Uuid,
    user_id: Uuid,
) -> Result<Option<(Setlist, Vec<SetlistItem>)>, ApiError> {
    match state.setlist_repo.can_export_pdf(setlist_id, user_id).await {
        Ok(()) => {}
        Err(ApiError::NotFound | ApiError::Forbidden) => return Ok(None),
        Err(e) => return Err(e),
    }
    let Some(setlist) = state.setlist_repo.find_by_id(setlist_id, user_id).await? else {
        return Ok(None);
    };
    let items = state
        .setlist_repo
        .get_items_capped(setlist_id, crate::controllers::setlist::MAX_LISTED_ITEMS)
        .await?;
    Ok(Some((setlist, items)))
}

fn backup_gig(gig: &Gig, setlist_id: Option<Uuid>, tour_id: Option<Uuid>) -> BackupGig {
    BackupGig {
        id: gig.id,
        venue: gig.venue.clone(),
        scheduled_at: gig.scheduled_at,
        setlist_id,
        status: gig.status,
        notes: gig.notes.clone(),
        location: gig.location.clone(),
        tour_id,
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/setlists/{id}/export",
    tags = ["Setlists"],
    summary = "Export a setlist as a file.",
    description = "Returns a downloadable JSON file (the backup format, `kind: \"setlist\"`) holding \
                   the setlist — title, description, links, blocks, breaks and the key each song \
                   is played in — with every song in it (lyrics, chords, BPM, key, tags, notes, \
                   links and harmonic analysis) and their artists. Imported with \
                   `POST /setlists/import`, it gives any account the same setlist, even one that \
                   has none of its songs or artists.\n\n\
                   Allowed to whoever may export the setlist to PDF (its owner, its \
                   collaborators, band members with the band's `export_pdf` permission). At most \
                   30 setlist, gig and tour files per hour (`TOO_MANY_ATTEMPTS`, 429). Refused \
                   under impersonation (`IMPERSONATION_READ_ONLY`).",
    params(("id" = Uuid, Path, description = "The ID of the setlist")),
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "The setlist file.", content_type = "application/json", body = BackupFile),
        (status = 403, description = "Not allowed to export this setlist."),
        (status = 404, description = "Setlist not found."),
        (status = 429, description = "Too many exports.")
    )
)]
pub async fn export_setlist(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = start_export(&access)?;
    debug!(%user_id, setlist_id = %id, "Processing request to export setlist file");

    // Errors as they are here (403, 404): the setlist is the whole file.
    state.setlist_repo.can_export_pdf(id, user_id).await?;
    let (setlist, items) = exportable_setlist(&state, id, user_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let filename = slug_filename("setlist", &setlist.title, "setlyst.json");
    let file = state
        .backup_repo
        .export_shared(
            BackupKind::Setlist,
            vec![(setlist, items)],
            Vec::new(),
            Vec::new(),
        )
        .await?;

    info!(%user_id, setlist_id = %id, songs = file.songs.len(), "Setlist file exported");
    Ok((StatusCode::OK, json_attachment(&filename), Json(file)))
}

#[utoipa::path(
    get,
    path = "/api/v1/gigs/{id}/export",
    tags = ["Gigs"],
    summary = "Export a gig as a file.",
    description = "Returns a downloadable JSON file (the backup format, `kind: \"gig\"`) holding the \
                   gig — venue, location, date, status and notes — and its setlist, with the \
                   setlist's songs and artists as in `GET /setlists/{id}/export`. Imported with \
                   `POST /gigs/import`, it gives any account the same gig and setlist.\n\n\
                   Allowed to whoever can see the gig; its setlist is included when they may \
                   export it too (otherwise the gig comes without one). The gig's tour isn't \
                   part of it (export the tour for that). Same limits as the setlist file.",
    params(("id" = Uuid, Path, description = "The ID of the gig")),
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "The gig file.", content_type = "application/json", body = BackupFile),
        (status = 404, description = "Gig not found."),
        (status = 429, description = "Too many exports.")
    )
)]
pub async fn export_gig(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = start_export(&access)?;
    debug!(%user_id, gig_id = %id, "Processing request to export gig file");

    let gig = state
        .gig_repo
        .find_by_id(id, user_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let setlist = match gig.setlist_id {
        Some(setlist_id) => exportable_setlist(&state, setlist_id, user_id).await?,
        None => None,
    };
    let setlist_id = setlist.as_ref().map(|(s, _)| s.id);
    let filename = slug_filename("gig", &gig.venue, "setlyst.json");
    let file = state
        .backup_repo
        .export_shared(
            BackupKind::Gig,
            setlist.into_iter().collect(),
            vec![backup_gig(&gig, setlist_id, None)],
            Vec::new(),
        )
        .await?;

    info!(%user_id, gig_id = %id, songs = file.songs.len(), "Gig file exported");
    Ok((StatusCode::OK, json_attachment(&filename), Json(file)))
}

#[utoipa::path(
    get,
    path = "/api/v1/tours/{id}/export",
    tags = ["Tours"],
    summary = "Export a tour as a file.",
    description = "Returns a downloadable JSON file (the backup format, `kind: \"tour\"`) holding the \
                   tour — name, description and dates — with all of its gigs and their setlists \
                   (each written once, however many gigs play it), with the setlists' songs and \
                   artists as in `GET /setlists/{id}/export`. Imported with `POST /tours/import`, \
                   it gives any account the same tour.\n\n\
                   Allowed to whoever can see the tour; each setlist is included when they may \
                   export it (otherwise its gigs come without one). Same limits as the setlist \
                   file.",
    params(("id" = Uuid, Path, description = "The ID of the tour")),
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "The tour file.", content_type = "application/json", body = BackupFile),
        (status = 404, description = "Tour not found."),
        (status = 429, description = "Too many exports.")
    )
)]
pub async fn export_tour(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = start_export(&access)?;
    debug!(%user_id, tour_id = %id, "Processing request to export tour file");

    let tour = state
        .tour_repo
        .find_by_id(id, user_id)
        .await?
        .ok_or(ApiError::NotFound)?;

    let mut gigs = Vec::new();
    let mut setlists = Vec::new();
    let mut included: HashSet<Uuid> = HashSet::new();
    let mut refused: HashSet<Uuid> = HashSet::new();
    // The tour's gigs in their order, read in one statement rather than
    // one lookup per gig.
    let summaries = state.tour_repo.gigs(id).await?;
    let gig_ids: Vec<Uuid> = summaries.iter().map(|s| s.id).collect();
    let mut by_id: std::collections::HashMap<Uuid, _> = state
        .gig_repo
        .find_many(&gig_ids, user_id)
        .await?
        .into_iter()
        .map(|gig| (gig.id, gig))
        .collect();
    for summary in summaries {
        let Some(gig) = by_id.remove(&summary.id) else {
            continue;
        };
        let mut setlist_id = None;
        if let Some(wanted) = gig.setlist_id
            && !refused.contains(&wanted)
        {
            if included.contains(&wanted) {
                setlist_id = Some(wanted);
            } else if let Some(bundle) = exportable_setlist(&state, wanted, user_id).await? {
                included.insert(wanted);
                setlists.push(bundle);
                setlist_id = Some(wanted);
            } else {
                refused.insert(wanted);
            }
        }
        gigs.push(backup_gig(&gig, setlist_id, Some(tour.id)));
    }

    let filename = slug_filename("tour", &tour.name, "setlyst.json");
    let file = state
        .backup_repo
        .export_shared(
            BackupKind::Tour,
            setlists,
            gigs,
            vec![BackupTour {
                id: tour.id,
                name: tour.name.clone(),
                description: tour.description.clone(),
                start_date: tour.start_date,
                end_date: tour.end_date,
            }],
        )
        .await?;

    info!(
        %user_id,
        tour_id = %id,
        gigs = file.gigs.len(),
        setlists = file.setlists.len(),
        "Tour file exported"
    );
    Ok((StatusCode::OK, json_attachment(&filename), Json(file)))
}

/// Imports a shared file of `kind` into the caller's personal content.
async fn import_shared(
    state: AppState,
    access: AccessControl,
    payload: BackupFile,
    kind: BackupKind,
) -> Result<(StatusCode, Json<ImportSummary>), ApiError> {
    let user_id = access.user_id();
    debug!(
        %user_id,
        ?kind,
        songs = payload.songs.len(),
        gigs = payload.gigs.len(),
        "Processing request to import a shared file"
    );

    payload.check_shared(kind).map_err(ApiError::BadRequest)?;
    crate::services::account::require_verified_email(&state, user_id).await?;
    presets::limit(&presets::SHARED_IMPORT, user_id)?;
    // A tour file is the tour: without the plan feature there is nothing
    // to import it as.
    if kind == BackupKind::Tour {
        ensure_feature(&state, user_id, Feature::Tours).await?;
    }

    let limits = state.quota_repo.effective_limits(user_id).await?;
    let _slot = bulk_slot().await?;
    match state
        .backup_repo
        .import(user_id, payload, limits, kind == BackupKind::Tour)
        .await
    {
        Ok(summary) => {
            info!(
                %user_id,
                ?kind,
                songs_imported = summary.songs_imported,
                setlist_ids = ?summary.setlist_ids,
                gig_ids = ?summary.gig_ids,
                tour_ids = ?summary.tour_ids,
                "Shared file imported"
            );
            Ok((StatusCode::CREATED, Json(summary)))
        }
        Err(e) => {
            error!(%user_id, ?kind, error = %e, "Failed to import shared file");
            Err(e)
        }
    }
}

#[utoipa::path(
    post,
    path = "/api/v1/setlists/import",
    tags = ["Setlists"],
    summary = "Import a setlist file.",
    description = "Accepts a file from `GET /setlists/{id}/export` (`kind: \"setlist\"`, one \
                   setlist; anything else is `400`) and creates the setlist with its blocks, \
                   breaks and keys. `setlist_ids` in the answer holds it.\n\n\
                   See `POST /tours/import` for the merge rules and limits.",
    request_body = BackupFile,
    security(("jwt_token" = [])),
    responses(
        (status = 201, description = "Setlist imported.", body = ImportSummary),
        (status = 400, description = "Invalid file, or not a setlist file."),
        (status = 403, description = "Quota exceeded, or e-mail not verified."),
        (status = 409, description = "Another import is running for this account."),
        (status = 429, description = "Too many imports.")
    )
)]
pub async fn import_setlist(
    State(state): State<AppState>,
    access: AccessControl,
    Json(payload): Json<BackupFile>,
) -> Result<impl IntoResponse, ApiError> {
    import_shared(state, access, payload, BackupKind::Setlist).await
}

#[utoipa::path(
    post,
    path = "/api/v1/gigs/import",
    tags = ["Gigs"],
    summary = "Import a gig file.",
    description = "Accepts a file from `GET /gigs/{id}/export` (`kind: \"gig\"`: one gig and its \
                   setlist, if any; anything else is `400`) and creates the gig with its setlist, \
                   linked. `gig_ids` and `setlist_ids` in the answer hold them.\n\n\
                   See `POST /tours/import` for the merge rules and limits.",
    request_body = BackupFile,
    security(("jwt_token" = [])),
    responses(
        (status = 201, description = "Gig imported.", body = ImportSummary),
        (status = 400, description = "Invalid file, or not a gig file."),
        (status = 403, description = "Quota exceeded, or e-mail not verified."),
        (status = 409, description = "Another import is running for this account."),
        (status = 429, description = "Too many imports.")
    )
)]
pub async fn import_gig(
    State(state): State<AppState>,
    access: AccessControl,
    Json(payload): Json<BackupFile>,
) -> Result<impl IntoResponse, ApiError> {
    import_shared(state, access, payload, BackupKind::Gig).await
}

#[utoipa::path(
    post,
    path = "/api/v1/tours/import",
    tags = ["Tours"],
    summary = "Import a tour file.",
    description = concat!(
        "Accepts a file from `GET /tours/{id}/export` (`kind: \"tour\"`: one tour, its gigs and \
         their setlists; anything else is `400`) and creates the tour with its gigs, each linked \
         to its setlist. `tour_ids`, `gig_ids` and `setlist_ids` in the answer hold them. Needs \
         the plan feature `tours` (`FEATURE_NOT_AVAILABLE`, 403).\n\n",
        "Same merge rules as a backup import: artists already in the account under the same name \
         are reused, and so are songs with the same title, artist and version name (keeping their \
         own lyrics and analysis); everything else is created, in the caller's personal content. \
         A setlist title already taken gets a suffix (\"Show (2)\").\n\n\
         Quotas apply (`QUOTA_EXCEEDED`), and the whole import is atomic. Needs a verified e-mail \
         address (`EMAIL_NOT_VERIFIED`, 403); at most 10 setlist, gig and tour imports per hour \
         (`TOO_MANY_ATTEMPTS`, 429); `IMPORT_IN_PROGRESS` (409) and `SERVICE_BUSY` (503) as for \
         backups. Bodies up to 10 MB."
    ),
    request_body = BackupFile,
    security(("jwt_token" = [])),
    responses(
        (status = 201, description = "Tour imported.", body = ImportSummary),
        (status = 400, description = "Invalid file, or not a tour file."),
        (status = 403, description = "Quota exceeded, plan without tours, or e-mail not verified."),
        (status = 409, description = "Another import is running for this account."),
        (status = 429, description = "Too many imports.")
    )
)]
pub async fn import_tour(
    State(state): State<AppState>,
    access: AccessControl,
    Json(payload): Json<BackupFile>,
) -> Result<impl IntoResponse, ApiError> {
    import_shared(state, access, payload, BackupKind::Tour).await
}
