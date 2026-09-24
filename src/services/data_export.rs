//! The personal data export (LGPD art. 18, II and V): everything the
//! platform keeps about an account, in one JSON document.
//!
//! Repertoire content (songs, setlists, gigs…) has its own portable
//! format, the backup (`/backup/export`); this export covers the rest —
//! profile, preferences (communication choices included), consents,
//! subscription and payments, credits, band memberships and what the
//! account wrote in bands (reminders, suggestions, votes), favorites and
//! pins, moderation records about or by the account, recent e-mails,
//! pending one-time codes, notifications and the account's security log.
//!
//! [`EXPORTED_USER_REFERENCES`] lists every `table.column` pointing at the
//! account that the export reads; `tests/privacy.rs` checks it against the
//! database schema, so a new table holding personal data can't be
//! forgotten silently.

use crate::{database::AppState, errors::api_error::ApiError};
use chrono::Utc;
use serde_json::{Map, Value, json};
use uuid::Uuid;

/// Columns that never leave the server, even to the account's owner:
/// credentials and second-factor secrets. Also left out: columns that
/// point at other people (`*_by`: the staff member who banned or edited
/// the account, the account that referred it).
fn is_secret_column(name: &str) -> bool {
    const MARKERS: [&str; 6] = ["password", "totp", "secret", "hash", "token", "recovery"];
    let name = name.to_ascii_lowercase();
    MARKERS.iter().any(|m| name.contains(m)) || name.ends_with("_by")
}

fn without_secrets(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .filter(|(key, _)| !is_secret_column(key))
                .collect::<Map<String, Value>>(),
        ),
        other => other,
    }
}

/// Every row of `sql` (which selects from a subquery aliased `t` and binds
/// the account id as `$1`) as JSON objects.
async fn rows(state: &AppState, sql: &'static str, user_id: Uuid) -> Result<Vec<Value>, ApiError> {
    // Audited: `sql` is always one of the string literals below (the
    // parameter is `&'static str`), and the account id is bound.
    let wrapped = sqlx::AssertSqlSafe(format!("SELECT to_jsonb(t) FROM ({sql}) t"));
    let values: Vec<Value> = sqlx::query_scalar(wrapped)
        .bind(user_id)
        .fetch_all(&state.db)
        .await?;
    Ok(values.into_iter().map(without_secrets).collect())
}

async fn one(state: &AppState, sql: &'static str, user_id: Uuid) -> Result<Value, ApiError> {
    Ok(rows(state, sql, user_id)
        .await?
        .into_iter()
        .next()
        .unwrap_or(Value::Null))
}

/// Every foreign key to `users.id` (as `table.column`) whose rows the
/// export includes. Keep in sync with [`personal_data`].
pub const EXPORTED_USER_REFERENCES: &[&str] = &[
    "announcement_receipts.user_id",
    "audit_logs.actor_id",
    "band_members.user_id",
    "band_notes.author_id",
    "band_song_suggestion_votes.user_id",
    "band_song_suggestions.suggested_by",
    "credit_ledger.user_id",
    "email_outbox.user_id",
    "favorite_bands.user_id",
    "favorite_setlists.user_id",
    "legal_acceptances.user_id",
    "login_challenges.user_id",
    "moderation_flags.reported_by",
    "moderation_flags.user_id",
    "notifications.user_id",
    "oauth_identities.user_id",
    "payments.user_id",
    "promo_redemptions.user_id",
    "referrals.referred_id",
    "referrals.referrer_id",
    "subscription_events.user_id",
    "subscriptions.user_id",
    "user_pins.user_id",
    "user_preferences.user_id",
    "user_quotas.user_id",
    "username_history.user_id",
    "verification_codes.user_id",
];

pub async fn personal_data(state: &AppState, user_id: Uuid) -> Result<Value, ApiError> {
    let account = one(state, "SELECT * FROM users WHERE id = $1", user_id).await?;
    if account.is_null() {
        return Err(ApiError::NotFound);
    }

    Ok(json!({
        "format": "setlyst.personal-data",
        "version": 1,
        "generated_at": Utc::now().to_rfc3339(),
        "note": "Personal data held about this account. Repertoire content (songs, setlists, gigs, tours) is exported separately, in the backup.",
        "account": account,
        "username_history": rows(state,
            "SELECT old_username, changed_at FROM username_history WHERE user_id = $1 ORDER BY changed_at",
            user_id).await?,
        "preferences": one(state,
            "SELECT language, theme, live_mode_font_size, ui_settings, communication,
                    created_at, updated_at
             FROM user_preferences WHERE user_id = $1",
            user_id).await?,
        "linked_identities": rows(state,
            "SELECT provider, subject, email, created_at, last_used_at
             FROM oauth_identities WHERE user_id = $1",
            user_id).await?,
        "subscription": one(state,
            "SELECT plan_code, status, source, started_at, current_period_end, trial_ends_at,
                    cancel_at_period_end, canceled_at, billing_interval, created_at, updated_at
             FROM subscriptions WHERE user_id = $1",
            user_id).await?,
        "subscription_history": rows(state,
            "SELECT kind, from_plan, to_plan, from_status, to_status, created_at
             FROM subscription_events WHERE user_id = $1 ORDER BY created_at",
            user_id).await?,
        "payments": rows(state,
            "SELECT invoice_id, plan_code, billing_interval, amount_cents, refunded_cents, currency, paid_at
             FROM payments WHERE user_id = $1 ORDER BY paid_at",
            user_id).await?,
        "credits": rows(state,
            "SELECT amount, reason, note, created_at FROM credit_ledger WHERE user_id = $1 ORDER BY created_at",
            user_id).await?,
        "promo_codes_redeemed": rows(state,
            "SELECT c.code, r.redeemed_at FROM promo_redemptions r
             JOIN promo_codes c ON c.id = r.promo_code_id
             WHERE r.user_id = $1 ORDER BY r.redeemed_at",
            user_id).await?,
        // Other people's accounts are not identified: only the outcome.
        "referrals_made": rows(state,
            "SELECT status, created_at, rewarded_at FROM referrals WHERE referrer_id = $1 ORDER BY created_at",
            user_id).await?,
        "referred": one(state,
            "SELECT status, created_at, rewarded_at FROM referrals WHERE referred_id = $1",
            user_id).await?,
        "quota_overrides": one(state,
            "SELECT overrides, unlimited, updated_at FROM user_quotas WHERE user_id = $1",
            user_id).await?,
        "band_memberships": rows(state,
            "SELECT b.name AS band, m.role, m.title, m.joined_at FROM band_members m
             JOIN bands b ON b.id = m.band_id
             WHERE m.user_id = $1 ORDER BY m.joined_at",
            user_id).await?,
        // What the account wrote inside bands (other members' content is
        // left out).
        "band_notes_authored": rows(state,
            "SELECT b.name AS band, n.content, n.color, n.is_pinned, n.due_at, n.created_at, n.updated_at
             FROM band_notes n JOIN bands b ON b.id = n.band_id
             WHERE n.author_id = $1 ORDER BY n.created_at",
            user_id).await?,
        "song_suggestions": rows(state,
            "SELECT b.name AS band, s.song_title, s.artist_name, s.note, s.status,
                    s.resolution_note, s.created_at, s.resolved_at
             FROM band_song_suggestions s JOIN bands b ON b.id = s.band_id
             WHERE s.suggested_by = $1 ORDER BY s.created_at",
            user_id).await?,
        "suggestion_votes": rows(state,
            "SELECT b.name AS band, s.song_title, s.artist_name, v.value, v.created_at, v.updated_at
             FROM band_song_suggestion_votes v
             JOIN band_song_suggestions s ON s.id = v.suggestion_id
             JOIN bands b ON b.id = s.band_id
             WHERE v.user_id = $1 ORDER BY v.created_at",
            user_id).await?,
        "favorites": {
            "setlists": rows(state,
                "SELECT s.title AS setlist, f.created_at FROM favorite_setlists f
                 JOIN setlists s ON s.id = f.setlist_id
                 WHERE f.user_id = $1 ORDER BY f.created_at",
                user_id).await?,
            "bands": rows(state,
                "SELECT b.name AS band, f.created_at FROM favorite_bands f
                 JOIN bands b ON b.id = f.band_id
                 WHERE f.user_id = $1 ORDER BY f.created_at",
                user_id).await?,
        },
        "pins": rows(state,
            "SELECT item_type, item_id, position, created_at FROM user_pins
             WHERE user_id = $1 ORDER BY position",
            user_id).await?,
        // Moderation records about the account (who reported it and who
        // decided is left out), and reports it filed (without the reported
        // content, which is someone else's).
        "moderation": {
            "about_me": rows(state,
                "SELECT target_type, value, reasons, source, status, resolution, resolution_note,
                        created_at, resolved_at
                 FROM moderation_flags WHERE user_id = $1 ORDER BY created_at",
                user_id).await?,
            "reports_by_me": rows(state,
                "SELECT target_type, report_note, status, created_at, resolved_at
                 FROM moderation_flags WHERE reported_by = $1 ORDER BY created_at",
                user_id).await?,
        },
        // The last 30 days (older ones are deleted by the outbox retention).
        "emails_sent": rows(state,
            "SELECT template, to_email, status, created_at, sent_at FROM email_outbox
             WHERE user_id = $1 AND created_at > NOW() - INTERVAL '30 days'
             ORDER BY created_at",
            user_id).await?,
        // Consents given and withdrawn (terms, privacy notice, age
        // declaration, marketing), with the version accepted.
        "legal_acceptances": rows(state,
            "SELECT document, version, accepted, source, ip_address, user_agent, created_at
             FROM legal_acceptances WHERE user_id = $1 ORDER BY created_at",
            user_id).await?,
        // Codes themselves are only stored hashed and never exported.
        "one_time_codes": rows(state,
            "SELECT purpose, target_email, ip_address, created_at, expires_at, consumed_at
             FROM verification_codes WHERE user_id = $1 ORDER BY created_at",
            user_id).await?,
        "sign_in_challenges": rows(state,
            "SELECT method, ip_address, created_at, expires_at, consumed_at
             FROM login_challenges WHERE user_id = $1 ORDER BY created_at",
            user_id).await?,
        "notifications": rows(state,
            "SELECT * FROM notifications WHERE user_id = $1 ORDER BY created_at",
            user_id).await?,
        "announcements": rows(state,
            "SELECT announcement_id, seen_at, dismissed_at, acknowledged_at
             FROM announcement_receipts WHERE user_id = $1",
            user_id).await?,
        // What the account did (with the address it came from) and what was
        // done to it; staff identities are left out.
        // The account's own actions keep their details (e-mail changes are
        // recorded masked); actions of others only say what happened.
        "security_log": rows(state,
            "SELECT action, ip_address, metadata, created_at FROM audit_logs
             WHERE actor_id = $1 AND impersonator_id IS NULL
             UNION ALL
             SELECT action, NULL, NULL, created_at FROM audit_logs
             WHERE target_type = 'user' AND target_id = $1
               AND (actor_id IS DISTINCT FROM $1 OR impersonator_id IS NOT NULL)
             ORDER BY created_at",
            user_id).await?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_never_leave() {
        let filtered = without_secrets(json!({
            "username": "ana",
            "password_hash": "x",
            "totp_secret": "y",
            "totp_last_step": 1,
            "email": "ana@example.com",
            "stripe_customer_id": "cus_1",
            "banned_by": "staff-id",
            "referred_by": "someone-id",
        }));
        assert_eq!(
            filtered,
            json!({"username": "ana", "email": "ana@example.com", "stripe_customer_id": "cus_1"})
        );
    }
}
