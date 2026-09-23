//! Plan entitlements: which features an account may use.
//!
//! While subscriptions are not enforced (the platform setting
//! `billing.enforced`), every account is entitled to everything. Once they
//! are, a feature is available when the account's effective plan (a live
//! subscription whose period hasn't ended) includes it. Admins always have
//! every feature.

use crate::{
    database::{AppState, repositories::billing_repository::load_effective_plan},
    errors::api_error::{ApiError, codes},
    models::{billing::Plan, user::Role},
};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use utoipa::ToSchema;
use uuid::Uuid;

/// A feature that plans can include or leave out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Feature {
    CreateBands,
    Tours,
    AnalyticsExport,
    AdvancedPdf,
    ChordproImport,
    SongSuggestions,
    PublicSharing,
    OfflineMode,
    PrioritySupport,
}

impl Feature {
    pub const ALL: [Feature; 9] = [
        Feature::CreateBands,
        Feature::Tours,
        Feature::AnalyticsExport,
        Feature::AdvancedPdf,
        Feature::ChordproImport,
        Feature::SongSuggestions,
        Feature::PublicSharing,
        Feature::OfflineMode,
        Feature::PrioritySupport,
    ];

    pub fn key(&self) -> &'static str {
        match self {
            Feature::CreateBands => "create_bands",
            Feature::Tours => "tours",
            Feature::AnalyticsExport => "analytics_export",
            Feature::AdvancedPdf => "advanced_pdf",
            Feature::ChordproImport => "chordpro_import",
            Feature::SongSuggestions => "song_suggestions",
            Feature::PublicSharing => "public_sharing",
            Feature::OfflineMode => "offline_mode",
            Feature::PrioritySupport => "priority_support",
        }
    }
}

/// What an account is entitled to right now.
#[derive(Debug, Clone)]
pub struct Entitlements {
    pub enforced: bool,
    pub is_admin: bool,
    pub plan: Option<Plan>,
}

impl Entitlements {
    pub fn has(&self, feature: Feature) -> bool {
        !self.enforced || self.is_admin || self.plan.as_ref().is_some_and(|p| p.has(feature))
    }

    /// Every known feature with its availability.
    pub fn features(&self) -> BTreeMap<String, bool> {
        Feature::ALL
            .iter()
            .map(|f| (f.key().to_string(), self.has(*f)))
            .collect()
    }
}

/// Loads the entitlements of `user_id`.
pub async fn entitlements(state: &AppState, user_id: Uuid) -> Result<Entitlements, ApiError> {
    let settings = state.billing_repo.get_settings().await?;
    let role: Option<Role> = sqlx::query_scalar("SELECT role FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(&state.db)
        .await?;
    let plan = load_effective_plan(&state.db, user_id).await?;
    Ok(Entitlements {
        enforced: settings.enforced,
        is_admin: role == Some(Role::Admin),
        plan,
    })
}

/// `true` when `user_id` may use `feature`.
pub async fn has_feature(
    state: &AppState,
    user_id: Uuid,
    feature: Feature,
) -> Result<bool, ApiError> {
    Ok(entitlements(state, user_id).await?.has(feature))
}

/// The cheapest public plan that includes `feature` (to point the user
/// at an upgrade).
pub async fn cheapest_plan_with(
    state: &AppState,
    feature: Feature,
) -> Result<Option<String>, ApiError> {
    let plans = state.billing_repo.list_plans(true).await?;
    Ok(plans
        .into_iter()
        .filter(|p| p.has(feature))
        .min_by_key(|p| (p.price_monthly_cents, p.sort_order))
        .map(|p| p.code))
}

/// Fails with `FEATURE_NOT_IN_PLAN` (403, meta `{feature, plan}`) when
/// `user_id` may not use `feature`. `plan` is the cheapest plan that
/// includes it.
pub async fn ensure_feature(
    state: &AppState,
    user_id: Uuid,
    feature: Feature,
) -> Result<(), ApiError> {
    if has_feature(state, user_id, feature).await? {
        return Ok(());
    }
    let plan = cheapest_plan_with(state, feature).await?;
    Err(ApiError::rule_with_meta(
        StatusCode::FORBIDDEN,
        codes::FEATURE_NOT_IN_PLAN,
        "Your current plan does not include this feature.",
        json!({ "feature": feature.key(), "plan": plan }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entitlements_follow_enforcement_role_and_plan() {
        let open = Entitlements {
            enforced: false,
            is_admin: false,
            plan: None,
        };
        assert!(open.has(Feature::Tours));
        let enforced = Entitlements {
            enforced: true,
            is_admin: false,
            plan: None,
        };
        assert!(!enforced.has(Feature::Tours));
        assert!(enforced.features().values().all(|v| !v));
        let admin = Entitlements {
            enforced: true,
            is_admin: true,
            plan: None,
        };
        assert!(admin.has(Feature::PrioritySupport));
    }
}
