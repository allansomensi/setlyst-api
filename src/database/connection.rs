use crate::{config::Config, errors::api_error::ApiError};
use sqlx::PgPool;

pub async fn create_pool() -> Result<PgPool, ApiError> {
    let config = Config::get();
    let pool = PgPool::connect(&config.database_url).await?;
    Ok(pool)
}
