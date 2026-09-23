//! Trash purge: rows deleted more than the retention period ago are
//! removed for good.

use crate::{
    database::repositories::trash_repository::{TrashRepository, TrashRepositoryImpl},
    errors::api_error::ApiError,
};
use chrono::{Duration, Utc};
use sqlx::PgPool;

/// Permanently deletes every song, artist, setlist, gig and tour that has
/// been in the trash for more than `retention_days` (songs before their
/// artists; an artist still referenced by a live song is kept). Returns
/// how many rows were deleted.
pub async fn purge_trash(pool: &PgPool, retention_days: i64) -> Result<u64, ApiError> {
    let cutoff = Utc::now().naive_utc() - Duration::days(retention_days.clamp(1, 3650));
    TrashRepositoryImpl::new(pool.clone())
        .purge_before(cutoff)
        .await
}
