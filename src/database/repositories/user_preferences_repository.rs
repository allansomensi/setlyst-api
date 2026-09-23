use crate::{
    errors::api_error::ApiError,
    models::user_preferences::{
        UpdatePreferencesPayload, UserPreferences, UserTheme, merge_ui_settings,
    },
};
use chrono::Utc;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

#[async_trait::async_trait]
pub trait UserPreferencesRepository: Send + Sync {
    /// Fetches the user's saved preferences, or a transient (not persisted)
    /// default when they haven't saved any yet. `fallback_language` is used
    /// only in that transient-default case — pass the locale the caller is
    /// actually viewing the app in so a brand-new user's Settings page
    /// shows the language they're already seeing.
    async fn get_by_user_id(
        &self,
        user_id: Uuid,
        fallback_language: &str,
    ) -> Result<UserPreferences, ApiError>;
    /// Creates or updates the row, shallow-merging `ui_settings`, and
    /// returns the stored result.
    async fn upsert(
        &self,
        user_id: Uuid,
        payload: &UpdatePreferencesPayload,
    ) -> Result<UserPreferences, ApiError>;
}

pub struct UserPreferencesRepositoryImpl {
    pub db: PgPool,
}

impl UserPreferencesRepositoryImpl {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}

#[async_trait::async_trait]
impl UserPreferencesRepository for UserPreferencesRepositoryImpl {
    async fn get_by_user_id(
        &self,
        user_id: Uuid,
        fallback_language: &str,
    ) -> Result<UserPreferences, ApiError> {
        let prefs = sqlx::query_as::<_, UserPreferences>(
            "SELECT id, user_id, language, theme, live_mode_font_size, ui_settings, created_at, updated_at
             FROM user_preferences WHERE user_id = $1",
        )
        .bind(user_id)
        .fetch_optional(&self.db)
        .await?;

        let now = Utc::now().naive_utc();

        Ok(prefs.unwrap_or(UserPreferences {
            id: Uuid::new_v4(),
            user_id,
            language: fallback_language.to_string(),
            theme: UserTheme::System,
            live_mode_font_size: 100,
            ui_settings: json!({}),
            created_at: now,
            updated_at: now,
        }))
    }

    async fn upsert(
        &self,
        user_id: Uuid,
        payload: &UpdatePreferencesPayload,
    ) -> Result<UserPreferences, ApiError> {
        let now = Utc::now().naive_utc();
        let mut tx = self.db.begin().await?;

        // Read-merge-write under a row lock so two tabs saving different
        // UI settings at once can't drop each other's keys.
        let current: Option<Value> = sqlx::query_scalar(
            "SELECT ui_settings FROM user_preferences WHERE user_id = $1 FOR UPDATE",
        )
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?;

        let ui_settings = match &payload.ui_settings {
            Some(patch) => merge_ui_settings(&current.unwrap_or_else(|| json!({})), patch),
            None => current.unwrap_or_else(|| json!({})),
        };

        let prefs = sqlx::query_as::<_, UserPreferences>(
            r#"
            INSERT INTO user_preferences (id, user_id, language, theme, live_mode_font_size, ui_settings, created_at, updated_at)
            VALUES ($1, $2, COALESCE($3, 'en'), COALESCE($4, 'system'), COALESCE($5, 100), $6, $7, $7)
            ON CONFLICT (user_id) DO UPDATE SET
                language = COALESCE($3, user_preferences.language),
                theme = COALESCE($4, user_preferences.theme),
                live_mode_font_size = COALESCE($5, user_preferences.live_mode_font_size),
                ui_settings = $6,
                updated_at = $7
            RETURNING id, user_id, language, theme, live_mode_font_size, ui_settings, created_at, updated_at
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(user_id)
        .bind(&payload.language)
        .bind(&payload.theme)
        .bind(payload.live_mode_font_size)
        .bind(&ui_settings)
        .bind(now)
        .fetch_one(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(prefs)
    }
}
