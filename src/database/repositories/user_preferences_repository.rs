use crate::{
    errors::api_error::ApiError,
    models::user_preferences::{UpdatePreferencesPayload, UserPreferences, UserTheme},
};
use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

#[async_trait::async_trait]
pub trait UserPreferencesRepository: Send + Sync {
    async fn get_by_user_id(&self, user_id: Uuid) -> Result<UserPreferences, ApiError>;
    async fn upsert(
        &self,
        user_id: Uuid,
        payload: &UpdatePreferencesPayload,
    ) -> Result<(), ApiError>;
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
    async fn get_by_user_id(&self, user_id: Uuid) -> Result<UserPreferences, ApiError> {
        let prefs = sqlx::query_as::<_, UserPreferences>(
            "SELECT * FROM user_preferences WHERE user_id = $1",
        )
        .bind(user_id)
        .fetch_optional(&self.db)
        .await?;

        let now = Utc::now().naive_utc();

        match prefs {
            Some(p) => Ok(p),
            None => Ok(UserPreferences {
                id: Uuid::new_v4(),
                user_id,
                language: "en".to_string(),
                theme: UserTheme::System,
                live_mode_font_size: 100,
                created_at: now,
                updated_at: now,
            }),
        }
    }

    async fn upsert(
        &self,
        user_id: Uuid,
        payload: &UpdatePreferencesPayload,
    ) -> Result<(), ApiError> {
        let now = chrono::Utc::now().naive_utc();

        sqlx::query(
            r#"
            INSERT INTO user_preferences (id, user_id, language, theme, live_mode_font_size, created_at, updated_at)
            VALUES ($1, $2, COALESCE($3, 'en'), COALESCE($4, 'system'), COALESCE($5, 100), $6, $6)
            ON CONFLICT (user_id) DO UPDATE SET
                language = COALESCE($3, user_preferences.language),
                theme = COALESCE($4, user_preferences.theme),
                live_mode_font_size = COALESCE($5, user_preferences.live_mode_font_size),
                updated_at = $6
            "#
        )
        .bind(Uuid::new_v4())
        .bind(user_id)
        .bind(&payload.language)
        .bind(&payload.theme)
        .bind(payload.live_mode_font_size)
        .bind(now)
        .execute(&self.db)
        .await?;

        Ok(())
    }
}
