use crate::{
    controllers::{
        artist, auth, backup, band, gig, metrics, migrations, notification, setlist, song, status,
        user,
    },
    export::pdf::PdfLocale,
    models::{
        artist::Artist,
        auth::LoginResponse,
        backup::{BackupFile, ImportSummary},
        band::{
            Band, BandInvite, BandMember, BandPermission, BandRolePermission,
            BandRolePermissionEntry, BandWithMembership, UpdateBandMemberTitlePayload,
            UpdateBandRolePermissionsPayload,
        },
        gig::{Gig, GigStatus, PublicGig},
        metrics::{
            AdminMetrics, AdminTimeseries, ArtistSongCount, GenreCount, MetricsResponse, RoleCount,
            TimeseriesPoint, TimeseriesResponse, UserMetrics, UserTimeseries,
        },
        notification::{Notification, NotificationType, UnreadCountResponse},
        setlist::{PublicSetlist, Setlist, SetlistItem, SetlistMarker},
        song::{Genre, Song, SongWithArtist, Tonality},
        status::Status,
        user::{ChangePasswordPayload, User, UsernameAvailability, UsernameHistoryEntry},
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
        user::change_current_user_password,
        user::check_username_availability,
        user::get_username_history,
        user::get_user_profile,
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
        song::export_songs_chordpro,

        // Setlists
        setlist::find_setlist_by_id,
        setlist::find_all_setlists,
        setlist::create_setlist,
        setlist::update_setlist,
        setlist::delete_setlist,
        setlist::add_song_to_setlist,
        setlist::remove_song_from_setlist,
        setlist::get_setlist_songs,
        setlist::reorder_setlist_songs,
        setlist::duplicate_setlist,
        setlist::get_setlist_items,
        setlist::reorder_setlist_items,
        setlist::create_setlist_block,
        setlist::update_setlist_block,
        setlist::create_setlist_break,
        setlist::update_setlist_break,
        setlist::delete_setlist_marker,
        setlist::export_setlist_pdf,
        setlist::enable_setlist_sharing,
        setlist::disable_setlist_sharing,
        setlist::favorite_setlist,
        setlist::unfavorite_setlist,
        setlist::get_public_setlist,
        setlist::export_public_setlist_pdf,

        // Gigs
        gig::find_gig_by_id,
        gig::find_all_gigs,
        gig::create_gig,
        gig::update_gig,
        gig::delete_gig,
        gig::enable_gig_sharing,
        gig::disable_gig_sharing,
        gig::get_public_gig,

        // Bands
        band::find_all_bands,
        band::find_band_by_id,
        band::create_band,
        band::update_band,
        band::delete_band,
        band::favorite_band,
        band::unfavorite_band,
        band::transfer_ownership,
        band::list_band_members,
        band::update_band_member_role,
        band::update_band_member_title,
        band::remove_band_member,
        band::create_band_invite,
        band::list_band_invites,
        band::revoke_band_invite,
        band::accept_band_invite,
        band::get_band_role_permissions,
        band::update_band_role_permissions,
        setlist::find_band_setlists,
        gig::find_band_gigs,

        // Metrics
        metrics::get_metrics,
        metrics::get_timeseries_metrics,

        // Notifications
        notification::list_notifications,
        notification::get_unread_count,
        notification::mark_notification_read,
        notification::mark_all_notifications_read,
        notification::delete_notification,

        // Backup
        backup::export_backup,
        backup::import_backup,
    ),
    components(
        schemas(
            Status,
            User,
            UsernameAvailability,
            UsernameHistoryEntry,
            ChangePasswordPayload,
            UserPreferences,
            Artist,
            Song,
            SongWithArtist,
            Setlist,
            PublicSetlist,
            SetlistMarker,
            SetlistItem,
            Gig,
            GigStatus,
            PublicGig,
            Tonality,
            Genre,
            Band,
            BandWithMembership,
            BandMember,
            BandInvite,
            BandPermission,
            BandRolePermission,
            BandRolePermissionEntry,
            UpdateBandRolePermissionsPayload,
            UpdateBandMemberTitlePayload,
            MetricsResponse,
            UserMetrics,
            AdminMetrics,
            TimeseriesResponse,
            UserTimeseries,
            AdminTimeseries,
            TimeseriesPoint,
            GenreCount,
            ArtistSongCount,
            RoleCount,
            BackupFile,
            ImportSummary,
            PdfLocale,
            LoginResponse,
            Notification,
            NotificationType,
            UnreadCountResponse,
        )
    ),
    tags(
        (name = "Status",     description = "Status endpoints"),
        (name = "Migrations", description = "Migrations endpoints"),
        (name = "Auth",       description = "Auth endpoints"),
        (name = "Users",      description = "Users endpoints"),
        (name = "Artists",    description = "Artists endpoints"),
        (name = "Songs",      description = "Songs endpoints"),
        (name = "Setlists",   description = "Setlist endpoints"),
        (name = "Gigs",       description = "Gig (show) endpoints"),
        (name = "Bands",      description = "Band, membership and invite endpoints"),
        (name = "Metrics",    description = "Metrics endpoints"),
        (name = "Notifications", description = "In-app notification endpoints"),
        (name = "Backup",     description = "Backup and restore endpoints"),
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
