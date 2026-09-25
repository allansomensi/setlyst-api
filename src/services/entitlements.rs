//! Plan entitlements: which features an account may use.
//!
//! See [`AccessTier`]: staff (admins and moderators) always have every
//! feature. While subscriptions are not enforced (the platform setting
//! `billing.enforced`, the beta), every verified account has everything.
//! Once they are, a feature is available when the account's effective plan
//! (a live subscription whose period hasn't ended) includes it, and
//! accounts without a plan get the free tier ([`FREE_FEATURES`]). Accounts
//! that haven't verified their e-mail address (and have no plan) only get
//! [`UNVERIFIED_FEATURES`], in the beta too.

use crate::{
    database::{AppState, repositories::billing_repository::load_effective_plan},
    errors::api_error::{ApiError, codes},
    models::{
        billing::{AccessTier, Plan},
        user::Role,
    },
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
    PdfExport,
    AdvancedPdf,
    ChordproImport,
    SongSuggestions,
    PublicSharing,
    OfflineMode,
    PrioritySupport,
}

impl Feature {
    pub const ALL: [Feature; 10] = [
        Feature::CreateBands,
        Feature::Tours,
        Feature::AnalyticsExport,
        Feature::PdfExport,
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
            Feature::PdfExport => "pdf_export",
            Feature::AdvancedPdf => "advanced_pdf",
            Feature::ChordproImport => "chordpro_import",
            Feature::SongSuggestions => "song_suggestions",
            Feature::PublicSharing => "public_sharing",
            Feature::OfflineMode => "offline_mode",
            Feature::PrioritySupport => "priority_support",
        }
    }
}

/// Features of verified accounts without a plan once plans are enforced:
/// one band (see `QuotaLimits::FREE`) and collaborating in bands; no
/// tours, public links, PDF or report exports.
pub const FREE_FEATURES: &[Feature] = &[
    Feature::CreateBands,
    Feature::SongSuggestions,
    Feature::OfflineMode,
];

/// Features of accounts that haven't verified their e-mail address.
pub const UNVERIFIED_FEATURES: &[Feature] = &[Feature::OfflineMode];

/// What an account is entitled to right now.
#[derive(Debug, Clone)]
pub struct Entitlements {
    pub enforced: bool,
    pub is_staff: bool,
    pub email_verified: bool,
    pub plan: Option<Plan>,
}

impl Entitlements {
    pub fn tier(&self) -> AccessTier {
        AccessTier::resolve(
            self.is_staff,
            self.enforced,
            self.plan.is_some(),
            self.email_verified,
        )
    }

    pub fn has(&self, feature: Feature) -> bool {
        match self.tier() {
            AccessTier::Staff | AccessTier::Beta => true,
            AccessTier::Plan => self.plan.as_ref().is_some_and(|p| p.has(feature)),
            AccessTier::Unverified => UNVERIFIED_FEATURES.contains(&feature),
            AccessTier::Free => FREE_FEATURES.contains(&feature),
        }
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
    let row: Option<(Role, bool)> = sqlx::query_as(
        "SELECT role, (email_verified_at IS NOT NULL AND email IS NOT NULL) FROM users WHERE id = $1",
    )
    .bind(user_id)
    .fetch_optional(&state.db)
    .await?;
    let plan = load_effective_plan(&state.db, user_id).await?;
    Ok(Entitlements {
        enforced: settings.enforced,
        is_staff: row.as_ref().is_some_and(|(role, _)| role.is_staff()),
        email_verified: row.is_some_and(|(_, verified)| verified),
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

/// `true` while the owner of a setlist or gig may keep it public: the
/// public views answer 404 once the owner's plan no longer includes
/// public sharing (the hourly job also revokes those links).
pub async fn owner_can_share(state: &AppState, owner_id: Uuid) -> Result<bool, ApiError> {
    has_feature(state, owner_id, Feature::PublicSharing).await
}

/// `true` while a shared setlist or gig (created by `creator_id`, in
/// `band_id` when it belongs to a band) may still be viewed through its
/// public link: its creator's plan includes public sharing or, for band
/// content, the band owner's plan does. The public views answer 404
/// otherwise. Links of personal content are also cleared by the billing
/// jobs; band content is only covered by this check.
pub async fn shared_content_visible(
    state: &AppState,
    creator_id: Uuid,
    band_id: Option<Uuid>,
) -> Result<bool, ApiError> {
    if !state.billing_repo.get_settings().await?.enforced {
        return Ok(true);
    }
    if owner_can_share(state, creator_id).await? {
        return Ok(true);
    }
    let Some(band_id) = band_id else {
        return Ok(false);
    };
    let band_owner: Option<Uuid> = sqlx::query_scalar(
        "SELECT user_id FROM band_members WHERE band_id = $1 AND role = 'owner' LIMIT 1",
    )
    .bind(band_id)
    .fetch_optional(&state.db)
    .await?;
    match band_owner {
        Some(owner) if owner != creator_id => owner_can_share(state, owner).await,
        _ => Ok(false),
    }
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
    let entitlements = entitlements(state, user_id).await?;
    if entitlements.has(feature) {
        return Ok(());
    }
    if entitlements.tier() == AccessTier::Unverified {
        return Err(ApiError::rule_with_meta(
            StatusCode::FORBIDDEN,
            codes::EMAIL_NOT_VERIFIED,
            "Verify your e-mail address to use this feature.",
            json!({ "feature": feature.key() }),
        ));
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
    fn entitlements_follow_tier() {
        let base = Entitlements {
            enforced: false,
            is_staff: false,
            email_verified: true,
            plan: None,
        };
        // The beta: everything.
        assert_eq!(base.tier(), AccessTier::Beta);
        assert!(base.has(Feature::Tours));

        // Unverified, beta or not: almost nothing.
        let unverified = Entitlements {
            email_verified: false,
            ..base.clone()
        };
        assert_eq!(unverified.tier(), AccessTier::Unverified);
        assert!(!unverified.has(Feature::CreateBands));
        assert!(unverified.has(Feature::OfflineMode));

        // Enforced without a plan: the free tier.
        let free = Entitlements {
            enforced: true,
            ..base.clone()
        };
        assert_eq!(free.tier(), AccessTier::Free);
        assert!(free.has(Feature::CreateBands));
        assert!(!free.has(Feature::Tours));
        assert!(!free.has(Feature::PdfExport));
        assert!(!free.has(Feature::AnalyticsExport));

        // Staff: everything, whatever else holds.
        let staff = Entitlements {
            enforced: true,
            is_staff: true,
            email_verified: false,
            plan: None,
        };
        assert_eq!(staff.tier(), AccessTier::Staff);
        assert!(staff.features().values().all(|v| *v));
    }
}
