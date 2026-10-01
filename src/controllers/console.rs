//! The staff console's home and tools that span the platform: what needs
//! attention (`/admin/overview`), search (`/admin/search`), CSV exports
//! and bulk actions on accounts.

use crate::{
    controllers::user,
    database::{
        AppState,
        repositories::audit_repository::{AuditEvent, STAFF_VIEW_DEDUPE_SECONDS},
    },
    errors::api_error::ApiError,
    models::{
        admin::AdminListQuery,
        audit::{AuditLogQuery, actions},
        auth::access::{AccessControl, ClientIp},
        console::{
            BulkUserAction, BulkUserFailure, BulkUserPayload, BulkUserResult, ConsoleOverview,
            ConsoleSearchQuery, ConsoleSearchResults,
        },
        user::{BanUserPayload, Status, UpdateUserPayload, UserListQuery},
    },
    services::account,
    utils::csv::push_row,
};
use axum::{
    Json,
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use chrono::{NaiveDateTime, Utc};
use serde_json::json;
use tracing::info;
use validator::Validate;

/// Rows a CSV export holds at most.
pub const MAX_EXPORT_ROWS: i64 = 50_000;

#[utoipa::path(
    get,
    path = "/api/v1/admin/overview",
    tags = ["Admin"],
    summary = "What needs attention right now (staff).",
    description = "Accounts (totals, sign-ups, activity, suspensions, staff without 2FA), sign-ups per day for 30 days, the support queue, open moderation flags, e-mail delivery problems, active incidents and the platform switches.",
    security(("jwt_token" = [])),
    responses((status = 200, description = "Overview.", body = ConsoleOverview))
)]
pub async fn overview(
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    let repo = &state.console_repo;
    let (
        users,
        signups,
        support,
        moderation_open,
        emails_failed_24h,
        emails_pending,
        oldest_pending,
        incidents_active,
        platform,
    ) = tokio::try_join!(
        repo.user_counts(),
        repo.signups(30),
        state.support_repo.summary(access.user_id()),
        repo.moderation_open(),
        state.email_admin_repo.failed_last_day(),
        repo.emails_pending(),
        state.email_admin_repo.oldest_pending(),
        state.incident_repo.count_active(),
        state.platform_repo.get(),
    )?;
    Ok(Json(ConsoleOverview {
        maintenance_mode: platform.maintenance.mode,
        registrations_open: platform.registrations_open,
        users,
        signups,
        support_open: support.open,
        support_unassigned: support.unassigned,
        support_urgent: support.urgent,
        moderation_open,
        emails_failed_24h,
        emails_pending,
        oldest_pending_email_at: oldest_pending,
        incidents_active,
        generated_at: Utc::now().naive_utc(),
    }))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/search",
    tags = ["Admin"],
    summary = "Search accounts, bands, songs, setlists and support requests at once (staff).",
    description = "`q` of at least 2 characters; up to 5 matches of each kind. `#1042` (or `1042`) finds a support request by number. Recorded in the audit log like an account search.",
    params(ConsoleSearchQuery),
    security(("jwt_token" = [])),
    responses((status = 200, description = "Matches.", body = ConsoleSearchResults))
)]
pub async fn search(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Query(query): Query<ConsoleSearchQuery>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    let q = query.q.trim();
    if q.chars().count() < 2 {
        return Ok(Json(ConsoleSearchResults::default()));
    }
    let Some(pattern) = (AdminListQuery {
        q: Some(q.to_string()),
        ..Default::default()
    })
    .search_pattern() else {
        return Ok(Json(ConsoleSearchResults::default()));
    };
    let number = q.trim_start_matches('#').parse::<i64>().ok();

    // Searching accounts (by e-mail, names) is staff access to personal
    // data: recorded, with the search masked when it is an address.
    let mut event = AuditEvent::by(&access, actions::STAFF_CONTENT_VIEWED)
        .meta(json!({ "view": "console_search", "q": account::mask_identifier_for_staff(q) }))
        .ip(&ip.0)
        .once_within(STAFF_VIEW_DEDUPE_SECONDS);
    event.target_type = Some("user");
    event.spawn(state.audit_repo.clone());

    Ok(Json(state.console_repo.search(&pattern, number).await?))
}

/// A CSV download: UTF-8 with a BOM (so spreadsheets read the accents),
/// never cached.
fn csv_download(name: &str, body: String) -> Response {
    let file_name = format!("{name}-{}.csv", Utc::now().format("%Y%m%d-%H%M%S"));
    let mut response = Response::new(Body::from(format!("\u{FEFF}{body}")));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/csv; charset=utf-8"),
    );
    if let Ok(value) = HeaderValue::from_str(&format!("attachment; filename=\"{file_name}\"")) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn timestamp(value: Option<NaiveDateTime>) -> String {
    value
        .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default()
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/users/export",
    tags = ["Admin"],
    summary = "Export the accounts as CSV (admin).",
    description = "Takes the filters of `GET /users` (`q`, `role`, `state`, `verified`, `two_factor`, `created_from`, `created_to`, `sort`); at most 50 000 rows. Recorded in the audit log.",
    params(UserListQuery),
    security(("jwt_token" = [])),
    responses((status = 200, description = "CSV file.", content_type = "text/csv"))
)]
pub async fn export_users(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Query(query): Query<UserListQuery>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_admin()?;
    if access.impersonator().is_some() {
        return Err(ApiError::impersonation_read_only());
    }
    let users = state
        .user_repo
        .export(&query.filter(), MAX_EXPORT_ROWS)
        .await?;
    let mut out = String::with_capacity(users.len() * 160);
    push_row(
        &mut out,
        [
            "id",
            "username",
            "email",
            "email_verified",
            "first_name",
            "last_name",
            "role",
            "status",
            "suspended",
            "suspended_until",
            "two_factor",
            "created_at",
            "last_login_at",
        ],
    );
    for user in &users {
        push_row(
            &mut out,
            [
                user.id.to_string(),
                user.username.clone(),
                user.email.clone().unwrap_or_default(),
                user.email_verified.to_string(),
                user.first_name.clone().unwrap_or_default(),
                user.last_name.clone().unwrap_or_default(),
                format!("{:?}", user.role).to_lowercase(),
                format!("{:?}", user.status).to_lowercase(),
                user.is_banned.to_string(),
                timestamp(user.banned_until),
                user.two_factor_enabled.to_string(),
                timestamp(Some(user.created_at)),
                timestamp(user.last_login_at),
            ],
        );
    }
    AuditEvent::by(&access, actions::STAFF_DATA_EXPORTED)
        .meta(json!({ "kind": "users", "rows": users.len() }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    info!(rows = users.len(), by = %access.user_id(), "Accounts exported");
    Ok(csv_download("setlyst-users", out))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/audit-logs/export",
    tags = ["Admin"],
    summary = "Export the audit log as CSV (admin).",
    description = "Takes the filters of `GET /admin/audit-logs`; newest first, at most 50 000 rows. Recorded in the audit log.",
    params(AuditLogQuery),
    security(("jwt_token" = [])),
    responses((status = 200, description = "CSV file.", content_type = "text/csv"))
)]
pub async fn export_audit_logs(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Query(query): Query<AuditLogQuery>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_admin()?;
    if access.impersonator().is_some() {
        return Err(ApiError::impersonation_read_only());
    }
    let (entries, _) = state.audit_repo.list(&query, 1, MAX_EXPORT_ROWS).await?;
    let mut out = String::with_capacity(entries.len() * 200);
    push_row(
        &mut out,
        [
            "id",
            "created_at",
            "action",
            "actor_id",
            "actor_username",
            "impersonator_id",
            "target_type",
            "target_id",
            "target_label",
            "ip_address",
            "metadata",
        ],
    );
    for entry in &entries {
        push_row(
            &mut out,
            [
                entry.id.to_string(),
                timestamp(Some(entry.created_at)),
                entry.action.clone(),
                entry.actor_id.map(|id| id.to_string()).unwrap_or_default(),
                entry.actor_username.clone().unwrap_or_default(),
                entry
                    .impersonator_id
                    .map(|id| id.to_string())
                    .unwrap_or_default(),
                entry.target_type.clone().unwrap_or_default(),
                entry.target_id.map(|id| id.to_string()).unwrap_or_default(),
                entry.target_label.clone().unwrap_or_default(),
                entry.ip_address.clone().unwrap_or_default(),
                entry.metadata.to_string(),
            ],
        );
    }
    AuditEvent::by(&access, actions::STAFF_DATA_EXPORTED)
        .meta(json!({ "kind": "audit_logs", "rows": entries.len() }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    Ok(csv_download("setlyst-audit-log", out))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/users/bulk",
    tags = ["Admin"],
    summary = "Apply one action to several accounts (staff).",
    description = "`action`: `revoke_sessions`, `ban` (with `duration_hours` and `reason`), `unban`, `deactivate` or `activate`, for 1 to 100 accounts. Each account goes through the same rules as the single action (the caller must outrank it, never themselves); the ones refused are listed in `failed` with their error code, the rest are applied. Every account gets its own audit entry, plus one for the batch.",
    request_body = BulkUserPayload,
    security(("jwt_token" = [])),
    responses((status = 200, description = "Outcome per account.", body = BulkUserResult))
)]
pub async fn bulk_users(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Json(payload): Json<BulkUserPayload>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    payload.validate()?;
    let mut ids = payload.user_ids.clone();
    ids.sort();
    ids.dedup();

    let mut succeeded = Vec::new();
    let mut failed = Vec::new();
    for id in ids {
        let outcome = match payload.action {
            BulkUserAction::RevokeSessions => user::revoke_user_sessions(
                State(state.clone()),
                access.clone(),
                ip.clone(),
                Path(id),
            )
            .await
            .map(|_| ()),
            BulkUserAction::Ban => user::ban_user(
                State(state.clone()),
                access.clone(),
                ip.clone(),
                Path(id),
                Json(BanUserPayload {
                    duration_hours: payload.duration_hours,
                    reason: payload.reason.clone(),
                }),
            )
            .await
            .map(|_| ()),
            BulkUserAction::Unban => {
                user::unban_user(State(state.clone()), access.clone(), ip.clone(), Path(id))
                    .await
                    .map(|_| ())
            }
            BulkUserAction::Deactivate | BulkUserAction::Activate => user::update_user(
                State(state.clone()),
                access.clone(),
                ip.clone(),
                Path(id),
                Json(UpdateUserPayload {
                    status: Some(if payload.action == BulkUserAction::Deactivate {
                        Status::Inactive
                    } else {
                        Status::Active
                    }),
                    ..Default::default()
                }),
            )
            .await
            .map(|_| ()),
        };
        match outcome {
            Ok(()) => succeeded.push(id),
            Err(e) => failed.push(BulkUserFailure {
                user_id: id,
                code: e.code().to_string(),
            }),
        }
    }

    AuditEvent::by(&access, actions::USER_BULK_ACTION)
        .meta(json!({
            "action": payload.action,
            "succeeded": succeeded.len(),
            "failed": failed.len(),
        }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    Ok((StatusCode::OK, Json(BulkUserResult { succeeded, failed })))
}
