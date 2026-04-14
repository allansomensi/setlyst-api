use crate::errors::api_error::ApiError;
use sqlx::PgPool;
use std::env;

pub async fn create_pool() -> Result<PgPool, ApiError> {
    let database_url = env::var("DATABASE_URL")?;

    let pool = PgPool::connect(&database_url).await?;
    Ok(pool)
}
