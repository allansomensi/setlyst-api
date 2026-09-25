use crate::utils::rate_limit::presets;
use crate::{
    controllers::pin::{mark_one, mark_pinned},
    controllers::setlist::sync_band_copy_from,
    database::{
        AppState,
        repositories::song_repository::{BandCopyScope, SongFilter},
    },
    errors::api_error::{ApiError, codes},
    export::{
        chordpro::{chordpro_filename, render_song, render_songs},
        limiter::render_pdf,
        pdf::{
            SongExportQuery, SongPdfOptions, content_disposition, generate_song_pdf, slug_filename,
        },
    },
    import::chordpro::{ChordProError, ImportWarning, parse as parse_chordpro},
    models::{
        PaginatedResponse,
        artist::CreateArtistPayload,
        auth::access::AccessControl,
        band::BandPermission,
        link::LinkInput,
        quota::QuotaResource,
        resolve_page,
        song::{
            BandCopyStatus, CreateSongPayload, RenameTagPayload, Song, SongExport, SongListQuery,
            SongSetlistRef, TagCount, Tonality, UpdateSongPayload,
        },
    },
    services::entitlements::{Feature, ensure_feature, has_feature},
    validations::tag::{normalize_tag, normalize_tags},
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{self, LOCATION},
    },
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{debug, info};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;
use validator::Validate;

#[utoipa::path(
    get,
    path = "/api/v1/songs",
    tags = ["Songs"],
    summary = "List the caller's songs.",
    description = "Paginated personal songs (trashed ones excluded), optionally filtered by a search term (title or artist) and by tags (songs must carry *all* given tags).",
    params(SongListQuery),
    security(("jwt_token" = [])),
    responses((status = 200, description = "Songs retrieved successfully.", body = PaginatedResponse<Song>))
)]
pub async fn find_all_songs(
    State(state): State<AppState>,
    access: AccessControl,
    Query(query): Query<SongListQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    let (current_page, per_page) = resolve_page(query.page, query.per_page, 20);

    let tags = query
        .tags
        .as_deref()
        .map(|raw| raw.split(',').take(20).filter_map(normalize_tag).collect())
        .unwrap_or_default();

    let filter = SongFilter {
        search: query.q.clone().map(|q| q.chars().take(100).collect()),
        tags,
    };

    let (mut songs, total_items) = state
        .song_repo
        .find_all(user_id, &filter, current_page, per_page)
        .await?;
    mark_pinned(&state, user_id, &mut songs).await?;

    Ok(Json(PaginatedResponse::new(
        songs,
        total_items,
        current_page,
        per_page,
    )))
}

#[utoipa::path(
    get,
    path = "/api/v1/songs/{id}",
    tags = ["Songs"],
    summary = "Get a song by ID.",
    params(("id" = Uuid, Path, description = "The song ID")),
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Song retrieved successfully.", body = Song),
        (status = 404, description = "Song not found.")
    )
)]
pub async fn find_song_by_id(
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    let mut song = state
        .song_repo
        .find_by_id(id, user_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    mark_one(&state, user_id, &mut song).await?;
    Ok(Json(song))
}

#[utoipa::path(
    get,
    path = "/api/v1/songs/{id}/setlists",
    tags = ["Songs"],
    summary = "Setlists that contain a song.",
    description = "Live (not trashed) setlists the caller can see that contain the song: their personal setlists and those of their bands (the repertoire included, `is_repertoire`). Personal setlists first, then by band.",
    params(("id" = Uuid, Path, description = "The song ID")),
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Setlists.", body = [SongSetlistRef]),
        (status = 404, description = "Song not found.")
    )
)]
pub async fn find_song_setlists(
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    state
        .song_repo
        .find_by_id(id, user_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(state.song_repo.setlists_of(id, user_id).await?))
}

#[utoipa::path(
    get,
    path = "/api/v1/songs/{id}/band-copies",
    tags = ["Songs"],
    summary = "Band copies of one of the caller's songs.",
    description = "A band plays its own copy of a member's song, so editing the original never changes the band's version behind its back. This tells the member who contributed a song how each copy compares to their original (`has_updates`, `band_edited`) and whether they may update it (`can_update`, see `POST /songs/{id}/sync`).\n\nFor a personal song: its copies in the caller's bands. For a band's copy: that copy, when its original is the caller's (otherwise an empty list: other members' songs stay private).",
    params(("id" = Uuid, Path, description = "The song ID")),
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Band copies, by band name.", body = [BandCopyStatus]),
        (status = 404, description = "Song not found.")
    )
)]
pub async fn find_song_band_copies(
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    let song = state
        .song_repo
        .find_by_id(id, user_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let scope = match song.band_id {
        Some(_) => BandCopyScope::Copy(id),
        None => BandCopyScope::Source(id),
    };
    Ok(Json(state.song_repo.band_copies(user_id, scope).await?))
}

#[utoipa::path(
    get,
    path = "/api/v1/bands/{id}/song-updates",
    tags = ["Songs"],
    summary = "Band songs the caller can bring up to date.",
    description = "The band's copies of the caller's personal songs whose original has changes the copy lacks (see `GET /songs/{id}/band-copies`). Any member.",
    params(("id" = Uuid, Path, description = "The band ID")),
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Outdated band copies, by band name.", body = [BandCopyStatus]),
        (status = 404, description = "Band not found, or the caller isn't a member.")
    )
)]
pub async fn find_band_song_updates(
    Path(band_id): Path<Uuid>,
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    state
        .band_repo
        .role_of(band_id, user_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let mut copies = state
        .song_repo
        .band_copies(user_id, BandCopyScope::Band(band_id))
        .await?;
    copies.retain(|copy| copy.has_updates);
    Ok(Json(copies))
}

#[utoipa::path(
    post,
    path = "/api/v1/songs/{id}/sync",
    tags = ["Songs"],
    summary = "Update a band's copy of a song from its original.",
    description = "Replaces the band's copy's title, artist, fields, links and tags with those of the personal song it was copied from; edits the band made to its copy are replaced (`band_edited`). Only the member who contributed the song, while that original still exists (`SONG_ORIGINAL_UNAVAILABLE`, 409), with the band's `manage_songs` permission. The copy keeps its place in every setlist.",
    params(("id" = Uuid, Path, description = "The band's copy")),
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "The copy, now in step with its original.", body = BandCopyStatus),
        (status = 403, description = "Not allowed to manage the band's songs."),
        (status = 404, description = "Song not found."),
        (status = 409, description = "No original of the caller's to update from.")
    )
)]
pub async fn sync_band_song(
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    state.song_repo.can_manage(id, user_id).await?;

    let original_unavailable = || {
        ApiError::rule(
            StatusCode::CONFLICT,
            codes::SONG_ORIGINAL_UNAVAILABLE,
            "Only the member who added this song to the band can update it from their own version, while it still exists.",
        )
    };
    let status = state
        .song_repo
        .band_copies(user_id, BandCopyScope::Copy(id))
        .await?
        .pop()
        .ok_or_else(original_unavailable)?;
    let source = state
        .song_repo
        .find_with_artist_name(status.source_id)
        .await?
        .ok_or_else(original_unavailable)?;

    sync_band_copy_from(&state, status.band_id, id, &source, user_id).await?;

    let updated = state
        .song_repo
        .band_copies(user_id, BandCopyScope::Copy(id))
        .await?
        .pop()
        .ok_or(ApiError::NotFound)?;
    Ok(Json(updated))
}

#[utoipa::path(
    post,
    path = "/api/v1/songs",
    tags = ["Songs"],
    summary = "Create a new song.",
    description = "`links`: at most 5 `https` links to YouTube, Spotify, Google Drive, Apple Music, Deezer, SoundCloud, Dropbox or OneDrive (`INVALID_LINK` otherwise, `meta.url`).",
    request_body = CreateSongPayload,
    security(("jwt_token" = [])),
    responses(
        (status = 201, description = "Song created successfully.", body = Song),
        (status = 400, description = "Invalid input."),
        (status = 403, description = "Quota exceeded."),
        (status = 409, description = "A song with this title already exists for the artist.")
    )
)]
pub async fn create_song(
    State(state): State<AppState>,
    access: AccessControl,
    Json(payload): Json<CreateSongPayload>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    debug!(%user_id, "Processing request to create a new song");

    payload.validate()?;
    let tags = normalize_tags(payload.tags.as_deref().unwrap_or_default())?;
    crate::validations::link::normalize_links(payload.links.as_deref().unwrap_or_default())?;

    state.artist_repo.exists(payload.artist_id, user_id).await?;
    state
        .song_repo
        .is_unique(&payload.title, payload.artist_id, user_id, None)
        .await?;
    let quota = state
        .quota_repo
        .user_guard(user_id, QuotaResource::Songs, 1)
        .await?;
    state.quota_repo.ensure_tags(user_id, &tags).await?;

    let new_song = state
        .song_repo
        .create(&payload, &tags, user_id, &[quota])
        .await?;

    info!(%user_id, song_id = %new_song.id, "Song created successfully");

    let mut headers = HeaderMap::new();
    if let Ok(location) = HeaderValue::from_str(&format!("/api/v1/songs/{}", new_song.id)) {
        headers.insert(LOCATION, location);
    }

    Ok((StatusCode::CREATED, headers, Json(new_song)))
}

#[utoipa::path(
    patch,
    path = "/api/v1/songs/{id}",
    tags = ["Songs"],
    summary = "Update a song.",
    description = "Nullable fields (tempo, lyrics, key, genre, duration, energy, time signature, capo, tuning, performance notes) can be cleared by sending `null`. `tags` and `links`, when present, replace the whole set (`[]` clears).",
    params(("id" = Uuid, Path, description = "The song ID")),
    request_body = UpdateSongPayload,
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Song updated successfully.", body = Uuid),
        (status = 403, description = "Not allowed to manage this song."),
        (status = 404, description = "Song not found.")
    )
)]
pub async fn update_song(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
    Json(payload): Json<UpdateSongPayload>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    debug!(%user_id, song_id = %id, "Processing request to update song");

    payload.validate()?;
    let tags = payload.tags.as_deref().map(normalize_tags).transpose()?;

    state.song_repo.can_manage(id, user_id).await?;

    let existing_song = state
        .song_repo
        .find_by_id(id, user_id)
        .await?
        .ok_or(ApiError::NotFound)?;

    if existing_song.band_id.is_some() {
        // A band copy points at a band-owned artist; re-pointing it at
        // someone's personal artist would tie the band back to one
        // member's catalog.
        if payload
            .artist_id
            .is_some_and(|artist| artist != existing_song.artist_id)
        {
            return Err(ApiError::BadRequest(
                "The artist of a band's copy of a song can't be changed.".to_string(),
            ));
        }
    } else {
        if let Some(artist_id) = payload.artist_id {
            state.artist_repo.exists(artist_id, user_id).await?;
        }

        if payload.title.is_some() || payload.artist_id.is_some() {
            let title_to_check = payload.title.as_deref().unwrap_or(&existing_song.title);
            let artist_to_check = payload.artist_id.unwrap_or(existing_song.artist_id);

            state
                .song_repo
                .is_unique(title_to_check, artist_to_check, user_id, Some(id))
                .await?;
        }

        if let Some(tags) = &tags {
            state.quota_repo.ensure_tags(user_id, tags).await?;
        }
    }

    let song_id = state
        .song_repo
        .update(id, &payload, tags.as_deref(), user_id)
        .await?;

    info!(%user_id, %song_id, "Song updated successfully");
    Ok(Json(song_id))
}

#[utoipa::path(
    delete,
    path = "/api/v1/songs/{id}",
    tags = ["Songs"],
    summary = "Move a song to the trash.",
    description = "The song disappears from every list, setlist and share, and can be restored from the trash (`POST /trash/song/{id}/restore`) until it is purged.",
    params(("id" = Uuid, Path, description = "The song ID")),
    security(("jwt_token" = [])),
    responses((status = 204, description = "Song moved to the trash."))
)]
pub async fn delete_song(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();

    state.song_repo.can_manage(id, user_id).await?;
    state.song_repo.trash(id, user_id).await?;

    info!(%user_id, song_id = %id, "Song moved to the trash");
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    get,
    path = "/api/v1/songs/tags",
    tags = ["Songs"],
    summary = "List the caller's tags.",
    description = "Every tag used on the caller's personal songs, most used first.",
    security(("jwt_token" = [])),
    responses((status = 200, description = "Tags retrieved.", body = [TagCount]))
)]
pub async fn list_song_tags(
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.song_repo.list_tags(access.user_id()).await?))
}

#[utoipa::path(
    patch,
    path = "/api/v1/songs/tags/{tag}",
    tags = ["Songs"],
    summary = "Rename (or merge) a tag.",
    description = "Renames the tag on every personal song of the caller. Renaming onto an existing tag merges the two.",
    params(("tag" = String, Path, description = "The current tag")),
    request_body = RenameTagPayload,
    security(("jwt_token" = [])),
    responses((status = 200, description = "Tag renamed."))
)]
pub async fn rename_song_tag(
    State(state): State<AppState>,
    access: AccessControl,
    Path(tag): Path<String>,
    Json(payload): Json<RenameTagPayload>,
) -> Result<impl IntoResponse, ApiError> {
    payload.validate()?;

    let from = normalize_tag(&tag).ok_or(ApiError::NotFound)?;
    let to = normalize_tags(std::slice::from_ref(&payload.new_name))?
        .into_iter()
        .next()
        .ok_or_else(|| ApiError::BadRequest("The new tag name can't be empty.".to_string()))?;

    if from == to {
        return Err(ApiError::NotModified);
    }

    let affected = state
        .song_repo
        .rename_tag(access.user_id(), &from, &to)
        .await?;

    Ok(Json(json!({ "tag": to, "songs_updated": affected })))
}

#[utoipa::path(
    delete,
    path = "/api/v1/songs/tags/{tag}",
    tags = ["Songs"],
    summary = "Remove a tag from every song.",
    params(("tag" = String, Path, description = "The tag to remove")),
    security(("jwt_token" = [])),
    responses((status = 204, description = "Tag removed."))
)]
pub async fn delete_song_tag(
    State(state): State<AppState>,
    access: AccessControl,
    Path(tag): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let tag = normalize_tag(&tag).ok_or(ApiError::NotFound)?;
    state.song_repo.delete_tag(access.user_id(), &tag).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// A text download with a safe, RFC 5987 file name.
fn chordpro_response(filename: &str, body: String) -> axum::response::Response {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    if let Ok(disposition) = HeaderValue::from_str(&content_disposition(filename)) {
        headers.insert(header::CONTENT_DISPOSITION, disposition);
    }
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    (StatusCode::OK, headers, body).into_response()
}

#[utoipa::path(
    get,
    path = "/api/v1/songs/export/chordpro",
    tags = ["Songs"],
    summary = "Export all songs as ChordPro.",
    description = "Every live personal song, separated by `{new_song}`, each with `{title}`, `{artist}`, `{key}`, `{tempo}`, `{time}`, `{capo}`, `{duration: m:ss}` and `{meta: energy N}` when set. At most 10 per hour (`TOO_MANY_ATTEMPTS`, 429) and 2 bulk exports at once platform-wide (`SERVICE_BUSY`, 503). Refused under impersonation (`IMPERSONATION_READ_ONLY`).",
    security(("jwt_token" = [])),
    responses((status = 200, description = "ChordPro file.", content_type = "text/plain"))
)]
pub async fn export_songs_chordpro(
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();

    // Staff viewing an account read-only never walk away with its content.
    if access.impersonator().is_some() {
        return Err(ApiError::impersonation_read_only());
    }
    presets::limit(&presets::CHORDPRO_EXPORT, user_id)?;
    let _slot = crate::controllers::backup::bulk_slot().await?;

    let songs = state.song_repo.export_all_chordpro(user_id).await?;
    let songs_count = songs.len();
    let body = render_songs(&songs);

    let filename = format!(
        "setlyst-songs-{}.cho",
        chrono::Utc::now().format("%Y%m%d%H%M%S")
    );

    info!(%user_id, %songs_count, "Songs exported in ChordPro format");
    Ok(chordpro_response(&filename, body))
}

/// Loads a song the caller may see and export: its owner, or a member of
/// its band whose role has the band's `export_pdf` permission.
async fn exportable_song(
    state: &AppState,
    user_id: Uuid,
    id: Uuid,
) -> Result<crate::models::song::SongWithArtist, ApiError> {
    let song = state
        .song_repo
        .find_by_id(id, user_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if let Some(band_id) = song.band_id {
        state
            .band_repo
            .require_permission(band_id, user_id, BandPermission::ExportPdf)
            .await?;
    }
    state
        .song_repo
        .find_with_artist_name(id)
        .await?
        .ok_or(ApiError::NotFound)
}

#[utoipa::path(
    get,
    path = "/api/v1/songs/{id}/export/chordpro",
    tags = ["Songs"],
    summary = "Export one song as ChordPro (`.cho`).",
    params(("id" = Uuid, Path, description = "The song ID")),
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "ChordPro file.", content_type = "text/plain"),
        (status = 403, description = "Band song without the band's export permission."),
        (status = 404, description = "Song not found.")
    )
)]
pub async fn export_song_chordpro(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    // Like every other export: staff viewing the platform as someone
    // must not walk away with their content, one song at a time either.
    if access.impersonator().is_some() {
        return Err(ApiError::impersonation_read_only());
    }
    let song = exportable_song(&state, access.user_id(), id).await?;
    let export = SongExport {
        title: song.title.clone(),
        artist_name: Some(song.artist_name.clone()),
        tonality: song.tonality.and_then(|t| {
            serde_json::to_value(t)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
        }),
        tempo: song.tempo,
        lyrics: song.lyrics.clone(),
        time_signature: song.time_signature.clone(),
        capo: song.capo,
        duration: song.duration,
        energy: song.energy,
    };
    Ok(chordpro_response(
        &chordpro_filename(&song.title),
        render_song(&export),
    ))
}

#[utoipa::path(
    get,
    path = "/api/v1/songs/{id}/export/pdf",
    tags = ["Songs"],
    summary = "Export one song as a PDF sheet.",
    description = "Title, artist, a metadata line (key, capo, BPM, time signature, tuning, each toggleable), optional performance notes and the lyrics with chords (`chord_mode` hide|inline|above). Needs the plan feature `pdf_export` (`FEATURE_NOT_IN_PLAN`; `EMAIL_NOT_VERIFIED` before the e-mail is verified).\n\n**Advanced options** (plan feature `advanced_pdf`, `FEATURE_NOT_IN_PLAN` otherwise): `columns=2`, `watermark=false`, `margins` other than `normal`. Everything else is available to every plan.\n\nBand songs require the band's `export_pdf` permission. At most 3 PDFs render at once; when busy for 10 s the answer is `SERVICE_BUSY` (503, `meta.retry_after_seconds`).",
    params(("id" = Uuid, Path, description = "The song ID"), SongExportQuery),
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "PDF.", content_type = "application/pdf"),
        (status = 403, description = "Missing band permission or plan feature."),
        (status = 404, description = "Song not found."),
        (status = 503, description = "Too many PDFs being generated.")
    )
)]
pub async fn export_song_pdf(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
    Query(query): Query<SongExportQuery>,
) -> Result<axum::response::Response, ApiError> {
    if access.impersonator().is_some() {
        return Err(ApiError::impersonation_read_only());
    }
    let user_id = access.user_id();
    presets::limit(&presets::PDF_EXPORT, user_id)?;
    ensure_feature(&state, user_id, Feature::PdfExport).await?;
    let song = exportable_song(&state, user_id, id).await?;
    let options = SongPdfOptions::from(query);
    if options.is_advanced() {
        ensure_feature(&state, user_id, Feature::AdvancedPdf).await?;
    }

    let filename = slug_filename("song", &song.title, "pdf");
    let bytes = render_pdf(move || generate_song_pdf(&song, &options)).await?;
    Ok(crate::controllers::setlist::pdf_response(bytes, &filename))
}

// ---------------------------------------------------------------------
// ChordPro import
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ImportChordProQuery {
    /// `true` only parses and returns a preview; nothing is saved.
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Deserialize, Serialize, ToSchema, Validate)]
pub struct ImportChordProPayload {
    /// The file's text (at most 64 KiB, 3000 lines, 500 characters per line).
    pub content: String,
    /// An existing personal artist to attach the song to.
    pub artist_id: Option<Uuid>,
    /// Artist name (reused if the caller already has it, created
    /// otherwise). Defaults to the file's `{artist}`/`{subtitle}`.
    #[validate(length(max = 255, message = "The artist name must be at most 255 characters."))]
    pub artist_name: Option<String>,
    /// Overrides the file's `{title}`.
    #[validate(length(max = 255, message = "The title must be at most 255 characters."))]
    pub title: Option<String>,
}

/// Dry-run answer of the ChordPro import.
#[derive(Debug, Serialize, ToSchema)]
pub struct ChordProPreview {
    pub title: String,
    pub artist_name: Option<String>,
    pub tonality: Option<Tonality>,
    pub tempo: Option<i32>,
    pub time_signature: Option<String>,
    pub capo: Option<i16>,
    /// Seconds.
    pub duration: Option<i32>,
    pub lyrics: Option<String>,
    pub warnings: Vec<ImportWarning>,
}

fn chordpro_error(error: ChordProError) -> ApiError {
    match error {
        ChordProError::TooLarge => ApiError::rule(
            StatusCode::PAYLOAD_TOO_LARGE,
            codes::CHORDPRO_TOO_LARGE,
            "The ChordPro file is larger than 64 KiB.",
        ),
        ChordProError::Invalid { reason, line } => {
            let mut meta = json!({ "reason": reason });
            if let Some(line) = line {
                meta["line"] = json!(line);
            }
            ApiError::rule_with_meta(
                StatusCode::BAD_REQUEST,
                codes::CHORDPRO_INVALID,
                "The ChordPro file can't be imported.",
                meta,
            )
        }
    }
}

#[utoipa::path(
    post,
    path = "/api/v1/songs/import/chordpro",
    tags = ["Songs"],
    summary = "Import a song from a ChordPro file.",
    description = "Plan feature `chordpro_import`. With `dry_run=true` the file is only parsed and a `ChordProPreview` (with `warnings`) is returned. Otherwise the song is created in the caller's library (the artist is reused or created) and returned (201).\n\nErrors: `CHORDPRO_TOO_LARGE` (413); `CHORDPRO_INVALID` (400, `meta: {reason, line?}` with reason `nul_character`, `too_many_lines`, `line_too_long`, `multiple_songs` or `missing_title`); lyrics over the normal song limit fail with `VALIDATION_ERROR`.",
    params(ImportChordProQuery),
    request_body = ImportChordProPayload,
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Preview (dry run).", body = ChordProPreview),
        (status = 201, description = "Song created.", body = Song),
        (status = 400, description = "Invalid file."),
        (status = 403, description = "Plan feature or quota."),
        (status = 409, description = "The caller already has this song."),
        (status = 413, description = "File too large.")
    )
)]
pub async fn import_chordpro(
    State(state): State<AppState>,
    access: AccessControl,
    Query(query): Query<ImportChordProQuery>,
    Json(payload): Json<ImportChordProPayload>,
) -> Result<axum::response::Response, ApiError> {
    let user_id = access.user_id();
    payload.validate()?;
    ensure_feature(&state, user_id, Feature::ChordproImport).await?;

    let parsed =
        parse_chordpro(&payload.content, payload.title.as_deref()).map_err(chordpro_error)?;

    if let Some(lyrics) = &parsed.lyrics {
        crate::validations::text::validate_lyrics(lyrics).map_err(|e| {
            let mut errors = validator::ValidationErrors::new();
            errors.add("content", e);
            ApiError::from(errors)
        })?;
    }

    let artist_name = payload
        .artist_name
        .as_deref()
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .map(str::to_string)
        .or_else(|| parsed.artist_name.clone());

    if query.dry_run {
        let preview = ChordProPreview {
            title: parsed.title,
            artist_name,
            tonality: parsed.tonality,
            tempo: parsed.tempo,
            time_signature: parsed.time_signature,
            capo: parsed.capo,
            duration: parsed.duration,
            lyrics: parsed.lyrics,
            warnings: parsed.warnings,
        };
        return Ok((StatusCode::OK, Json(preview)).into_response());
    }

    let artist_id = match payload.artist_id {
        Some(artist_id) => {
            state.artist_repo.exists(artist_id, user_id).await?;
            artist_id
        }
        None => {
            let Some(name) = artist_name else {
                let mut error = validator::ValidationError::new("artist_required");
                error.message = Some(std::borrow::Cow::from(
                    "Choose an artist: the file doesn't name one.",
                ));
                let mut errors = validator::ValidationErrors::new();
                errors.add("artist_name", error);
                return Err(ApiError::from(errors));
            };
            let name: String = name.chars().take(255).collect();
            match state
                .artist_repo
                .find_personal_by_name(user_id, &name)
                .await?
            {
                Some(id) => id,
                None => {
                    let payload = CreateArtistPayload { name };
                    payload.validate()?;
                    let quota = state
                        .quota_repo
                        .user_guard(user_id, QuotaResource::Artists, 1)
                        .await?;
                    state
                        .artist_repo
                        .create(&payload, user_id, &[quota])
                        .await?
                        .id
                }
            }
        }
    };

    let create = CreateSongPayload {
        title: parsed.title,
        artist_id,
        tempo: parsed.tempo,
        lyrics: parsed.lyrics,
        tonality: parsed.tonality,
        genre: None,
        duration: parsed.duration,
        tags: None,
        energy: None,
        time_signature: parsed.time_signature,
        capo: parsed.capo,
        tuning: None,
        performance_notes: None,
        links: None::<Vec<LinkInput>>,
    };
    create.validate()?;
    state
        .song_repo
        .is_unique(&create.title, artist_id, user_id, None)
        .await?;
    let quota = state
        .quota_repo
        .user_guard(user_id, QuotaResource::Songs, 1)
        .await?;

    let song = state
        .song_repo
        .create(&create, &[], user_id, &[quota])
        .await?;
    info!(%user_id, song_id = %song.id, "Song imported from ChordPro");

    let mut headers = HeaderMap::new();
    if let Ok(location) = HeaderValue::from_str(&format!("/api/v1/songs/{}", song.id)) {
        headers.insert(LOCATION, location);
    }
    Ok((StatusCode::CREATED, headers, Json(song)).into_response())
}

/// Whether the caller's plan allows advanced PDF options (used by the
/// setlist export to decide between refusing and downgrading).
pub async fn allows_advanced_pdf(state: &AppState, user_id: Uuid) -> Result<bool, ApiError> {
    has_feature(state, user_id, Feature::AdvancedPdf).await
}
