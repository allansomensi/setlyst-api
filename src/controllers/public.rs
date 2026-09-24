//! Public endpoints (no session): legal version, plans, release notes and
//! e-mail unsubscribe.

use crate::{
    database::{
        AppState,
        repositories::audit_repository::{AuditEvent, record_legal_acceptances},
    },
    email::unsubscribe,
    errors::api_error::ApiError,
    models::{
        audit::{LegalAcceptance, actions, legal_documents},
        auth::access::ClientIp,
        billing::PublicPlan,
        communication::{Category, UnsubscribeInfo, UnsubscribePayload, UnsubscribeQuery},
        release_note::ReleaseNote,
        user::CURRENT_TERMS_VERSION,
    },
    services::account::user_agent,
};
use axum::{
    Json,
    extract::{Query, State},
    http::HeaderMap,
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::info;
use utoipa::ToSchema;
use validator::Validate;

/// Release notes returned publicly, at most.
const PUBLIC_RELEASE_NOTES: i64 = 50;

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct LegalVersion {
    /// Version of the Terms of Use / Privacy Policy in force.
    pub version: String,
}

#[utoipa::path(
    get,
    path = "/api/v1/public/legal/version",
    tags = ["Public"],
    summary = "Version of the legal documents in force.",
    responses((status = 200, description = "Current version.", body = LegalVersion))
)]
pub async fn legal_version() -> impl IntoResponse {
    Json(LegalVersion {
        version: CURRENT_TERMS_VERSION.to_string(),
    })
}

#[utoipa::path(
    get,
    path = "/api/v1/public/plans",
    tags = ["Public"],
    summary = "Public plans with their running promotions.",
    description = "Plans marked public, in display order. `promotion` is the best running promotion for the plan (plan-specific, or one for every paid plan).",
    responses((status = 200, description = "Plans.", body = [PublicPlan]))
)]
pub async fn list_plans(State(state): State<AppState>) -> Result<impl IntoResponse, ApiError> {
    let plans = state.billing_repo.list_plans(true).await?;
    let promotions = state.billing_repo.running_promotions().await?;
    let body: Vec<PublicPlan> = plans
        .into_iter()
        .map(|plan| {
            let paid = plan.price_monthly_cents > 0 || plan.price_yearly_cents > 0;
            // Already ordered by discount, best first.
            let promotion = promotions
                .iter()
                .find(|(code, _)| match code {
                    Some(code) => *code == plan.code,
                    None => paid,
                })
                .map(|(_, promotion)| promotion.clone());
            PublicPlan {
                code: plan.code,
                name: plan.name,
                description: plan.description,
                price_monthly_cents: plan.price_monthly_cents,
                price_yearly_cents: plan.price_yearly_cents,
                currency: plan.currency,
                limits: plan.limits,
                features: plan.features,
                highlighted: plan.highlighted,
                sort_order: plan.sort_order,
                promotion,
            }
        })
        .collect();
    Ok(Json(body))
}

#[utoipa::path(
    get,
    path = "/api/v1/public/release-notes",
    tags = ["Public"],
    summary = "Published release notes.",
    description = "Newest `released_on` first, at most 50. Drafts are never listed.",
    responses((status = 200, description = "Release notes.", body = [ReleaseNote]))
)]
pub async fn list_release_notes(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(
        state
            .release_note_repo
            .list_published(PUBLIC_RELEASE_NOTES)
            .await?,
    ))
}

#[utoipa::path(
    get,
    path = "/api/v1/public/email/unsubscribe",
    tags = ["Public"],
    summary = "Inspect an unsubscribe link.",
    description = "Tells the unsubscribe page which category a token refers to. Invalid tokens answer `{ valid: false, category: null }` (not an error).",
    params(UnsubscribeQuery),
    responses((status = 200, description = "Token details.", body = UnsubscribeInfo))
)]
pub async fn inspect_unsubscribe(Query(query): Query<UnsubscribeQuery>) -> impl IntoResponse {
    let parsed = (query.token.len() <= 200)
        .then(|| unsubscribe::parse(&query.token))
        .flatten();
    Json(UnsubscribeInfo {
        category: parsed.map(|(_, category)| category),
        valid: parsed.is_some(),
    })
}

#[utoipa::path(
    post,
    path = "/api/v1/public/email/unsubscribe",
    tags = ["Public"],
    summary = "Unsubscribe from a category of e-mails.",
    description = "Turns off e-mails of the token's category for its account (the in-app setting is kept). Invalid tokens answer `BAD_REQUEST`. Rate-limited per IP.",
    request_body = UnsubscribePayload,
    responses(
        (status = 200, description = "Unsubscribed.", body = UnsubscribeInfo),
        (status = 400, description = "Invalid token."),
    )
)]
pub async fn unsubscribe(
    State(state): State<AppState>,
    ip: ClientIp,
    headers: HeaderMap,
    Json(payload): Json<UnsubscribePayload>,
) -> Result<impl IntoResponse, ApiError> {
    payload.validate()?;
    Ok(Json(
        apply_unsubscribe(&state, &payload.token, ip, &headers).await?,
    ))
}

#[utoipa::path(
    post,
    path = "/api/v1/public/email/unsubscribe/one-click",
    tags = ["Public"],
    summary = "One-click unsubscribe (RFC 8058).",
    description = "The target of the `List-Unsubscribe` header of e-mails that can be unsubscribed from: mail providers POST `List-Unsubscribe=One-Click` (form-encoded) here when the recipient uses their own unsubscribe button. The token is taken from the query string; any body is ignored. Same effect as `POST /public/email/unsubscribe`. Invalid tokens answer `BAD_REQUEST`. Rate-limited per IP.",
    params(UnsubscribeQuery),
    responses(
        (status = 200, description = "Unsubscribed.", body = UnsubscribeInfo),
        (status = 400, description = "Invalid token."),
    )
)]
pub async fn unsubscribe_one_click(
    State(state): State<AppState>,
    ip: ClientIp,
    headers: HeaderMap,
    Query(query): Query<UnsubscribeQuery>,
) -> Result<impl IntoResponse, ApiError> {
    if query.token.len() > 200 {
        return Err(invalid_unsubscribe_link());
    }
    Ok(Json(
        apply_unsubscribe(&state, &query.token, ip, &headers).await?,
    ))
}

fn invalid_unsubscribe_link() -> ApiError {
    ApiError::BadRequest("This unsubscribe link is not valid.".into())
}

/// Switches e-mails of the token's category off for its account (the
/// in-app setting is kept). A change is audited as
/// `user.communication_changed` (source `unsubscribe_link`), and turning
/// marketing e-mail off is recorded in the consent ledger as a withdrawal
/// of the `marketing_email` consent (LGPD art. 8, § 5).
async fn apply_unsubscribe(
    state: &AppState,
    token: &str,
    ip: ClientIp,
    headers: &HeaderMap,
) -> Result<UnsubscribeInfo, ApiError> {
    let Some((user_id, category)) = unsubscribe::parse(token) else {
        return Err(invalid_unsubscribe_link());
    };
    let Some(user) = state.user_repo.find_by_id(user_id).await? else {
        return Err(invalid_unsubscribe_link());
    };
    let (mut prefs, _) = state.user_prefs_repo.get_communication(user_id).await?;
    let mut channel = prefs.get(category);
    let was_on = channel.email;
    channel.email = false;
    prefs.set(category, channel);
    state
        .user_prefs_repo
        .set_communication(user_id, &prefs)
        .await?;
    info!(%user_id, category = category.key(), "Unsubscribed from e-mails");

    if was_on {
        AuditEvent::new(actions::USER_COMMUNICATION_CHANGED)
            .actor(user_id, &user.username)
            .target("user", user_id, &user.username)
            .meta(json!({ "categories": [category.key()], "source": "unsubscribe_link" }))
            .ip(&ip.0)
            .record(&*state.audit_repo)
            .await;
        if category == Category::Marketing {
            record_legal_acceptances(
                &*state.audit_repo,
                &[LegalAcceptance {
                    user_id,
                    document: legal_documents::MARKETING_EMAIL,
                    version: CURRENT_TERMS_VERSION.to_string(),
                    accepted: false,
                    source: "unsubscribe_link",
                    ip_address: ip.0,
                    user_agent: user_agent(headers),
                }],
            )
            .await;
        }
    }
    Ok(UnsubscribeInfo {
        category: Some(category),
        valid: true,
    })
}
