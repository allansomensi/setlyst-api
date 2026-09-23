//! Plans, subscriptions, credits, referrals, promo codes and promotions.
//!
//! Reads and simple writes live here; multi-step operations that must be
//! atomic (granting plan time, redeeming codes and rewards) are in
//! `services::billing`, which runs them in one transaction.

use crate::{
    errors::api_error::ApiError,
    models::billing::{
        BILLING_SETTINGS_KEY, BillingOverview, BillingSettings, CreatePromoCodePayload,
        CreatePromotionPayload, CreditEntry, Plan, PlanPromotion, PlanRow, PromoCode,
        PromoRedemption, Promotion, ReferralEntry, Subscription, SubscriptionEvent,
        UpdatePromoCodePayload, UpdatePromotionPayload, UpsertPlanPayload, trim_localized,
    },
};
use chrono::{NaiveDateTime, Utc};
use serde_json::{Value, json};
use sqlx::{PgExecutor, PgPool};
use std::collections::BTreeMap;
use uuid::Uuid;

fn now() -> NaiveDateTime {
    Utc::now().naive_utc()
}

/// Columns of a [`PlanRow`].
macro_rules! plan_columns {
    () => {
        "code, name, description, price_monthly_cents, price_yearly_cents, currency, limits,
         features, highlighted, is_public, sort_order, updated_at"
    };
}

/// Columns of a [`Subscription`] from `subscriptions`.
macro_rules! subscription_columns {
    () => {
        "plan_code, status, source, started_at, current_period_end, trial_ends_at, cancel_at_period_end"
    };
}

/// Columns of a [`PromoCode`] from `promo_codes p`.
macro_rules! promo_columns {
    () => {
        "p.id, p.code, p.description, p.kind, p.plan_code, p.duration_days, p.credits,
         p.discount_percent, p.max_redemptions, p.redemptions_count, p.new_users_only,
         p.starts_at, p.expires_at, p.disabled_at,
         (SELECT x.username FROM users x WHERE x.id = p.created_by) AS created_by_username,
         p.created_at, p.updated_at"
    };
}

/// Reads the billing settings with any executor (pool or transaction).
pub async fn load_settings<'e, E: PgExecutor<'e>>(
    executor: E,
) -> Result<BillingSettings, ApiError> {
    let stored: Option<Value> =
        sqlx::query_scalar("SELECT value FROM platform_settings WHERE key = $1")
            .bind(BILLING_SETTINGS_KEY)
            .fetch_optional(executor)
            .await?;
    Ok(stored
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default())
}

/// The plan in effect for `user_id` (a live subscription whose period has
/// not ended), if any.
pub async fn load_effective_plan<'e, E: PgExecutor<'e>>(
    executor: E,
    user_id: Uuid,
) -> Result<Option<Plan>, ApiError> {
    let row: Option<PlanRow> = sqlx::query_as(
        "SELECT p.code, p.name, p.description, p.price_monthly_cents, p.price_yearly_cents, p.currency,
                p.limits, p.features, p.highlighted, p.is_public, p.sort_order, p.updated_at
         FROM subscriptions s
         JOIN plans p ON p.code = s.plan_code
         WHERE s.user_id = $1
           AND s.status IN ('trialing', 'active', 'past_due')
           AND (s.current_period_end IS NULL OR s.current_period_end > $2)",
    )
    .bind(user_id)
    .bind(now())
    .fetch_optional(executor)
    .await?;
    Ok(row.map(Plan::from))
}

#[async_trait::async_trait]
pub trait BillingRepository: Send + Sync {
    async fn get_settings(&self) -> Result<BillingSettings, ApiError>;
    async fn set_settings(
        &self,
        settings: &BillingSettings,
        actor_id: Uuid,
    ) -> Result<(), ApiError>;

    async fn list_plans(&self, public_only: bool) -> Result<Vec<Plan>, ApiError>;
    async fn find_plan(&self, code: &str) -> Result<Option<Plan>, ApiError>;
    /// Updates `code`, or creates it when missing (a new plan needs a
    /// name). Returns `None` when creating without a name.
    async fn upsert_plan(
        &self,
        code: &str,
        payload: &UpsertPlanPayload,
        actor_id: Uuid,
    ) -> Result<Option<Plan>, ApiError>;
    /// Currently running promotions, keyed by plan (`None` = every paid
    /// plan), best discount first.
    async fn running_promotions(&self) -> Result<Vec<(Option<String>, PlanPromotion)>, ApiError>;

    async fn get_subscription(&self, user_id: Uuid) -> Result<Option<Subscription>, ApiError>;
    async fn effective_plan(&self, user_id: Uuid) -> Result<Option<Plan>, ApiError>;
    async fn subscription_events(
        &self,
        user_id: Uuid,
        limit: i64,
    ) -> Result<Vec<SubscriptionEvent>, ApiError>;

    async fn credit_balance(&self, user_id: Uuid) -> Result<i64, ApiError>;
    async fn credit_ledger(
        &self,
        user_id: Uuid,
        page: i64,
        per_page: i64,
    ) -> Result<(Vec<CreditEntry>, i64), ApiError>;
    /// `(rewarded, pending)` referrals made by the user.
    async fn referral_counts(&self, user_id: Uuid) -> Result<(i64, i64), ApiError>;
    async fn referrals(
        &self,
        user_id: Uuid,
        page: i64,
        per_page: i64,
    ) -> Result<(Vec<ReferralEntry>, i64), ApiError>;

    async fn list_promo_codes(
        &self,
        search: Option<&str>,
        page: i64,
        per_page: i64,
    ) -> Result<(Vec<PromoCode>, i64), ApiError>;
    async fn find_promo_code(&self, id: Uuid) -> Result<Option<PromoCode>, ApiError>;
    async fn create_promo_code(
        &self,
        code: &str,
        payload: &CreatePromoCodePayload,
        actor_id: Uuid,
    ) -> Result<PromoCode, ApiError>;
    async fn update_promo_code(
        &self,
        id: Uuid,
        payload: &UpdatePromoCodePayload,
    ) -> Result<Option<PromoCode>, ApiError>;
    async fn promo_redemptions(&self, id: Uuid) -> Result<Vec<PromoRedemption>, ApiError>;

    async fn list_promotions(&self) -> Result<Vec<Promotion>, ApiError>;
    async fn find_promotion(&self, id: Uuid) -> Result<Option<Promotion>, ApiError>;
    async fn create_promotion(
        &self,
        payload: &CreatePromotionPayload,
        actor_id: Uuid,
    ) -> Result<Promotion, ApiError>;
    async fn update_promotion(
        &self,
        id: Uuid,
        payload: &UpdatePromotionPayload,
    ) -> Result<Option<Promotion>, ApiError>;
    async fn delete_promotion(&self, id: Uuid) -> Result<bool, ApiError>;

    async fn overview(&self) -> Result<BillingOverview, ApiError>;
}

pub struct BillingRepositoryImpl {
    pub db: PgPool,
}

impl BillingRepositoryImpl {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}

#[async_trait::async_trait]
impl BillingRepository for BillingRepositoryImpl {
    async fn get_settings(&self) -> Result<BillingSettings, ApiError> {
        load_settings(&self.db).await
    }

    async fn set_settings(
        &self,
        settings: &BillingSettings,
        actor_id: Uuid,
    ) -> Result<(), ApiError> {
        let value =
            serde_json::to_value(settings).map_err(|e| ApiError::BadRequest(e.to_string()))?;
        sqlx::query(
            "INSERT INTO platform_settings (key, value, updated_at, updated_by)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (key) DO UPDATE SET value = $2, updated_at = $3, updated_by = $4",
        )
        .bind(BILLING_SETTINGS_KEY)
        .bind(value)
        .bind(now())
        .bind(actor_id)
        .execute(&self.db)
        .await?;
        Ok(())
    }

    async fn list_plans(&self, public_only: bool) -> Result<Vec<Plan>, ApiError> {
        let rows: Vec<PlanRow> = sqlx::query_as(concat!(
            "SELECT ",
            plan_columns!(),
            " FROM plans WHERE ($1 = FALSE OR is_public) ORDER BY sort_order, code"
        ))
        .bind(public_only)
        .fetch_all(&self.db)
        .await?;
        Ok(rows.into_iter().map(Plan::from).collect())
    }

    async fn find_plan(&self, code: &str) -> Result<Option<Plan>, ApiError> {
        let row: Option<PlanRow> = sqlx::query_as(concat!(
            "SELECT ",
            plan_columns!(),
            " FROM plans WHERE code = $1"
        ))
        .bind(code)
        .fetch_optional(&self.db)
        .await?;
        Ok(row.map(Plan::from))
    }

    async fn upsert_plan(
        &self,
        code: &str,
        payload: &UpsertPlanPayload,
        actor_id: Uuid,
    ) -> Result<Option<Plan>, ApiError> {
        let existing = self.find_plan(code).await?;
        let base = match existing {
            Some(plan) => plan,
            None => {
                let Some(name) = &payload.name else {
                    return Ok(None);
                };
                Plan {
                    code: code.to_string(),
                    name: name.clone(),
                    description: json!({}),
                    price_monthly_cents: 0,
                    price_yearly_cents: 0,
                    currency: "BRL".into(),
                    limits: Default::default(),
                    features: crate::models::billing::normalize_features(&json!({})),
                    highlighted: false,
                    is_public: false,
                    sort_order: 0,
                    updated_at: now(),
                }
            }
        };
        let mut features = base.features.clone();
        if let Some(changes) = &payload.features {
            for (key, value) in changes {
                features.insert(key.clone(), *value);
            }
        }
        let limits = payload.limits.unwrap_or(base.limits);
        let name = payload
            .name
            .as_ref()
            .map(trim_localized)
            .unwrap_or(base.name);
        let description = payload
            .description
            .as_ref()
            .map(trim_localized)
            .unwrap_or(base.description);

        let row: PlanRow = sqlx::query_as(concat!(
            "INSERT INTO plans (code, name, description, price_monthly_cents, price_yearly_cents, currency,
                                limits, features, highlighted, is_public, sort_order, updated_at, updated_by)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
             ON CONFLICT (code) DO UPDATE SET
                 name = $2, description = $3, price_monthly_cents = $4, price_yearly_cents = $5,
                 currency = $6, limits = $7, features = $8, highlighted = $9, is_public = $10,
                 sort_order = $11, updated_at = $12, updated_by = $13
             RETURNING ",
            plan_columns!()
        ))
        .bind(code)
        .bind(name)
        .bind(description)
        .bind(payload.price_monthly_cents.unwrap_or(base.price_monthly_cents))
        .bind(payload.price_yearly_cents.unwrap_or(base.price_yearly_cents))
        .bind(payload.currency.clone().unwrap_or(base.currency))
        .bind(serde_json::to_value(limits).unwrap_or_default())
        .bind(serde_json::to_value(&features).unwrap_or_default())
        .bind(payload.highlighted.unwrap_or(base.highlighted))
        .bind(payload.is_public.unwrap_or(base.is_public))
        .bind(payload.sort_order.unwrap_or(base.sort_order))
        .bind(now())
        .bind(actor_id)
        .fetch_one(&self.db)
        .await?;
        Ok(Some(Plan::from(row)))
    }

    async fn running_promotions(&self) -> Result<Vec<(Option<String>, PlanPromotion)>, ApiError> {
        let rows: Vec<(Option<String>, Uuid, Value, i32, NaiveDateTime)> = sqlx::query_as(
            "SELECT plan_code, id, headline, discount_percent, ends_at FROM promotions
             WHERE active AND starts_at <= $1 AND ends_at > $1
             ORDER BY discount_percent DESC, ends_at ASC",
        )
        .bind(now())
        .fetch_all(&self.db)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(plan, id, headline, discount_percent, ends_at)| {
                (
                    plan,
                    PlanPromotion {
                        id,
                        headline,
                        discount_percent,
                        ends_at,
                    },
                )
            })
            .collect())
    }

    async fn get_subscription(&self, user_id: Uuid) -> Result<Option<Subscription>, ApiError> {
        Ok(sqlx::query_as::<_, Subscription>(concat!(
            "SELECT ",
            subscription_columns!(),
            " FROM subscriptions WHERE user_id = $1"
        ))
        .bind(user_id)
        .fetch_optional(&self.db)
        .await?)
    }

    async fn effective_plan(&self, user_id: Uuid) -> Result<Option<Plan>, ApiError> {
        load_effective_plan(&self.db, user_id).await
    }

    async fn subscription_events(
        &self,
        user_id: Uuid,
        limit: i64,
    ) -> Result<Vec<SubscriptionEvent>, ApiError> {
        Ok(sqlx::query_as::<_, SubscriptionEvent>(
            "SELECT e.id, e.kind, e.from_plan, e.to_plan, e.from_status, e.to_status, e.data,
                    (SELECT x.username FROM users x WHERE x.id = e.actor_id) AS actor_username,
                    e.created_at
             FROM subscription_events e
             WHERE e.user_id = $1
             ORDER BY e.created_at DESC
             LIMIT $2",
        )
        .bind(user_id)
        .bind(limit)
        .fetch_all(&self.db)
        .await?)
    }

    async fn credit_balance(&self, user_id: Uuid) -> Result<i64, ApiError> {
        Ok(sqlx::query_scalar(
            "SELECT COALESCE(SUM(amount), 0)::bigint FROM credit_ledger WHERE user_id = $1",
        )
        .bind(user_id)
        .fetch_one(&self.db)
        .await?)
    }

    async fn credit_ledger(
        &self,
        user_id: Uuid,
        page: i64,
        per_page: i64,
    ) -> Result<(Vec<CreditEntry>, i64), ApiError> {
        let total: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM credit_ledger WHERE user_id = $1")
                .bind(user_id)
                .fetch_one(&self.db)
                .await?;
        let rows = sqlx::query_as::<_, CreditEntry>(
            "SELECT id, amount, reason, note, created_at FROM credit_ledger
             WHERE user_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3",
        )
        .bind(user_id)
        .bind(per_page)
        .bind((page - 1) * per_page)
        .fetch_all(&self.db)
        .await?;
        Ok((rows, total))
    }

    async fn referral_counts(&self, user_id: Uuid) -> Result<(i64, i64), ApiError> {
        Ok(sqlx::query_as(
            "SELECT COUNT(*) FILTER (WHERE status = 'rewarded'), COUNT(*) FILTER (WHERE status = 'pending')
             FROM referrals WHERE referrer_id = $1",
        )
        .bind(user_id)
        .fetch_one(&self.db)
        .await?)
    }

    async fn referrals(
        &self,
        user_id: Uuid,
        page: i64,
        per_page: i64,
    ) -> Result<(Vec<ReferralEntry>, i64), ApiError> {
        let total: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM referrals WHERE referrer_id = $1")
                .bind(user_id)
                .fetch_one(&self.db)
                .await?;
        let rows = sqlx::query_as::<_, ReferralEntry>(
            "SELECT u.username, r.status, r.created_at, r.rewarded_at
             FROM referrals r JOIN users u ON u.id = r.referred_id
             WHERE r.referrer_id = $1
             ORDER BY r.created_at DESC LIMIT $2 OFFSET $3",
        )
        .bind(user_id)
        .bind(per_page)
        .bind((page - 1) * per_page)
        .fetch_all(&self.db)
        .await?;
        Ok((rows, total))
    }

    async fn list_promo_codes(
        &self,
        search: Option<&str>,
        page: i64,
        per_page: i64,
    ) -> Result<(Vec<PromoCode>, i64), ApiError> {
        let total: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM promo_codes p
             WHERE ($1::text IS NULL OR p.code ILIKE $1 OR p.description ILIKE $1)",
        )
        .bind(search)
        .fetch_one(&self.db)
        .await?;
        let rows = sqlx::query_as::<_, PromoCode>(concat!(
            "SELECT ",
            promo_columns!(),
            " FROM promo_codes p
              WHERE ($1::text IS NULL OR p.code ILIKE $1 OR p.description ILIKE $1)
              ORDER BY p.created_at DESC LIMIT $2 OFFSET $3"
        ))
        .bind(search)
        .bind(per_page)
        .bind((page - 1) * per_page)
        .fetch_all(&self.db)
        .await?;
        Ok((rows, total))
    }

    async fn find_promo_code(&self, id: Uuid) -> Result<Option<PromoCode>, ApiError> {
        Ok(sqlx::query_as::<_, PromoCode>(concat!(
            "SELECT ",
            promo_columns!(),
            " FROM promo_codes p WHERE p.id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.db)
        .await?)
    }

    async fn create_promo_code(
        &self,
        code: &str,
        payload: &CreatePromoCodePayload,
        actor_id: Uuid,
    ) -> Result<PromoCode, ApiError> {
        let id = Uuid::now_v7();
        let timestamp = now();
        sqlx::query(
            "INSERT INTO promo_codes (id, code, description, kind, plan_code, duration_days, credits,
                                      discount_percent, max_redemptions, new_users_only, starts_at,
                                      expires_at, created_by, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $14)",
        )
        .bind(id)
        .bind(code)
        .bind(payload.description.as_deref().map(str::trim))
        .bind(payload.kind)
        .bind(&payload.plan_code)
        .bind(payload.duration_days)
        .bind(payload.credits)
        .bind(payload.discount_percent)
        .bind(payload.max_redemptions)
        .bind(payload.new_users_only)
        .bind(payload.starts_at)
        .bind(payload.expires_at)
        .bind(actor_id)
        .bind(timestamp)
        .execute(&self.db)
        .await?;
        self.find_promo_code(id).await?.ok_or(ApiError::NotFound)
    }

    async fn update_promo_code(
        &self,
        id: Uuid,
        payload: &UpdatePromoCodePayload,
    ) -> Result<Option<PromoCode>, ApiError> {
        let timestamp = now();
        let result = sqlx::query(
            "UPDATE promo_codes SET
                 description = CASE WHEN $2 THEN $3 ELSE description END,
                 expires_at = CASE WHEN $4 THEN $5 ELSE expires_at END,
                 max_redemptions = CASE WHEN $6 THEN $7 ELSE max_redemptions END,
                 disabled_at = CASE WHEN $8 IS NULL THEN disabled_at
                                    WHEN $8 THEN COALESCE(disabled_at, $9)
                                    ELSE NULL END,
                 updated_at = $9
             WHERE id = $1",
        )
        .bind(id)
        .bind(payload.description.is_some())
        .bind(
            payload
                .description
                .clone()
                .flatten()
                .map(|d| d.trim().to_string()),
        )
        .bind(payload.expires_at.is_some())
        .bind(payload.expires_at.flatten())
        .bind(payload.max_redemptions.is_some())
        .bind(payload.max_redemptions.flatten())
        .bind(payload.disabled)
        .bind(timestamp)
        .execute(&self.db)
        .await?;
        if result.rows_affected() == 0 {
            return Ok(None);
        }
        self.find_promo_code(id).await
    }

    async fn promo_redemptions(&self, id: Uuid) -> Result<Vec<PromoRedemption>, ApiError> {
        Ok(sqlx::query_as::<_, PromoRedemption>(
            "SELECT r.user_id, u.username, r.redeemed_at
             FROM promo_redemptions r JOIN users u ON u.id = r.user_id
             WHERE r.promo_code_id = $1
             ORDER BY r.redeemed_at DESC
             LIMIT 1000",
        )
        .bind(id)
        .fetch_all(&self.db)
        .await?)
    }

    async fn list_promotions(&self) -> Result<Vec<Promotion>, ApiError> {
        Ok(sqlx::query_as::<_, Promotion>(
            "SELECT id, name, headline, plan_code, discount_percent, starts_at, ends_at, active,
                    created_at, updated_at
             FROM promotions ORDER BY starts_at DESC LIMIT 500",
        )
        .fetch_all(&self.db)
        .await?)
    }

    async fn find_promotion(&self, id: Uuid) -> Result<Option<Promotion>, ApiError> {
        Ok(sqlx::query_as::<_, Promotion>(
            "SELECT id, name, headline, plan_code, discount_percent, starts_at, ends_at, active,
                    created_at, updated_at
             FROM promotions WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.db)
        .await?)
    }

    async fn create_promotion(
        &self,
        payload: &CreatePromotionPayload,
        actor_id: Uuid,
    ) -> Result<Promotion, ApiError> {
        let id = Uuid::now_v7();
        let timestamp = now();
        sqlx::query(
            "INSERT INTO promotions (id, name, headline, plan_code, discount_percent, starts_at, ends_at,
                                     active, created_by, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10)",
        )
        .bind(id)
        .bind(payload.name.trim())
        .bind(trim_localized(&payload.headline))
        .bind(&payload.plan_code)
        .bind(payload.discount_percent)
        .bind(payload.starts_at)
        .bind(payload.ends_at)
        .bind(payload.active.unwrap_or(true))
        .bind(actor_id)
        .bind(timestamp)
        .execute(&self.db)
        .await?;
        self.find_promotion(id).await?.ok_or(ApiError::NotFound)
    }

    async fn update_promotion(
        &self,
        id: Uuid,
        payload: &UpdatePromotionPayload,
    ) -> Result<Option<Promotion>, ApiError> {
        let result = sqlx::query(
            "UPDATE promotions SET
                 name = COALESCE($2, name),
                 headline = COALESCE($3, headline),
                 plan_code = CASE WHEN $4 THEN $5 ELSE plan_code END,
                 discount_percent = COALESCE($6, discount_percent),
                 starts_at = COALESCE($7, starts_at),
                 ends_at = COALESCE($8, ends_at),
                 active = COALESCE($9, active),
                 updated_at = $10
             WHERE id = $1",
        )
        .bind(id)
        .bind(payload.name.as_deref().map(str::trim))
        .bind(payload.headline.as_ref().map(trim_localized))
        .bind(payload.plan_code.is_some())
        .bind(payload.plan_code.clone().flatten())
        .bind(payload.discount_percent)
        .bind(payload.starts_at)
        .bind(payload.ends_at)
        .bind(payload.active)
        .bind(now())
        .execute(&self.db)
        .await?;
        if result.rows_affected() == 0 {
            return Ok(None);
        }
        self.find_promotion(id).await
    }

    async fn delete_promotion(&self, id: Uuid) -> Result<bool, ApiError> {
        let result = sqlx::query("DELETE FROM promotions WHERE id = $1")
            .bind(id)
            .execute(&self.db)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn overview(&self) -> Result<BillingOverview, ApiError> {
        let timestamp = now();
        let by_status_rows: Vec<(String, i64)> =
            sqlx::query_as("SELECT status::text, COUNT(*) FROM subscriptions GROUP BY status")
                .fetch_all(&self.db)
                .await?;
        let by_plan_rows: Vec<(String, i64)> = sqlx::query_as(
            "SELECT plan_code, COUNT(*) FROM subscriptions
             WHERE status IN ('trialing', 'active', 'past_due')
               AND (current_period_end IS NULL OR current_period_end > $1)
             GROUP BY plan_code",
        )
        .bind(timestamp)
        .fetch_all(&self.db)
        .await?;
        let without: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM users u
             WHERE NOT EXISTS (SELECT 1 FROM subscriptions s WHERE s.user_id = u.id)",
        )
        .fetch_one(&self.db)
        .await?;
        let (issued, spent): (i64, i64) = sqlx::query_as(
            "SELECT COALESCE(SUM(amount) FILTER (WHERE amount > 0), 0)::bigint,
                    COALESCE(-SUM(amount) FILTER (WHERE amount < 0), 0)::bigint
             FROM credit_ledger",
        )
        .fetch_one(&self.db)
        .await?;
        let redemptions: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM promo_redemptions WHERE redeemed_at >= $1")
                .bind(timestamp - chrono::Duration::days(30))
                .fetch_one(&self.db)
                .await?;
        let rewarded: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM referrals WHERE status = 'rewarded'")
                .fetch_one(&self.db)
                .await?;
        Ok(BillingOverview {
            enforced: self.get_settings().await?.enforced,
            by_status: by_status_rows.into_iter().collect::<BTreeMap<_, _>>(),
            by_plan: by_plan_rows.into_iter().collect::<BTreeMap<_, _>>(),
            accounts_without_subscription: without,
            credits_issued: issued,
            credits_spent: spent,
            promo_redemptions_last_30_days: redemptions,
            referrals_rewarded: rewarded,
        })
    }
}
