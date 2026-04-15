pub mod connection;
pub mod repositories;

use repositories::{
    artist_repository::ArtistRepository, setlist_repository::SetlistRepository,
    song_repository::SongRepository, user_repository::UserRepository,
};
use sqlx::PgPool;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub user_repo: Arc<dyn UserRepository>,
    pub artist_repo: Arc<dyn ArtistRepository>,
    pub song_repo: Arc<dyn SongRepository>,
    pub setlist_repo: Arc<dyn SetlistRepository>,
}
