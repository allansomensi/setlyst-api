use crate::{
    database::{AppState, repositories::song_repository::SongFilter},
    errors::api_error::ApiError,
    models::{
        PaginatedResponse,
        auth::access::AccessControl,
        quota::QuotaResource,
        song::{
            CreateSongPayload, RenameTagPayload, Song, SongListQuery, TagCount, UpdateSongPayload,
        },
    },
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
use serde_json::json;
use tracing::{debug, info};
use uuid::Uuid;
use validator::Validate;

#[utoipa::path(
    get,
    path = "/api/v1/songs",
    tags = ["Songs"],
    summary = "List the caller's songs.",
    description = "Paginated personal songs, optionally filtered by a search term (title or artist) and by tags (songs must carry *all* given tags).",
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
    let current_page = query.page.unwrap_or(1).max(1);
    let per_page = query.per_page.unwrap_or(20).clamp(1, 100);

    let tags = query
        .tags
        .as_deref()
        .map(|raw| raw.split(',').filter_map(normalize_tag).collect())
        .unwrap_or_default();

    let filter = SongFilter {
        search: query.q.clone(),
        tags,
    };

    let (songs, total_items) = state
        .song_repo
        .find_all(user_id, &filter, current_page, per_page)
        .await?;

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
    state
        .song_repo
        .find_by_id(id, access.user_id())
        .await?
        .map(Json)
        .ok_or(ApiError::NotFound)
}

#[utoipa::path(
    post,
    path = "/api/v1/songs",
    tags = ["Songs"],
    summary = "Create a new song.",
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

    state.artist_repo.exists(payload.artist_id, user_id).await?;
    state
        .song_repo
        .is_unique(&payload.title, payload.artist_id, user_id, None)
        .await?;
    state
        .quota_repo
        .ensure_user(user_id, QuotaResource::Songs, 1)
        .await?;
    state.quota_repo.ensure_tags(user_id, &tags).await?;

    let new_song = state.song_repo.create(&payload, &tags, user_id).await?;

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
    description = "Nullable fields can be cleared by sending `null`. `tags`, when present, replaces the whole tag set.",
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
    summary = "Delete a song.",
    params(("id" = Uuid, Path, description = "The song ID")),
    security(("jwt_token" = [])),
    responses((status = 204, description = "Song deleted successfully"))
)]
pub async fn delete_song(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();

    state.song_repo.can_manage(id, user_id).await?;
    state.song_repo.delete(id).await?;

    info!(%user_id, song_id = %id, "Song deleted successfully");
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

#[utoipa::path(
    get,
    path = "/api/v1/songs/export/chordpro",
    tags = ["Songs"],
    summary = "Export all songs as ChordPro.",
    security(("jwt_token" = [])),
    responses((status = 200, description = "ChordPro file.", content_type = "text/plain"))
)]
pub async fn export_songs_chordpro(
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();

    let songs = state.song_repo.export_all_chordpro(user_id).await?;
    let songs_count = songs.len();

    let mut chordpro_file = String::new();

    for (index, song) in songs.into_iter().enumerate() {
        if index > 0 {
            chordpro_file.push_str("\n{new_song}\n\n");
        }

        chordpro_file.push_str(&format!("{{title: {}}}\n", song.title));

        if let Some(artist) = song.artist_name {
            chordpro_file.push_str(&format!("{{artist: {artist}}}\n"));
        }

        if let Some(key) = song.tonality {
            chordpro_file.push_str(&format!("{{key: {key}}}\n"));
        }

        if let Some(tempo) = song.tempo {
            chordpro_file.push_str(&format!("{{tempo: {tempo}}}\n"));
        }

        chordpro_file.push('\n');

        match song.lyrics {
            Some(lyrics) => chordpro_file.push_str(&lyrics),
            None => chordpro_file.push_str("# No lyrics provided."),
        }

        chordpro_file.push('\n');
    }

    let filename = format!(
        "setlyst-songs-{}.cho",
        chrono::Utc::now().format("%Y%m%d%H%M%S")
    );

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );

    if let Ok(disposition) = HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
    {
        headers.insert(header::CONTENT_DISPOSITION, disposition);
    }

    info!(%user_id, %songs_count, "Songs exported in ChordPro format");

    Ok((StatusCode::OK, headers, chordpro_file))
}
