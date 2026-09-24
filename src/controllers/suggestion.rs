//! Song suggestions and votes (`/bands/{id}/suggestions`).

use crate::{
    controllers::setlist::band_song_for,
    database::{AppState, repositories::suggestion_repository::NewSuggestion},
    errors::api_error::{ApiError, codes},
    models::{
        PaginatedResponse,
        auth::access::AccessControl,
        band::{BandPermission, BandRole},
        notification::Notification,
        resolve_page,
        suggestion::{
            CreateSuggestionPayload, ResolveSuggestionPayload, Suggestion, SuggestionListQuery,
            SuggestionRow, SuggestionStatus, VotePayload, reaches_auto_accept,
        },
    },
    services::{
        entitlements::{Feature, ensure_feature},
        notifier::notify,
    },
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use tracing::{error, info, warn};
use uuid::Uuid;
use validator::Validate;

/// Suggestions one member may make to one band in 24 hours (each one
/// notifies every member).
pub const MAX_SUGGESTIONS_PER_DAY: i64 = 20;

fn clean_note(note: Option<&str>) -> Option<String> {
    note.map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_string)
}

async fn load(
    state: &AppState,
    band_id: Uuid,
    id: Uuid,
    user_id: Uuid,
) -> Result<SuggestionRow, ApiError> {
    state
        .suggestion_repo
        .find(id, band_id, user_id)
        .await?
        .ok_or(ApiError::NotFound)
}

async fn band_name(state: &AppState, band_id: Uuid) -> Result<String, ApiError> {
    Ok(state
        .band_repo
        .find_any(band_id)
        .await?
        .map(|b| b.name)
        .unwrap_or_default())
}

/// Tells the suggester how their suggestion ended (never when they closed
/// it themselves).
async fn notify_resolution(
    state: &AppState,
    row: &SuggestionRow,
    status: SuggestionStatus,
    resolver: Option<Uuid>,
) -> Result<(), ApiError> {
    if let Some(suggester) = row.suggested_by
        && Some(suggester) != resolver
    {
        let name = band_name(state, row.band_id).await?;
        notify(
            state,
            Notification::band_suggestion_resolved(
                suggester,
                row.band_id,
                &name,
                row.id,
                &row.song_title,
                status.key(),
            ),
        )
        .await;
    }
    Ok(())
}

/// Accepts an open suggestion: the song is copied into the band when it
/// is a member's personal song (same rules and quotas as adding it to a
/// band setlist), added to the target setlist (and so to the repertoire),
/// and the suggestion is closed. `actor_id` is recorded as the creator of
/// a new band copy; `resolved_by` is `None` for an automatic acceptance.
///
/// The suggestion is claimed first (`open` → `accepted` in one guarded
/// statement), so of a concurrent accept, reject, withdraw or automatic
/// acceptance exactly one wins and the others get `SUGGESTION_CLOSED`.
/// The copy and the setlist insert that follow are idempotent (the band
/// copy is unique per source song, a setlist holds a song once); if they
/// fail, the claim is undone and the suggestion is open again.
async fn accept(
    state: &AppState,
    row: &SuggestionRow,
    actor_id: Uuid,
    resolved_by: Option<Uuid>,
    note: Option<&str>,
) -> Result<(), ApiError> {
    if row.status != SuggestionStatus::Open {
        return Err(crate::database::repositories::suggestion_repository::suggestion_closed());
    }
    let song_id = row.live_song_id.ok_or(ApiError::NotFound)?;

    state
        .suggestion_repo
        .resolve(row.id, SuggestionStatus::Accepted, resolved_by, note)
        .await?;
    if let Err(e) = add_suggested_song(state, row, song_id, actor_id).await {
        if let Err(reopen) = state.suggestion_repo.reopen(row.id).await {
            error!(suggestion_id = %row.id, error = %reopen, "Could not reopen a suggestion after a failed acceptance");
        }
        return Err(e);
    }

    notify_resolution(state, row, SuggestionStatus::Accepted, resolved_by).await?;
    info!(suggestion_id = %row.id, band_id = %row.band_id, automatic = resolved_by.is_none(), "Song suggestion accepted");
    Ok(())
}

/// The effect of an accepted suggestion: the band's copy of the song in
/// the target setlist (and the repertoire). Safe to run concurrently.
async fn add_suggested_song(
    state: &AppState,
    row: &SuggestionRow,
    song_id: Uuid,
    actor_id: Uuid,
) -> Result<(), ApiError> {
    let setlist = state
        .setlist_repo
        .find_any(row.setlist_id)
        .await?
        .filter(|s| s.band_id == Some(row.band_id))
        .ok_or(ApiError::NotFound)?;
    let mut source = state
        .song_repo
        .find_with_artist_name(song_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    // A suggested personal song is forked for the band without its
    // performance notes: they're the suggester's private notes, and the
    // whole band can read the copy.
    if source.band_id.is_none() {
        source.performance_notes = None;
    }

    let band_song = band_song_for(state, row.band_id, &source, actor_id).await?;
    if !state.setlist_repo.has_song(setlist.id, band_song).await? {
        let quota = if setlist.is_repertoire {
            None
        } else {
            Some(state.quota_repo.setlist_items_guard(setlist.id, 1).await?)
        };
        state
            .setlist_repo
            .add_song(setlist.id, band_song, quota.as_slice())
            .await?;
        state.setlist_repo.touch(setlist.id, actor_id).await?;
    }
    Ok(())
}

#[utoipa::path(
    get,
    path = "/api/v1/bands/{id}/suggestions",
    tags = ["Bands"],
    summary = "List a band's song suggestions.",
    description = "Any member. `status`: `open` (default), `accepted`, `rejected`, `withdrawn` or `all`. Newest first.",
    params(("id" = Uuid, Path, description = "The ID of the band"), SuggestionListQuery),
    security(("jwt_token" = [])),
    responses((status = 200, description = "Suggestions.", body = PaginatedResponse<Suggestion>))
)]
pub async fn list_suggestions(
    State(state): State<AppState>,
    access: AccessControl,
    Path(band_id): Path<Uuid>,
    Query(query): Query<SuggestionListQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    let (page, per_page) = resolve_page(query.page, query.per_page, 20);
    state
        .band_repo
        .require_role(band_id, user_id, BandRole::Member)
        .await?;
    let (items, total) = state
        .suggestion_repo
        .list(
            band_id,
            user_id,
            query.status.unwrap_or_default().status(),
            page,
            per_page,
        )
        .await?;
    Ok(Json(PaginatedResponse::new(items, total, page, per_page)))
}

#[utoipa::path(
    post,
    path = "/api/v1/bands/{id}/suggestions",
    tags = ["Bands"],
    summary = "Suggest a song to the band.",
    description = "Any member, plan feature `song_suggestions`. The song must be one of the caller's personal songs or a song the band already owns; the target setlist must be one of the band's (default: the repertoire). `SONG_ALREADY_IN_SETLIST` (409) when the target already has it, `ALREADY_EXISTS` (409) when the same song is already open for that setlist. The suggester's up-vote is recorded (shown in `votes_up`, but never counted towards automatic acceptance) and the other members are notified. At most 20 suggestions per member and band in 24 hours (`TOO_MANY_ATTEMPTS`, 429).",
    params(("id" = Uuid, Path, description = "The ID of the band")),
    request_body = CreateSuggestionPayload,
    security(("jwt_token" = [])),
    responses(
        (status = 201, description = "Suggestion created.", body = Suggestion),
        (status = 403, description = "Plan feature."),
        (status = 404, description = "Band, song or setlist not found."),
        (status = 409, description = "Already in the setlist, or already suggested.")
    )
)]
pub async fn create_suggestion(
    State(state): State<AppState>,
    access: AccessControl,
    Path(band_id): Path<Uuid>,
    Json(payload): Json<CreateSuggestionPayload>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    payload.validate()?;
    state
        .band_repo
        .require_role(band_id, user_id, BandRole::Member)
        .await?;
    ensure_feature(&state, user_id, Feature::SongSuggestions).await?;

    let song = state
        .song_repo
        .find_with_artist_name(payload.song_id)
        .await?
        .filter(|s| match s.band_id {
            None => s.user_id == user_id,
            Some(owner) => owner == band_id,
        })
        .ok_or(ApiError::NotFound)?;

    let setlist_id = match payload.setlist_id {
        Some(id) => id,
        None => state
            .setlist_repo
            .find_repertoire_id(band_id)
            .await?
            .ok_or(ApiError::NotFound)?,
    };
    let setlist = state
        .setlist_repo
        .find_by_id(setlist_id, user_id)
        .await?
        .filter(|s| s.band_id == Some(band_id))
        .ok_or(ApiError::NotFound)?;

    // The band's copy of a personal song may already be in the setlist.
    let in_setlist = match song.band_id {
        Some(_) => state.setlist_repo.has_song(setlist.id, song.id).await?,
        None => match state.song_repo.find_band_fork(band_id, song.id).await? {
            Some(fork) => state.setlist_repo.has_song(setlist.id, fork).await?,
            None => false,
        },
    };
    if in_setlist {
        return Err(ApiError::rule(
            StatusCode::CONFLICT,
            codes::SONG_ALREADY_IN_SETLIST,
            "This song is already in that setlist.",
        ));
    }

    // Each suggestion notifies every member: bounded per member and band
    // (counted in the insert's transaction, see the repository).
    let note = clean_note(payload.note.as_deref());
    let id = state
        .suggestion_repo
        .create(
            &NewSuggestion {
                band_id,
                setlist_id: setlist.id,
                song_id: song.id,
                song_title: &song.title,
                artist_name: &song.artist_name,
                suggested_by: user_id,
                note: note.as_deref(),
            },
            MAX_SUGGESTIONS_PER_DAY,
        )
        .await?;
    info!(%user_id, %band_id, suggestion_id = %id, "Song suggested");

    let name = band_name(&state, band_id).await?;
    let username: String = sqlx::query_scalar("SELECT username FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_one(&state.db)
        .await?;
    for member in state.band_member_repo.list(band_id).await? {
        if member.user_id != user_id {
            notify(
                &state,
                Notification::band_suggestion_created(
                    member.user_id,
                    band_id,
                    &name,
                    id,
                    &song.title,
                    &username,
                ),
            )
            .await;
        }
    }

    // Never accepted on creation: the suggester's own up-vote doesn't
    // count towards automatic acceptance (see `vote_suggestion`).
    let row = load(&state, band_id, id, user_id).await?;
    Ok((StatusCode::CREATED, Json(Suggestion::from(row))))
}

#[utoipa::path(
    put,
    path = "/api/v1/bands/{id}/suggestions/{sid}/vote",
    tags = ["Bands"],
    summary = "Vote on a suggestion.",
    description = "Any member; `value` 1 or -1 (replaces the caller's previous vote). Open suggestions only (`SUGGESTION_CLOSED`). When the band's `suggestion_auto_accept_votes` is set and the up-votes of current members other than the suggester reach it (with more up than down), the suggestion is accepted automatically. Returns the suggestion.",
    params(
        ("id" = Uuid, Path, description = "The ID of the band"),
        ("sid" = Uuid, Path, description = "The suggestion ID")
    ),
    request_body = VotePayload,
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Suggestion.", body = Suggestion),
        (status = 409, description = "Not open.")
    )
)]
pub async fn vote_suggestion(
    State(state): State<AppState>,
    access: AccessControl,
    Path((band_id, id)): Path<(Uuid, Uuid)>,
    Json(payload): Json<VotePayload>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    payload.validate()?;
    state
        .band_repo
        .require_role(band_id, user_id, BandRole::Member)
        .await?;
    load(&state, band_id, id, user_id).await?;

    let (up, down) = state
        .suggestion_repo
        .vote(id, user_id, payload.value)
        .await?;

    let threshold = state.band_repo.suggestion_threshold(band_id).await?;
    if reaches_auto_accept(threshold, up, down) {
        let row = load(&state, band_id, id, user_id).await?;
        if let Err(e) = accept(&state, &row, user_id, None, None).await {
            warn!(suggestion_id = %id, error = %e, "Automatic acceptance failed; the suggestion stays open");
        }
    }

    Ok(Json(Suggestion::from(
        load(&state, band_id, id, user_id).await?,
    )))
}

#[utoipa::path(
    delete,
    path = "/api/v1/bands/{id}/suggestions/{sid}/vote",
    tags = ["Bands"],
    summary = "Remove the caller's vote.",
    params(
        ("id" = Uuid, Path, description = "The ID of the band"),
        ("sid" = Uuid, Path, description = "The suggestion ID")
    ),
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Suggestion.", body = Suggestion),
        (status = 409, description = "Not open.")
    )
)]
pub async fn unvote_suggestion(
    State(state): State<AppState>,
    access: AccessControl,
    Path((band_id, id)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    state
        .band_repo
        .require_role(band_id, user_id, BandRole::Member)
        .await?;
    load(&state, band_id, id, user_id).await?;
    state.suggestion_repo.remove_vote(id, user_id).await?;
    Ok(Json(Suggestion::from(
        load(&state, band_id, id, user_id).await?,
    )))
}

#[utoipa::path(
    post,
    path = "/api/v1/bands/{id}/suggestions/{sid}/accept",
    tags = ["Bands"],
    summary = "Accept a suggestion.",
    description = "Requires the band's `manage_setlists` permission. The song is copied into the band if needed (band song quota), added to the target setlist and to the repertoire. The suggester is notified.",
    params(
        ("id" = Uuid, Path, description = "The ID of the band"),
        ("sid" = Uuid, Path, description = "The suggestion ID")
    ),
    request_body = ResolveSuggestionPayload,
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Suggestion.", body = Suggestion),
        (status = 403, description = "No permission, or quota."),
        (status = 404, description = "Not found (or its song was deleted)."),
        (status = 409, description = "Not open.")
    )
)]
pub async fn accept_suggestion(
    State(state): State<AppState>,
    access: AccessControl,
    Path((band_id, id)): Path<(Uuid, Uuid)>,
    Json(payload): Json<ResolveSuggestionPayload>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    payload.validate()?;
    state
        .band_repo
        .require_permission(band_id, user_id, BandPermission::ManageSetlists)
        .await?;
    let row = load(&state, band_id, id, user_id).await?;
    let note = clean_note(payload.note.as_deref());
    accept(&state, &row, user_id, Some(user_id), note.as_deref()).await?;
    Ok(Json(Suggestion::from(
        load(&state, band_id, id, user_id).await?,
    )))
}

#[utoipa::path(
    post,
    path = "/api/v1/bands/{id}/suggestions/{sid}/reject",
    tags = ["Bands"],
    summary = "Reject a suggestion.",
    description = "Requires the band's `manage_setlists` permission. The suggester is notified.",
    params(
        ("id" = Uuid, Path, description = "The ID of the band"),
        ("sid" = Uuid, Path, description = "The suggestion ID")
    ),
    request_body = ResolveSuggestionPayload,
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Suggestion.", body = Suggestion),
        (status = 409, description = "Not open.")
    )
)]
pub async fn reject_suggestion(
    State(state): State<AppState>,
    access: AccessControl,
    Path((band_id, id)): Path<(Uuid, Uuid)>,
    Json(payload): Json<ResolveSuggestionPayload>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    payload.validate()?;
    state
        .band_repo
        .require_permission(band_id, user_id, BandPermission::ManageSetlists)
        .await?;
    let row = load(&state, band_id, id, user_id).await?;
    let note = clean_note(payload.note.as_deref());
    state
        .suggestion_repo
        .resolve(
            id,
            SuggestionStatus::Rejected,
            Some(user_id),
            note.as_deref(),
        )
        .await?;
    notify_resolution(&state, &row, SuggestionStatus::Rejected, Some(user_id)).await?;
    Ok(Json(Suggestion::from(
        load(&state, band_id, id, user_id).await?,
    )))
}

#[utoipa::path(
    post,
    path = "/api/v1/bands/{id}/suggestions/{sid}/withdraw",
    tags = ["Bands"],
    summary = "Withdraw one's own suggestion.",
    params(
        ("id" = Uuid, Path, description = "The ID of the band"),
        ("sid" = Uuid, Path, description = "The suggestion ID")
    ),
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Suggestion.", body = Suggestion),
        (status = 403, description = "Not the suggester."),
        (status = 409, description = "Not open.")
    )
)]
pub async fn withdraw_suggestion(
    State(state): State<AppState>,
    access: AccessControl,
    Path((band_id, id)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    state
        .band_repo
        .require_role(band_id, user_id, BandRole::Member)
        .await?;
    let row = load(&state, band_id, id, user_id).await?;
    if row.suggested_by != Some(user_id) {
        return Err(ApiError::Forbidden);
    }
    state
        .suggestion_repo
        .resolve(id, SuggestionStatus::Withdrawn, Some(user_id), None)
        .await?;
    Ok(Json(Suggestion::from(
        load(&state, band_id, id, user_id).await?,
    )))
}
