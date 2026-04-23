use crate::{
    controllers::{artist, auth, migrations, setlist, song, status, user},
    models::{
        artist::Artist,
        setlist::Setlist,
        song::{Genre, Song, Tonality},
        status::Status,
        user::User,
        user_preferences::UserPreferences,
    },
};
use serde::Serialize;
use utoipa::{
    Modify,
    openapi::{
        self,
        security::{HttpAuthScheme, HttpBuilder, SecurityScheme},
    },
};

#[derive(utoipa::OpenApi)]
#[openapi(
    info(
        title = "Setlyst API",
        description = "A simple REST API for setlist management.",
        contact(name = "Allan Somensi", email = "allansomensidev@proton.me"),
        license(name = "MIT", identifier = "MIT")
    ),
    servers(
        (url = "/", description = "Default Server")
    ),
    modifiers(&AuthToken),
    paths(
        // Status
        status::show_status,

        // Migrations
        migrations::live_run,

        // Auth
        auth::login,
        auth::register,
        auth::verify,

        // Users
        user::find_user_by_id,
        user::find_all_users,
        user::create_user,
        user::update_user,
        user::delete_user,
        user::get_current_user,
        user::update_current_user,
        user::get_current_user_preferences,
        user::update_current_user_preferences,
        user::get_user_preferences_by_id,

        // Artists
        artist::find_artist_by_id,
        artist::find_all_artists,
        artist::create_artist,
        artist::update_artist,
        artist::delete_artist,

        // Songs
        song::find_song_by_id,
        song::find_all_songs,
        song::create_song,
        song::update_song,
        song::delete_song,

        // Setlists
        setlist::find_setlist_by_id,
        setlist::find_all_setlists,
        setlist::create_setlist,
        setlist::update_setlist,
        setlist::delete_setlist,
        setlist::add_song_to_setlist,
        setlist::remove_song_from_setlist,
        setlist::get_setlist_songs,
        setlist::reorder_setlist_songs
    ),
    components(
        schemas(Status, User, UserPreferences ,Artist, Song, Setlist, Tonality, Genre)
    ),
    tags(
        (name = "Status", description = "Status endpoints"),
        (name = "Migrations", description = "Migrations endpoints"),
        (name = "Auth", description = "Auth endpoints"),
        (name = "Users", description = "Users endpoints"),
        (name = "Artists", description = "Artists endpoints"),
        (name = "Songs", description = "Songs endpoints"),
        (name = "Setlists", description = "Setlist endpoints"),
    )
)]
pub struct ApiDoc;

#[derive(Debug, Serialize)]
struct AuthToken;

impl Modify for AuthToken {
    fn modify(&self, openapi: &mut openapi::OpenApi) {
        if let Some(schema) = openapi.components.as_mut() {
            schema.add_security_scheme(
                "jwt_token",
                SecurityScheme::Http(
                    HttpBuilder::new()
                        .scheme(HttpAuthScheme::Bearer)
                        .bearer_format("JWT")
                        .build(),
                ),
            );
        }
    }
}
