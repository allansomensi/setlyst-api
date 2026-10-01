//! The e-mail delivery console (`/admin/emails`): what was sent, what
//! failed and why, retries and a test message. Template variables are
//! never shown (they can carry one-time codes).

use crate::{
    config::Config,
    database::{AppState, repositories::audit_repository::AuditEvent},
    email::{EmailTemplate, OutgoingEmail, enqueue},
    errors::api_error::{ApiError, codes},
    models::{
        PaginatedResponse,
        admin::AdminListQuery,
        audit::actions,
        auth::access::{AccessControl, ClientIp},
        email_admin::{OutboxEmail, OutboxQuery, OutboxSummary, TestEmailResponse},
    },
    services::account,
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::{Duration, Utc};
use serde_json::json;
use uuid::Uuid;

fn not_retryable() -> ApiError {
    ApiError::rule(
        StatusCode::CONFLICT,
        codes::EMAIL_NOT_RETRYABLE,
        "This e-mail can't be sent again.",
    )
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/emails",
    tags = ["Admin"],
    summary = "The e-mail outbox, newest first (staff).",
    description = "Delivered, skipped and failed messages are kept for 30 days. `q` matches the recipient address or username.",
    params(OutboxQuery),
    security(("jwt_token" = [])),
    responses((status = 200, description = "E-mails.", body = PaginatedResponse<OutboxEmail>))
)]
pub async fn list(
    State(state): State<AppState>,
    access: AccessControl,
    Query(query): Query<OutboxQuery>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    let (page, per_page) = crate::models::resolve_page(query.page, query.per_page, 25);
    let search = AdminListQuery {
        q: query.q.clone(),
        ..Default::default()
    }
    .search_pattern();
    let (emails, total) = state
        .email_admin_repo
        .list(&query, search.as_deref(), page, per_page)
        .await?;
    Ok(Json(PaginatedResponse::new(emails, total, page, per_page)))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/emails/summary",
    tags = ["Admin"],
    summary = "How e-mail delivery is going (staff).",
    security(("jwt_token" = [])),
    responses((status = 200, description = "Summary.", body = OutboxSummary))
)]
pub async fn summary(
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    let now = Utc::now().naive_utc();
    let repo = &state.email_admin_repo;
    let (last_24h, last_7d, templates, sent_last_hour, oldest_pending_at) = tokio::try_join!(
        repo.counts_since(now - Duration::hours(24)),
        repo.counts_since(now - Duration::days(7)),
        repo.templates_since(now - Duration::days(7)),
        repo.sent_last_hour(),
        repo.oldest_pending(),
    )?;
    let config = Config::get();
    Ok(Json(OutboxSummary {
        smtp_configured: config.smtp.is_some(),
        from: config
            .smtp
            .as_ref()
            .map(|smtp| smtp.from.clone())
            .unwrap_or_default(),
        hourly_cap: config.email_hourly_cap,
        sent_last_hour,
        oldest_pending_at,
        last_24h,
        last_7d,
        templates,
    }))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/emails/{id}/retry",
    tags = ["Admin"],
    summary = "Queue a failed or skipped e-mail again (admin).",
    description = "Not for one-time codes, whose variables are wiped once they leave the queue (`EMAIL_NOT_RETRYABLE`, 409). The recipient's communication preferences are checked again when it is sent.",
    params(("id" = Uuid, Path, description = "E-mail UUID")),
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Queued.", body = OutboxEmail),
        (status = 409, description = "Not retryable."),
    )
)]
pub async fn retry(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_admin()?;
    let email = state
        .email_admin_repo
        .find(id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if !state.email_admin_repo.retry(id).await? {
        return Err(not_retryable());
    }
    AuditEvent::by(&access, actions::EMAIL_RETRIED)
        .meta(
            json!({ "email_id": id, "template": email.template, "previous_status": email.status }),
        )
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    Ok(Json(
        state
            .email_admin_repo
            .find(id)
            .await?
            .ok_or(ApiError::NotFound)?,
    ))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/emails/{id}/cancel",
    tags = ["Admin"],
    summary = "Cancel an e-mail still waiting to be sent (admin).",
    params(("id" = Uuid, Path, description = "E-mail UUID")),
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Canceled.", body = OutboxEmail),
        (status = 409, description = "Not pending anymore."),
    )
)]
pub async fn cancel(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_admin()?;
    let email = state
        .email_admin_repo
        .find(id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if !state.email_admin_repo.cancel(id).await? {
        return Err(ApiError::rule(
            StatusCode::CONFLICT,
            codes::EMAIL_NOT_RETRYABLE,
            "Only an e-mail still waiting to be sent can be canceled.",
        ));
    }
    AuditEvent::by(&access, actions::EMAIL_CANCELED)
        .meta(json!({ "email_id": id, "template": email.template }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    Ok(Json(
        state
            .email_admin_repo
            .find(id)
            .await?
            .ok_or(ApiError::NotFound)?,
    ))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/emails/test",
    tags = ["Admin"],
    summary = "Send a test e-mail to the caller's own address (staff).",
    description = "Checks SMTP delivery end to end; follow it in the list. Needs a verified address on the caller's account (`EMAIL_NOT_VERIFIED`) and a configured SMTP server (`EMAIL_NOT_CONFIGURED`, 409). Subject to the usual per-address daily cap.",
    security(("jwt_token" = [])),
    responses(
        (status = 202, description = "Queued.", body = TestEmailResponse),
        (status = 409, description = "No SMTP server configured."),
    )
)]
pub async fn send_test(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    if Config::get().smtp.is_none() {
        return Err(ApiError::rule(
            StatusCode::CONFLICT,
            codes::EMAIL_NOT_CONFIGURED,
            "No SMTP server is configured, so nothing would be delivered.",
        ));
    }
    let user = state
        .user_repo
        .find_account(access.user_id())
        .await?
        .ok_or(ApiError::NotFound)?;
    let Some(to) = user.email.clone().filter(|_| user.email_verified()) else {
        return Err(ApiError::rule(
            StatusCode::BAD_REQUEST,
            codes::EMAIL_NOT_VERIFIED,
            "Verify your own e-mail address first.",
        ));
    };
    let locale = account::user_locale(&state, user.id).await?;
    let id = enqueue(
        &state.db,
        &OutgoingEmail {
            user_id: Some(user.id),
            to: to.clone(),
            locale,
            template: EmailTemplate::TestMessage {
                username: user.username.clone(),
                requested_at: Utc::now().naive_utc(),
            },
        },
    )
    .await?;
    if id.is_nil() {
        return Err(ApiError::rule(
            StatusCode::TOO_MANY_REQUESTS,
            codes::TOO_MANY_ATTEMPTS,
            "Too many test e-mails today. Try again tomorrow.",
        ));
    }
    AuditEvent::by(&access, actions::EMAIL_TEST_SENT)
        .meta(json!({ "email_id": id }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    Ok((StatusCode::ACCEPTED, Json(TestEmailResponse { id, to })))
}
