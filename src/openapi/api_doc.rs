use crate::{
    controllers::{artist, auth, migrations, song, status, user},
    models::{artist::Artist, song::Song, status::Status, user::User},
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
        (url = "http://localhost:8000", description = "Local server"),
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
        user::count_users,
        user::find_user_by_id,
        user::find_all_users,
        user::create_user,
        user::update_user,
        user::delete_user,

        // Artists
        artist::count_artists,
        artist::find_artist_by_id,
        artist::find_all_artists,
        artist::create_artist,
        artist::update_artist,
        artist::delete_artist,

        // Songs
        song::count_songs,
        song::find_song_by_id,
        song::find_all_songs,
        song::create_song,
        song::update_song,
        song::delete_song,
    ),
    components(
        schemas(Status, User, Artist, Song)
    ),
    tags(
        (name = "Status", description = "Status endpoints"),
        (name = "Migrations", description = "Migrations endpoints"),
        (name = "Auth", description = "Auth endpoints"),
        (name = "Users", description = "Users endpoints"),
        (name = "Artists", description = "Artists endpoints"),
        (name = "Songs", description = "Songs endpoints"),
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
