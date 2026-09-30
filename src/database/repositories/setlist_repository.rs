use crate::database::repositories::{
    quota_repository::{QuotaGuard, lock_scope},
    song_repository::{like_pattern, song_columns, song_summary_columns},
};
use crate::{
    errors::api_error::{ApiError, codes},
    models::{
        band::BandRole,
        link::Links,
        quota::{QuotaLimits, QuotaResource},
        setlist::{
            CopiedSetlistSong, CreateSetlistPayload, Setlist, SetlistItem, SetlistItemRef,
            SetlistItemType, SetlistMarker, SetlistMarkerType, UpdateSetlistPayload,
        },
        setlist_collaborator::CollaboratorRole,
        song::{Song, SongWithArtist},
    },
    validations::link::normalize_links,
};
use axum::http::StatusCode;
use sqlx::{PgPool, Postgres, Transaction};
use tracing::error;
use uuid::Uuid;

/// Columns selected for a [`Setlist`] from `setlists s` (everything but the
/// caller-specific `is_favorite`). A macro so it can be spliced into
/// `concat!` — sqlx only accepts literal query strings. `song_count` only
/// counts live songs of the setlist's own scope (see [`scoped_songs!`]).
macro_rules! setlist_columns {
    () => {
        "s.id, s.title, s.description, s.user_id, s.band_id, s.share_token,
         s.share_locked_at, s.share_lock_reason, s.created_at, s.updated_at, s.updated_by,
         s.links, s.is_repertoire,
         (SELECT u.username FROM users u WHERE u.id = s.updated_by) AS updated_by_username,
         (SELECT u.username FROM users u WHERE u.id = s.user_id) AS owner_username,
         (SELECT COUNT(*) FROM setlist_songs sc
            INNER JOIN songs so ON so.id = sc.song_id
            WHERE sc.setlist_id = s.id AND so.deleted_at IS NULL
              AND ((s.band_id IS NULL AND so.band_id IS NULL
                    AND (so.user_id = s.user_id
                         OR EXISTS (SELECT 1 FROM setlist_collaborators co
                                    WHERE co.setlist_id = s.id AND co.user_id = so.user_id
                                      AND co.accepted_at IS NOT NULL)))
                   OR so.band_id = s.band_id))
         + (SELECT COUNT(*) FROM setlist_held_songs h WHERE h.setlist_id = s.id) AS song_count,
         (SELECT COUNT(*) FROM setlist_collaborators co
            WHERE co.setlist_id = s.id AND co.accepted_at IS NOT NULL) AS collaborator_count,
         setlist_total_duration(s.id) AS total_duration"
    };
}

/// The condition keeping a setlist's songs to live songs of its own scope:
/// a personal setlist only shows the personal songs of its owner and of
/// its (accepted) collaborators, and a band setlist only that band's
/// copies — even if a stale row points elsewhere (defense in depth: an old
/// duplicate could hold band songs in a personal setlist). Expects
/// `songs s` and `setlists st`.
macro_rules! scoped_songs {
    () => {
        "s.deleted_at IS NULL
         AND ((st.band_id IS NULL AND s.band_id IS NULL
               AND (s.user_id = st.user_id
                    OR EXISTS (SELECT 1 FROM setlist_collaborators co
                               WHERE co.setlist_id = st.id AND co.user_id = s.user_id
                                 AND co.accepted_at IS NOT NULL)))
              OR s.band_id = st.band_id)"
    };
}

/// Who added each song, for the columns of a setlist song (expects
/// `setlist_songs ss`).
macro_rules! added_by_columns {
    () => {
        "ss.added_by, ss.added_at,
         (SELECT u.username FROM users u WHERE u.id = ss.added_by) AS added_by_username,
         (SELECT u.avatar_url FROM users u WHERE u.id = ss.added_by) AS added_by_avatar_url"
    };
}

/// A held song (`setlist_held_songs h` of `setlists st`) in the columns of
/// a setlist song: [`song_columns!`], artist name, key, attribution and
/// `held`, in that order, so the two can be `UNION`ed. It belongs to no
/// library: no artist record (nil id), and the setlist's owner as
/// `user_id`.
macro_rules! held_song_columns {
    () => {
        "h.id, h.title, '00000000-0000-0000-0000-000000000000'::uuid AS artist_id, st.user_id,
         NULL::uuid AS band_id, NULL::uuid AS forked_from, h.version_label, NULL::uuid AS version_of,
         h.tempo, h.lyrics, h.tonality, h.genre, h.duration, h.energy, h.time_signature, h.capo,
         h.tuning, h.performance_notes, h.links, h.tags::varchar[] AS tags,
         NULL::uuid AS updated_by, NULL::varchar AS updated_by_username,
         NULL::timestamp AS source_synced_at, h.created_at, h.held_at AS updated_at,
         h.artist_name, h.transpose,
         h.added_by, h.added_at,
         (SELECT u.username FROM users u WHERE u.id = h.added_by) AS added_by_username,
         (SELECT u.avatar_url FROM users u WHERE u.id = h.added_by) AS added_by_avatar_url,
         TRUE AS held"
    };
}

/// The next position at the end of setlist `$1`'s running order (songs,
/// held songs and markers share one ordering space).
macro_rules! next_position {
    () => {
        "COALESCE(GREATEST(
            (SELECT MAX(position) FROM setlist_songs WHERE setlist_id = $1),
            (SELECT MAX(position) FROM setlist_held_songs WHERE setlist_id = $1),
            (SELECT MAX(position) FROM setlist_markers WHERE setlist_id = $1)
        ), 0) + 1"
    };
}

/// [`held_song_columns!`] without the chart (`NULL` lyrics and notes), for
/// views that don't show it.
macro_rules! held_song_summary_columns {
    () => {
        "h.id, h.title, '00000000-0000-0000-0000-000000000000'::uuid AS artist_id, st.user_id,
         NULL::uuid AS band_id, NULL::uuid AS forked_from, h.version_label, NULL::uuid AS version_of,
         h.tempo, NULL::text AS lyrics, h.tonality, h.genre, h.duration, h.energy, h.time_signature, h.capo,
         h.tuning, NULL::text AS performance_notes, h.links, h.tags::varchar[] AS tags,
         NULL::uuid AS updated_by, NULL::varchar AS updated_by_username,
         NULL::timestamp AS source_synced_at, h.created_at, h.held_at AS updated_at,
         h.artist_name, h.transpose,
         h.added_by, h.added_at,
         (SELECT u.username FROM users u WHERE u.id = h.added_by) AS added_by_username,
         (SELECT u.avatar_url FROM users u WHERE u.id = h.added_by) AS added_by_avatar_url,
         TRUE AS held"
    };
}

/// [`setlist_song_rows!`] without the chart: `NULL` lyrics and notes. The
/// public share views and the PDFs that don't print the songbook read a
/// whole running order (a repertoire can hold thousands of songs) and
/// never show a line of it.
macro_rules! setlist_song_summary_rows {
    () => {
        concat!(
            "SELECT ss.position, ",
            song_summary_columns!(),
            ", a.name AS artist_name, ss.transpose, ",
            added_by_columns!(),
            ", FALSE AS held
             FROM songs s
             INNER JOIN setlist_songs ss ON s.id = ss.song_id
             INNER JOIN setlists st ON st.id = ss.setlist_id
             INNER JOIN artists a ON a.id = s.artist_id
             WHERE ss.setlist_id = $1 AND ",
            scoped_songs!(),
            " UNION ALL
             SELECT h.position, ",
            held_song_summary_columns!(),
            " FROM setlist_held_songs h
             INNER JOIN setlists st ON st.id = h.setlist_id
             WHERE h.setlist_id = $1"
        )
    };
}

/// Every song of setlist `$1` with its `position`: the songs it links to
/// (of its own scope, see [`scoped_songs!`]) and the ones it holds. A
/// subquery to order and page.
macro_rules! setlist_song_rows {
    () => {
        concat!(
            "SELECT ss.position, ",
            song_columns!(),
            ", a.name AS artist_name, ss.transpose, ",
            added_by_columns!(),
            ", FALSE AS held
             FROM songs s
             INNER JOIN setlist_songs ss ON s.id = ss.song_id
             INNER JOIN setlists st ON st.id = ss.setlist_id
             INNER JOIN artists a ON a.id = s.artist_id
             WHERE ss.setlist_id = $1 AND ",
            scoped_songs!(),
            " UNION ALL
             SELECT h.position, ",
            held_song_columns!(),
            " FROM setlist_held_songs h
             INNER JOIN setlists st ON st.id = h.setlist_id
             WHERE h.setlist_id = $1"
        )
    };
}

/// Whether `$2` may see setlist `s`: its personal owner, a member of its
/// band, or an accepted collaborator.
macro_rules! visible_to_caller {
    () => {
        "((s.band_id IS NULL AND s.user_id = $2)
          OR EXISTS (SELECT 1 FROM band_members bm WHERE bm.band_id = s.band_id AND bm.user_id = $2)
          OR (s.band_id IS NULL AND EXISTS (
                SELECT 1 FROM setlist_collaborators co
                WHERE co.setlist_id = s.id AND co.user_id = $2 AND co.accepted_at IS NOT NULL)))"
    };
}

#[async_trait::async_trait]
pub trait SetlistRepository: Send + Sync {
    /// Personal setlists of other accounts the caller collaborates on
    /// (accepted invites), newest change first. Each carries the caller's
    /// `collaborator_role`.
    async fn find_shared(
        &self,
        user_id: Uuid,
        page: i64,
        size: i64,
    ) -> Result<(Vec<Setlist>, i64), ApiError>;

    /// Lists the caller's personal setlists (i.e. `band_id IS NULL`).
    async fn find_all(
        &self,
        user_id: Uuid,
        page: i64,
        size: i64,
    ) -> Result<(Vec<Setlist>, i64), ApiError>;
    /// Lists every live setlist of a band, the repertoire first. Callers
    /// must check band membership themselves before calling this.
    /// `user_id` is only used to resolve each setlist's `is_favorite`.
    async fn find_all_for_band(
        &self,
        band_id: Uuid,
        user_id: Uuid,
        page: i64,
        size: i64,
    ) -> Result<(Vec<Setlist>, i64), ApiError>;
    /// Fetches a live setlist the caller may *view*: either their own
    /// personal setlist, or a setlist belonging to any band they are a
    /// member of.
    async fn find_by_id(&self, id: Uuid, user_id: Uuid) -> Result<Option<Setlist>, ApiError>;
    /// Creates a setlist. `quota` is enforced inside the insert's
    /// transaction (see [`QuotaGuard`]).
    async fn create(
        &self,
        payload: &CreateSetlistPayload,
        user_id: Uuid,
        quota: &[QuotaGuard],
    ) -> Result<Setlist, ApiError>;
    /// Title changes on a repertoire fail with `REPERTOIRE_PROTECTED`.
    async fn update(
        &self,
        id: Uuid,
        payload: &UpdateSetlistPayload,
        actor_id: Uuid,
    ) -> Result<Uuid, ApiError>;
    /// Permanently deletes a setlist (staff tooling and the trash). A
    /// repertoire can't be deleted (`REPERTOIRE_PROTECTED`).
    async fn delete(&self, id: Uuid) -> Result<(), ApiError>;
    /// Moves a live setlist to the trash (`REPERTOIRE_PROTECTED` for a
    /// repertoire).
    async fn trash(&self, id: Uuid, actor_id: Uuid) -> Result<(), ApiError>;
    /// Records that the setlist's contents changed (songs, blocks, breaks,
    /// order) — bumps `updated_at` and `updated_by`, so "last modified"
    /// reflects the running order, not just the title.
    async fn touch(&self, id: Uuid, actor_id: Uuid) -> Result<(), ApiError>;
    /// Any live setlist by ID, without an access filter (staff tooling and
    /// public gig pages).
    async fn find_any(&self, id: Uuid) -> Result<Option<Setlist>, ApiError>;
    /// Staff takedown: clears the share token and locks sharing so the
    /// owner can't re-enable it until staff unlocks it. Gigs showing this
    /// setlist lose their public link too.
    async fn lock_sharing(
        &self,
        id: Uuid,
        actor_id: Uuid,
        reason: Option<&str>,
    ) -> Result<(), ApiError>;
    async fn unlock_sharing(&self, id: Uuid) -> Result<(), ApiError>;
    /// Marks a setlist as a favorite for this user. Idempotent — favoriting
    /// an already-favorited setlist is a no-op, not an error.
    async fn add_favorite(&self, setlist_id: Uuid, user_id: Uuid) -> Result<(), ApiError>;
    /// Un-favorites a setlist for this user. Idempotent.
    async fn remove_favorite(&self, setlist_id: Uuid, user_id: Uuid) -> Result<(), ApiError>;
    /// Title uniqueness among the live setlists of the scope (the
    /// repertoire never counts).
    async fn is_unique(
        &self,
        title: &str,
        user_id: Uuid,
        band_id: Option<Uuid>,
        exclude_id: Option<Uuid>,
    ) -> Result<(), ApiError>;
    /// Checks the caller may *view* the setlist (see `find_by_id`).
    async fn exists(&self, id: Uuid, user_id: Uuid) -> Result<(), ApiError>;
    /// Checks the caller may *manage* (edit/delete/add or remove songs from)
    /// the setlist: its personal owner, or a band member whose role clears
    /// the band's setlist-management bar (`moderator`+, or `member` when the
    /// band allows it).
    async fn can_manage(&self, id: Uuid, user_id: Uuid) -> Result<(), ApiError>;

    /// Changing the running order (songs, blocks, breaks, keys): what
    /// [`can_manage`](Self::can_manage) allows, and collaborators from
    /// `editor` up.
    async fn can_edit_items(&self, id: Uuid, user_id: Uuid) -> Result<(), ApiError>;

    /// Editing the title, description and links: what
    /// [`can_manage`](Self::can_manage) allows, and `manager`
    /// collaborators.
    async fn can_edit_details(&self, id: Uuid, user_id: Uuid) -> Result<(), ApiError>;
    /// Checks the caller may export this setlist to PDF: its personal
    /// owner, or a band member whose role satisfies the band's
    /// `export_pdf` permission (`admin`+ always can; `moderator`/`member`
    /// follow the band's configurable permission matrix).
    async fn can_export_pdf(&self, id: Uuid, user_id: Uuid) -> Result<(), ApiError>;
    /// Generates a fresh public share token for the setlist, replacing any
    /// existing one (which immediately invalidates previously shared links).
    /// Returns the updated setlist.
    async fn enable_sharing(&self, id: Uuid) -> Result<Setlist, ApiError>;
    /// Disables public sharing (clears the share token).
    async fn disable_sharing(&self, id: Uuid) -> Result<(), ApiError>;
    /// Resolves a live setlist by its public share token. No ownership or
    /// membership filter — this is the lookup used by the unauthenticated
    /// `/public/setlists/{token}` routes.
    async fn find_by_share_token(&self, token: &str) -> Result<Option<Setlist>, ApiError>;
    /// Adds a song to the end of the setlist's shared song/marker ordering
    /// space (the position is always computed server-side). For a band
    /// setlist the song is also appended to the band's repertoire when it
    /// isn't there yet, in the same transaction.
    /// `quota` (the setlist's item limit) is enforced in the same
    /// transaction.
    async fn add_song(
        &self,
        setlist_id: Uuid,
        song_id: Uuid,
        added_by: Uuid,
        quota: &[QuotaGuard],
    ) -> Result<(), ApiError>;
    /// Checks whether a song is already part of a setlist — used to reject
    /// adding the same song twice rather than silently repositioning it.
    async fn has_song(&self, setlist_id: Uuid, song_id: Uuid) -> Result<bool, ApiError>;
    async fn remove_song(&self, setlist_id: Uuid, song_id: Uuid) -> Result<(), ApiError>;
    /// Sets the key a song is played in in this setlist, as semitones from
    /// its written key (`NotFound` if the song isn't in the setlist).
    async fn set_song_transpose(
        &self,
        setlist_id: Uuid,
        song_id: Uuid,
        transpose: i16,
    ) -> Result<(), ApiError>;
    async fn get_songs(
        &self,
        setlist_id: Uuid,
        page: i64,
        size: i64,
    ) -> Result<(Vec<SongWithArtist>, i64), ApiError>;
    /// Live songs with their position in the running order (public pages),
    /// at most the first `max_songs`.
    async fn get_positioned_songs(
        &self,
        setlist_id: Uuid,
        max_songs: i64,
    ) -> Result<Vec<(i32, SongWithArtist)>, ApiError>;
    /// How many live songs and markers the setlist holds.
    async fn count_items(&self, setlist_id: Uuid) -> Result<i64, ApiError>;
    async fn reorder_songs(&self, setlist_id: Uuid, song_ids: &[Uuid]) -> Result<(), ApiError>;
    /// Creates an independent personal copy of a setlist the caller can
    /// view: title (optionally overridden), description, links and every
    /// song/block/break in it, preserving relative order.
    ///
    /// Band songs are never referenced from the copy: each is replaced by a
    /// personal copy in the caller's library (an existing personal song
    /// with the same title and artist is reused; otherwise the artist and
    /// song are created). Songs that would take the caller over their song
    /// or artist quota are left out; their number is returned alongside
    /// the new setlist.
    ///
    /// `quota` (the caller's setlist limit) is enforced in the copy's
    /// transaction.
    async fn duplicate(
        &self,
        id: Uuid,
        user_id: Uuid,
        title_override: Option<String>,
        limits: Option<QuotaLimits>,
        quota: &[QuotaGuard],
    ) -> Result<(Setlist, i64), ApiError>;
    /// Lists a setlist's block/break markers, ordered by position.
    async fn get_markers(&self, setlist_id: Uuid) -> Result<Vec<SetlistMarker>, ApiError>;
    /// The full, position-merged view of a setlist's contents (songs,
    /// block headers, and breaks) used by the setlist builder UI.
    async fn get_items(&self, setlist_id: Uuid) -> Result<Vec<SetlistItem>, ApiError>;
    /// [`get_items_capped`](Self::get_items_capped) without the songs'
    /// lyrics and notes (`None` on every song), for what never prints them.
    async fn get_items_capped_summary(
        &self,
        setlist_id: Uuid,
        max_items: usize,
    ) -> Result<Vec<SetlistItem>, ApiError>;
    /// Like [`get_items`](Self::get_items), but reads at most `max_items + 1`
    /// songs and `max_items + 1` markers: enough for the caller to tell the
    /// setlist is over `max_items` without loading all of it (a repertoire
    /// can hold thousands of songs with their lyrics).
    async fn get_items_capped(
        &self,
        setlist_id: Uuid,
        max_items: usize,
    ) -> Result<Vec<SetlistItem>, ApiError>;
    /// Appends a block. `quota` (the setlist's item limit) is enforced in
    /// the insert's transaction; a repertoire holds at most
    /// [`MAX_REPERTOIRE_MARKERS`] blocks and breaks.
    async fn create_block(
        &self,
        setlist_id: Uuid,
        name: &str,
        quota: &[QuotaGuard],
    ) -> Result<SetlistMarker, ApiError>;
    async fn update_block(
        &self,
        setlist_id: Uuid,
        marker_id: Uuid,
        name: &str,
    ) -> Result<SetlistMarker, ApiError>;
    /// Appends a break; limits as for [`create_block`](Self::create_block).
    async fn create_break(
        &self,
        setlist_id: Uuid,
        label: Option<String>,
        duration_minutes: Option<i32>,
        quota: &[QuotaGuard],
    ) -> Result<SetlistMarker, ApiError>;
    async fn update_break(
        &self,
        setlist_id: Uuid,
        marker_id: Uuid,
        label: Option<String>,
        duration_minutes: Option<i32>,
    ) -> Result<SetlistMarker, ApiError>;
    async fn delete_marker(&self, setlist_id: Uuid, marker_id: Uuid) -> Result<(), ApiError>;
    /// Rewrites the position of every song and marker in `items` to match
    /// its index in the list — songs and markers share one ordering space,
    /// so this is how blocks/breaks get interleaved with songs.
    async fn reorder_items(
        &self,
        setlist_id: Uuid,
        items: &[SetlistItemRef],
    ) -> Result<(), ApiError>;
    /// Copies one song of a personal setlist into `user_id`'s library
    /// (their song and artist limits apply): a song the setlist holds, or
    /// one of someone else's library it links. When `adopt` (the caller
    /// owns the setlist) and the song is held, the setlist links the copy
    /// from then on and the held song goes.
    async fn copy_song_to_library(
        &self,
        setlist_id: Uuid,
        song_id: Uuid,
        user_id: Uuid,
        limits: Option<QuotaLimits>,
        adopt: bool,
    ) -> Result<CopiedSetlistSong, ApiError>;
    /// The id of a band's repertoire.
    async fn find_repertoire_id(&self, band_id: Uuid) -> Result<Option<Uuid>, ApiError>;
    /// Live songs of a band's repertoire, alphabetically, optionally
    /// filtered by title or artist.
    async fn get_repertoire_songs(
        &self,
        band_id: Uuid,
        search: Option<&str>,
        page: i64,
        size: i64,
    ) -> Result<(Vec<SongWithArtist>, i64), ApiError>;
}

/// Blocks and breaks a band's repertoire may hold. The repertoire's songs
/// are bounded by the band's song quota instead of `setlist_items`, but its
/// markers would otherwise be unbounded.
pub const MAX_REPERTOIRE_MARKERS: i64 = 200;

pub struct SetlistRepositoryImpl {
    pub db: PgPool,
}

impl SetlistRepositoryImpl {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}

/// Row shape used to decide write permission on a setlist without fetching
/// its full column set.
#[derive(sqlx::FromRow)]
struct SetlistAccessRow {
    owner_id: Uuid,
    band_id: Option<Uuid>,
    band_role: Option<BandRole>,
    role_permission_allowed: Option<bool>,
    collaborator_role: Option<CollaboratorRole>,
}

/// What a caller wants to do to a setlist, for [`SetlistRepositoryImpl::check_permission`].
#[derive(Debug, Clone, Copy)]
enum SetlistAction {
    /// Delete it, share it publicly: its owner, or band members allowed
    /// to manage setlists. Never collaborators.
    Manage,
    /// Change its running order: also collaborators from `editor` up.
    EditItems,
    /// Edit its title, description and links: also `manager`
    /// collaborators.
    EditDetails,
    /// Export it as a PDF: the band's `export_pdf` permission, and any
    /// collaborator (they already read every chart in it).
    ExportPdf,
}

impl SetlistAction {
    fn band_permission(self) -> &'static str {
        match self {
            SetlistAction::ExportPdf => "export_pdf",
            _ => "manage_setlists",
        }
    }

    /// The lowest collaborator role allowed, if any.
    fn collaborator_minimum(self) -> Option<CollaboratorRole> {
        match self {
            SetlistAction::Manage => None,
            SetlistAction::EditItems => Some(CollaboratorRole::Editor),
            SetlistAction::EditDetails => Some(CollaboratorRole::Manager),
            SetlistAction::ExportPdf => Some(CollaboratorRole::Viewer),
        }
    }
}

pub(crate) fn repertoire_protected() -> ApiError {
    ApiError::rule(
        StatusCode::CONFLICT,
        codes::REPERTOIRE_PROTECTED,
        "The band's repertoire can't be deleted or renamed.",
    )
}

/// Appends `song_id` at the end of `setlist_id` (no-op if already there),
/// recording `added_by` as who added it.
async fn append_song(
    tx: &mut Transaction<'_, Postgres>,
    setlist_id: Uuid,
    song_id: Uuid,
    added_by: Uuid,
) -> Result<(), ApiError> {
    sqlx::query(concat!(
        "INSERT INTO setlist_songs (setlist_id, song_id, position, added_by, added_at)
         SELECT $1, $2, ",
        next_position!(),
        ", $3, $4
         ON CONFLICT (setlist_id, song_id) DO NOTHING"
    ))
    .bind(setlist_id)
    .bind(song_id)
    .bind(added_by)
    .bind(chrono::Utc::now().naive_utc())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[async_trait::async_trait]
impl SetlistRepository for SetlistRepositoryImpl {
    async fn find_all(
        &self,
        user_id: Uuid,
        page: i64,
        size: i64,
    ) -> Result<(Vec<Setlist>, i64), ApiError> {
        let offset = (page - 1) * size;

        let count = sqlx::query_scalar(
            "SELECT COUNT(*) FROM setlists WHERE user_id = $1 AND band_id IS NULL AND deleted_at IS NULL;",
        )
        .bind(user_id)
        .fetch_one(&self.db);

        let setlists = sqlx::query_as::<_, Setlist>(concat!(
            "SELECT ",
            setlist_columns!(),
            ", EXISTS(SELECT 1 FROM favorite_setlists f WHERE f.setlist_id = s.id AND f.user_id = $1) AS is_favorite
            FROM setlists s
            WHERE s.user_id = $1 AND s.band_id IS NULL AND s.deleted_at IS NULL
            ORDER BY is_favorite DESC, LOWER(s.title) ASC, s.id ASC
            LIMIT $2 OFFSET $3"
        ))
        .bind(user_id)
        .bind(size)
        .bind(offset)
        .fetch_all(&self.db);

        let (count, setlists) = tokio::try_join!(count, setlists)?;
        Ok((setlists, count))
    }

    async fn find_shared(
        &self,
        user_id: Uuid,
        page: i64,
        size: i64,
    ) -> Result<(Vec<Setlist>, i64), ApiError> {
        let offset = (page - 1) * size;

        let count = sqlx::query_scalar(
            "SELECT COUNT(*) FROM setlist_collaborators c
             INNER JOIN setlists s ON s.id = c.setlist_id
             WHERE c.user_id = $1 AND c.accepted_at IS NOT NULL
               AND s.deleted_at IS NULL AND s.band_id IS NULL",
        )
        .bind(user_id)
        .fetch_one(&self.db);

        let setlists = sqlx::query_as::<_, Setlist>(concat!(
            "SELECT ",
            setlist_columns!(),
            ", EXISTS(SELECT 1 FROM favorite_setlists f WHERE f.setlist_id = s.id AND f.user_id = $1) AS is_favorite,
            c.role AS collaborator_role
            FROM setlist_collaborators c
            INNER JOIN setlists s ON s.id = c.setlist_id
            WHERE c.user_id = $1 AND c.accepted_at IS NOT NULL
              AND s.deleted_at IS NULL AND s.band_id IS NULL
            ORDER BY is_favorite DESC, s.updated_at DESC, s.id ASC
            LIMIT $2 OFFSET $3"
        ))
        .bind(user_id)
        .bind(size)
        .bind(offset)
        .fetch_all(&self.db);

        let (count, setlists) = tokio::try_join!(count, setlists)?;
        Ok((setlists, count))
    }

    async fn find_all_for_band(
        &self,
        band_id: Uuid,
        user_id: Uuid,
        page: i64,
        size: i64,
    ) -> Result<(Vec<Setlist>, i64), ApiError> {
        let offset = (page - 1) * size;

        let count = sqlx::query_scalar(
            "SELECT COUNT(*) FROM setlists WHERE band_id = $1 AND deleted_at IS NULL;",
        )
        .bind(band_id)
        .fetch_one(&self.db);

        let setlists = sqlx::query_as::<_, Setlist>(concat!(
            "SELECT ",
            setlist_columns!(),
            ", EXISTS(SELECT 1 FROM favorite_setlists f WHERE f.setlist_id = s.id AND f.user_id = $4) AS is_favorite
            FROM setlists s
            WHERE s.band_id = $1 AND s.deleted_at IS NULL
            ORDER BY s.is_repertoire DESC, is_favorite DESC, LOWER(s.title) ASC, s.id ASC
            LIMIT $2 OFFSET $3"
        ))
        .bind(band_id)
        .bind(size)
        .bind(offset)
        .bind(user_id)
        .fetch_all(&self.db);

        let (count, setlists) = tokio::try_join!(count, setlists)?;
        Ok((setlists, count))
    }

    async fn find_by_id(&self, id: Uuid, user_id: Uuid) -> Result<Option<Setlist>, ApiError> {
        let setlist = sqlx::query_as::<_, Setlist>(concat!(
            "SELECT ",
            setlist_columns!(),
            ", EXISTS(SELECT 1 FROM favorite_setlists f WHERE f.setlist_id = s.id AND f.user_id = $2) AS is_favorite,
            (SELECT co.role FROM setlist_collaborators co
             WHERE co.setlist_id = s.id AND co.user_id = $2 AND co.accepted_at IS NOT NULL) AS collaborator_role
            FROM setlists s
            WHERE s.id = $1 AND s.deleted_at IS NULL AND ",
            visible_to_caller!()
        ))
        .bind(id)
        .bind(user_id)
        .fetch_optional(&self.db)
        .await?;
        Ok(setlist)
    }

    async fn create(
        &self,
        payload: &CreateSetlistPayload,
        user_id: Uuid,
        quota: &[QuotaGuard],
    ) -> Result<Setlist, ApiError> {
        let links = normalize_links(payload.links.as_deref().unwrap_or_default())?;
        let description = payload
            .description
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .map(str::to_string);
        let mut new_setlist =
            Setlist::new(payload.title.trim(), description, user_id, payload.band_id);
        new_setlist.links = Links::from_stored(links.clone());

        let mut tx = self.db.begin().await?;
        QuotaGuard::enforce_all(quota, &mut tx).await?;
        sqlx::query(
            "INSERT INTO setlists (id, title, description, user_id, band_id, links, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(new_setlist.id)
        .bind(&new_setlist.title)
        .bind(&new_setlist.description)
        .bind(new_setlist.user_id)
        .bind(new_setlist.band_id)
        .bind(sqlx::types::Json(&links))
        .bind(new_setlist.created_at)
        .bind(new_setlist.updated_at)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(new_setlist)
    }

    async fn update(
        &self,
        id: Uuid,
        payload: &UpdateSetlistPayload,
        actor_id: Uuid,
    ) -> Result<Uuid, ApiError> {
        let mut tx = self.db.begin().await?;
        let mut updated = false;

        let is_repertoire: bool = sqlx::query_scalar(
            "SELECT is_repertoire FROM setlists WHERE id = $1 AND deleted_at IS NULL FOR UPDATE",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ApiError::NotFound)?;

        if let Some(title) = &payload.title {
            if is_repertoire {
                return Err(repertoire_protected());
            }
            sqlx::query("UPDATE setlists SET title = $1 WHERE id = $2")
                .bind(title.trim())
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if let Some(description) = &payload.description {
            let description = description
                .as_deref()
                .map(str::trim)
                .filter(|d| !d.is_empty());
            sqlx::query("UPDATE setlists SET description = $1 WHERE id = $2")
                .bind(description)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if let Some(links) = &payload.links {
            let links = normalize_links(links)?;
            sqlx::query("UPDATE setlists SET links = $1 WHERE id = $2")
                .bind(sqlx::types::Json(&links))
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if !updated {
            return Err(ApiError::NotModified);
        }

        sqlx::query("UPDATE setlists SET updated_at = $1, updated_by = $2 WHERE id = $3")
            .bind(chrono::Utc::now().naive_utc())
            .bind(actor_id)
            .bind(id)
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;
        Ok(id)
    }

    async fn delete(&self, id: Uuid) -> Result<(), ApiError> {
        let is_repertoire: Option<bool> =
            sqlx::query_scalar("SELECT is_repertoire FROM setlists WHERE id = $1")
                .bind(id)
                .fetch_optional(&self.db)
                .await?;
        match is_repertoire {
            None => return Err(ApiError::NotFound),
            Some(true) => return Err(repertoire_protected()),
            Some(false) => {}
        }
        let result = sqlx::query("DELETE FROM setlists WHERE id = $1 AND NOT is_repertoire")
            .bind(id)
            .execute(&self.db)
            .await?;
        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }
        Ok(())
    }

    async fn trash(&self, id: Uuid, actor_id: Uuid) -> Result<(), ApiError> {
        let is_repertoire: Option<bool> = sqlx::query_scalar(
            "SELECT is_repertoire FROM setlists WHERE id = $1 AND deleted_at IS NULL",
        )
        .bind(id)
        .fetch_optional(&self.db)
        .await?;
        match is_repertoire {
            None => return Err(ApiError::NotFound),
            Some(true) => return Err(repertoire_protected()),
            Some(false) => {}
        }
        let result = sqlx::query(
            "UPDATE setlists SET deleted_at = $2, deleted_by = $3, trash_batch = $4
             WHERE id = $1 AND deleted_at IS NULL AND NOT is_repertoire",
        )
        .bind(id)
        .bind(chrono::Utc::now().naive_utc())
        .bind(actor_id)
        .bind(Uuid::new_v4())
        .execute(&self.db)
        .await?;
        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }
        Ok(())
    }

    async fn touch(&self, id: Uuid, actor_id: Uuid) -> Result<(), ApiError> {
        sqlx::query("UPDATE setlists SET updated_at = $1, updated_by = $2 WHERE id = $3")
            .bind(chrono::Utc::now().naive_utc())
            .bind(actor_id)
            .bind(id)
            .execute(&self.db)
            .await?;
        Ok(())
    }

    async fn find_any(&self, id: Uuid) -> Result<Option<Setlist>, ApiError> {
        let setlist = sqlx::query_as::<_, Setlist>(concat!(
            "SELECT ",
            setlist_columns!(),
            ", false AS is_favorite FROM setlists s WHERE s.id = $1 AND s.deleted_at IS NULL"
        ))
        .bind(id)
        .fetch_optional(&self.db)
        .await?;
        Ok(setlist)
    }

    async fn lock_sharing(
        &self,
        id: Uuid,
        actor_id: Uuid,
        reason: Option<&str>,
    ) -> Result<(), ApiError> {
        let mut tx = self.db.begin().await?;
        let result = sqlx::query(
            "UPDATE setlists
             SET share_token = NULL, share_locked_at = $1, share_locked_by = $2, share_lock_reason = $3
             WHERE id = $4",
        )
        .bind(chrono::Utc::now().naive_utc())
        .bind(actor_id)
        .bind(reason)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }
        // A public gig page would otherwise keep showing the setlist that
        // was just taken down.
        sqlx::query("UPDATE gigs SET share_token = NULL WHERE setlist_id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn unlock_sharing(&self, id: Uuid) -> Result<(), ApiError> {
        let result = sqlx::query(
            "UPDATE setlists
             SET share_locked_at = NULL, share_locked_by = NULL, share_lock_reason = NULL
             WHERE id = $1",
        )
        .bind(id)
        .execute(&self.db)
        .await?;
        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }
        Ok(())
    }

    async fn add_favorite(&self, setlist_id: Uuid, user_id: Uuid) -> Result<(), ApiError> {
        sqlx::query(
            "INSERT INTO favorite_setlists (user_id, setlist_id, created_at) VALUES ($1, $2, $3)
             ON CONFLICT (user_id, setlist_id) DO NOTHING",
        )
        .bind(user_id)
        .bind(setlist_id)
        .bind(chrono::Utc::now().naive_utc())
        .execute(&self.db)
        .await?;
        Ok(())
    }

    async fn remove_favorite(&self, setlist_id: Uuid, user_id: Uuid) -> Result<(), ApiError> {
        sqlx::query("DELETE FROM favorite_setlists WHERE user_id = $1 AND setlist_id = $2")
            .bind(user_id)
            .bind(setlist_id)
            .execute(&self.db)
            .await?;
        Ok(())
    }

    async fn is_unique(
        &self,
        title: &str,
        user_id: Uuid,
        band_id: Option<Uuid>,
        exclude_id: Option<Uuid>,
    ) -> Result<(), ApiError> {
        let exists = sqlx::query(
            "SELECT id FROM setlists
             WHERE title = $1 AND deleted_at IS NULL AND NOT is_repertoire
               AND (($2::uuid IS NOT NULL AND band_id = $2)
                    OR ($2::uuid IS NULL AND band_id IS NULL AND user_id = $3))
               AND ($4::uuid IS NULL OR id != $4)",
        )
        .bind(title.trim())
        .bind(band_id)
        .bind(user_id)
        .bind(exclude_id)
        .fetch_optional(&self.db)
        .await?
        .is_some();

        if exists {
            error!("Setlist '{title}' already exists in this scope.");
            Err(ApiError::AlreadyExists)
        } else {
            Ok(())
        }
    }

    async fn exists(&self, id: Uuid, user_id: Uuid) -> Result<(), ApiError> {
        let exists = sqlx::query(concat!(
            "SELECT s.id FROM setlists s WHERE s.id = $1 AND s.deleted_at IS NULL AND ",
            visible_to_caller!()
        ))
        .bind(id)
        .bind(user_id)
        .fetch_optional(&self.db)
        .await?
        .is_some();

        if !exists {
            error!("Setlist ID not found or unauthorized.");
            Err(ApiError::NotFound)
        } else {
            Ok(())
        }
    }

    async fn can_manage(&self, id: Uuid, user_id: Uuid) -> Result<(), ApiError> {
        self.check_permission(id, user_id, SetlistAction::Manage)
            .await
    }

    async fn can_edit_items(&self, id: Uuid, user_id: Uuid) -> Result<(), ApiError> {
        self.check_permission(id, user_id, SetlistAction::EditItems)
            .await
    }

    async fn can_edit_details(&self, id: Uuid, user_id: Uuid) -> Result<(), ApiError> {
        self.check_permission(id, user_id, SetlistAction::EditDetails)
            .await
    }

    async fn can_export_pdf(&self, id: Uuid, user_id: Uuid) -> Result<(), ApiError> {
        self.check_permission(id, user_id, SetlistAction::ExportPdf)
            .await
    }

    async fn enable_sharing(&self, id: Uuid) -> Result<Setlist, ApiError> {
        let locked: Option<Option<chrono::NaiveDateTime>> = sqlx::query_scalar(
            "SELECT share_locked_at FROM setlists WHERE id = $1 AND deleted_at IS NULL",
        )
        .bind(id)
        .fetch_optional(&self.db)
        .await?;

        match locked {
            None => return Err(ApiError::NotFound),
            Some(Some(_)) => {
                return Err(ApiError::rule(
                    StatusCode::FORBIDDEN,
                    codes::SHARE_LOCKED,
                    "Public sharing for this setlist was disabled by a moderator.",
                ));
            }
            Some(None) => {}
        }

        let token = crate::utils::share_token::generate_share_token();

        sqlx::query(
            "UPDATE setlists SET share_token = $1 WHERE id = $2 AND share_locked_at IS NULL",
        )
        .bind(&token)
        .bind(id)
        .execute(&self.db)
        .await?;

        // Re-fetch through the same query used by the public routes so the
        // returned `total_duration` is computed consistently, rather than
        // duplicating that aggregation here.
        self.find_by_share_token(&token)
            .await?
            .ok_or(ApiError::NotFound)
    }

    async fn disable_sharing(&self, id: Uuid) -> Result<(), ApiError> {
        let result = sqlx::query("UPDATE setlists SET share_token = NULL WHERE id = $1")
            .bind(id)
            .execute(&self.db)
            .await?;

        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }

        Ok(())
    }

    async fn find_by_share_token(&self, token: &str) -> Result<Option<Setlist>, ApiError> {
        let setlist = sqlx::query_as::<_, Setlist>(concat!(
            "SELECT ",
            setlist_columns!(),
            ", false AS is_favorite
            FROM setlists s
            WHERE s.share_token = $1 AND s.share_locked_at IS NULL AND s.deleted_at IS NULL"
        ))
        .bind(token)
        .fetch_optional(&self.db)
        .await?;

        Ok(setlist)
    }

    async fn add_song(
        &self,
        setlist_id: Uuid,
        song_id: Uuid,
        added_by: Uuid,
        quota: &[QuotaGuard],
    ) -> Result<(), ApiError> {
        let mut tx = self.db.begin().await?;

        // Serializes concurrent appends to the same setlist, so two songs
        // added at once can't land on the same position. Always the
        // setlist row first and the quota locks second, in the same order
        // as `create_marker` (the opposite order would deadlock a song
        // and a block being appended at once).
        let row: Option<(Option<Uuid>, bool)> = sqlx::query_as(
            "SELECT band_id, is_repertoire FROM setlists WHERE id = $1 AND deleted_at IS NULL FOR UPDATE",
        )
        .bind(setlist_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((band_id, is_repertoire)) = row else {
            return Err(ApiError::NotFound);
        };
        QuotaGuard::enforce_all(quota, &mut tx).await?;

        append_song(&mut tx, setlist_id, song_id, added_by).await?;

        // Every song a band plays belongs to its repertoire.
        if let (Some(band_id), false) = (band_id, is_repertoire) {
            let repertoire: Option<Uuid> = sqlx::query_scalar(
                "SELECT id FROM setlists WHERE band_id = $1 AND is_repertoire FOR UPDATE",
            )
            .bind(band_id)
            .fetch_optional(&mut *tx)
            .await?;
            if let Some(repertoire) = repertoire {
                // The band already plays this song in some key: a new
                // setlist starts with it (the repertoire is where a band
                // keeps "our" key for each song).
                sqlx::query(
                    "UPDATE setlist_songs AS ss SET transpose = r.transpose
                     FROM setlist_songs r
                     WHERE ss.setlist_id = $1 AND ss.song_id = $3
                       AND r.setlist_id = $2 AND r.song_id = $3",
                )
                .bind(setlist_id)
                .bind(repertoire)
                .bind(song_id)
                .execute(&mut *tx)
                .await?;
                append_song(&mut tx, repertoire, song_id, added_by).await?;
            }
        }

        tx.commit().await?;
        Ok(())
    }

    async fn has_song(&self, setlist_id: Uuid, song_id: Uuid) -> Result<bool, ApiError> {
        let exists =
            sqlx::query("SELECT 1 FROM setlist_songs WHERE setlist_id = $1 AND song_id = $2;")
                .bind(setlist_id)
                .bind(song_id)
                .fetch_optional(&self.db)
                .await?
                .is_some();

        Ok(exists)
    }

    async fn remove_song(&self, setlist_id: Uuid, song_id: Uuid) -> Result<(), ApiError> {
        // A linked song, or one the setlist holds (which is then gone).
        let removed: i64 = sqlx::query_scalar(
            "WITH linked AS (
                 DELETE FROM setlist_songs WHERE setlist_id = $1 AND song_id = $2 RETURNING 1
             ), held AS (
                 DELETE FROM setlist_held_songs WHERE setlist_id = $1 AND id = $2 RETURNING 1
             )
             SELECT (SELECT COUNT(*) FROM linked) + (SELECT COUNT(*) FROM held)",
        )
        .bind(setlist_id)
        .bind(song_id)
        .fetch_one(&self.db)
        .await?;

        if removed == 0 {
            return Err(ApiError::NotFound);
        }

        Ok(())
    }

    async fn set_song_transpose(
        &self,
        setlist_id: Uuid,
        song_id: Uuid,
        transpose: i16,
    ) -> Result<(), ApiError> {
        let updated: i64 = sqlx::query_scalar(
            "WITH linked AS (
                 UPDATE setlist_songs SET transpose = $3
                 WHERE setlist_id = $1 AND song_id = $2 RETURNING 1
             ), held AS (
                 UPDATE setlist_held_songs SET transpose = $3
                 WHERE setlist_id = $1 AND id = $2 RETURNING 1
             )
             SELECT (SELECT COUNT(*) FROM linked) + (SELECT COUNT(*) FROM held)",
        )
        .bind(setlist_id)
        .bind(song_id)
        .bind(transpose)
        .fetch_one(&self.db)
        .await?;

        if updated == 0 {
            return Err(ApiError::NotFound);
        }
        Ok(())
    }

    async fn get_songs(
        &self,
        setlist_id: Uuid,
        page: i64,
        size: i64,
    ) -> Result<(Vec<SongWithArtist>, i64), ApiError> {
        let offset = (page - 1) * size;

        let count = sqlx::query_scalar(concat!(
            "SELECT (SELECT COUNT(*) FROM setlist_songs ss
             INNER JOIN songs s ON s.id = ss.song_id
             INNER JOIN setlists st ON st.id = ss.setlist_id
             WHERE ss.setlist_id = $1 AND ",
            scoped_songs!(),
            ") + (SELECT COUNT(*) FROM setlist_held_songs WHERE setlist_id = $1)"
        ))
        .bind(setlist_id)
        .fetch_one(&self.db);

        let songs = sqlx::query_as::<_, SongWithArtist>(concat!(
            "SELECT x.* FROM (",
            setlist_song_rows!(),
            ") x ORDER BY x.position ASC LIMIT $2 OFFSET $3"
        ))
        .bind(setlist_id)
        .bind(size)
        .bind(offset)
        .fetch_all(&self.db);

        let (count, songs) = tokio::try_join!(count, songs)?;
        Ok((songs, count))
    }

    async fn get_positioned_songs(
        &self,
        setlist_id: Uuid,
        max_songs: i64,
    ) -> Result<Vec<(i32, SongWithArtist)>, ApiError> {
        // The public views never show a chart: the songs come back
        // without their lyrics and notes.
        Ok(self
            .song_summary_rows_limited(setlist_id, max_songs.max(0))
            .await?
            .into_iter()
            .map(|row| (row.position, row.song))
            .collect())
    }

    async fn count_items(&self, setlist_id: Uuid) -> Result<i64, ApiError> {
        Ok(sqlx::query_scalar(concat!(
            "SELECT (SELECT COUNT(*) FROM songs s
                     INNER JOIN setlist_songs ss ON s.id = ss.song_id
                     INNER JOIN setlists st ON st.id = ss.setlist_id
                     WHERE ss.setlist_id = $1 AND ",
            scoped_songs!(),
            ") + (SELECT COUNT(*) FROM setlist_held_songs WHERE setlist_id = $1)
               + (SELECT COUNT(*) FROM setlist_markers WHERE setlist_id = $1)"
        ))
        .bind(setlist_id)
        .fetch_one(&self.db)
        .await?)
    }

    async fn reorder_songs(&self, setlist_id: Uuid, song_ids: &[Uuid]) -> Result<(), ApiError> {
        sqlx::query(
            r#"
            WITH u AS (
                SELECT unnest($1::uuid[]) AS id,
                       generate_series(1, array_length($1::uuid[], 1)) AS new_position
            ), linked AS (
                UPDATE setlist_songs AS ss SET position = u.new_position
                FROM u WHERE ss.setlist_id = $2 AND ss.song_id = u.id
            )
            UPDATE setlist_held_songs AS h SET position = u.new_position
            FROM u WHERE h.setlist_id = $2 AND h.id = u.id
            "#,
        )
        .bind(song_ids)
        .bind(setlist_id)
        .execute(&self.db)
        .await
        .map_err(|e| {
            tracing::error!("Failed to reorder setlist songs: {e}");
            ApiError::DatabaseError(e)
        })?;

        Ok(())
    }

    async fn duplicate(
        &self,
        id: Uuid,
        user_id: Uuid,
        title_override: Option<String>,
        limits: Option<QuotaLimits>,
        quota: &[QuotaGuard],
    ) -> Result<(Setlist, i64), ApiError> {
        let original = self
            .find_by_id(id, user_id)
            .await?
            .ok_or(ApiError::NotFound)?;

        let base_title: String = title_override
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| format!("{} (copy)", original.title))
            .chars()
            .take(240)
            .collect();

        // The size check comes first, from a count: a repertoire can hold
        // thousands of songs with their lyrics, which must not be loaded
        // only to be refused.
        if let Some(limits) = limits
            && self.count_items(id).await? > limits.setlist_items
        {
            return Err(ApiError::quota_exceeded(
                "setlist_items",
                limits.setlist_items,
            ));
        }

        // Plain reads, before the write transaction.
        let source_songs = self.song_rows(id).await?;
        let source_markers = self.get_markers(id).await?;

        let mut tx = self.db.begin().await?;
        QuotaGuard::enforce_all(quota, &mut tx).await?;

        // Personal copy: try the requested title, then fall back to
        // "<title> (2)", "<title> (3)", ... rather than failing outright
        // on the very common case of duplicating the same setlist twice.
        let mut candidate = base_title.clone();
        let mut suffix = 2;
        let final_title = loop {
            let exists = sqlx::query(
                "SELECT id FROM setlists
                 WHERE title = $1 AND user_id = $2 AND band_id IS NULL AND deleted_at IS NULL",
            )
            .bind(&candidate)
            .bind(user_id)
            .fetch_optional(&mut *tx)
            .await?
            .is_some();

            if !exists {
                break candidate.clone();
            }

            candidate = format!("{base_title} ({suffix})");
            suffix += 1;

            if suffix > 50 {
                error!(%id, %user_id, "Too many duplicate titles when duplicating setlist.");
                return Err(ApiError::AlreadyExists);
            }
        };

        let mut new_setlist =
            Setlist::new(&final_title, original.description.clone(), user_id, None);
        new_setlist.links = original.links.clone();

        sqlx::query(
            "INSERT INTO setlists (id, title, description, user_id, band_id, links, created_at, updated_at)
             VALUES ($1, $2, $3, $4, NULL, $5, $6, $7)",
        )
        .bind(new_setlist.id)
        .bind(&new_setlist.title)
        .bind(&new_setlist.description)
        .bind(new_setlist.user_id)
        .bind(sqlx::types::Json(new_setlist.links.to_stored()))
        .bind(new_setlist.created_at)
        .bind(new_setlist.updated_at)
        .execute(&mut *tx)
        .await?;

        // The forker counts the caller's songs and artists once and adds
        // to the counts as it copies: serialized with every other creation
        // in those scopes (a concurrent copy or `POST /songs` would
        // otherwise pass on the same stale count).
        lock_scope(&mut tx, QuotaResource::Songs, user_id).await?;
        lock_scope(&mut tx, QuotaResource::Artists, user_id).await?;
        let mut forker = PersonalForker::new(&mut tx, user_id, limits).await?;
        let mut song_ids: Vec<Uuid> = Vec::with_capacity(source_songs.len());
        let mut positions: Vec<i32> = Vec::with_capacity(source_songs.len());
        let mut transposes: Vec<i16> = Vec::with_capacity(source_songs.len());
        let mut skipped = 0i64;
        for row in &source_songs {
            // The copy links the caller's own personal songs, and copies
            // band songs into their library (as a band setlist's copy
            // always has). Songs of anyone else's library, and the ones
            // the original holds, stay the copy's alone: held by it, never
            // added to the caller's library unasked.
            if row.song.held || (row.song.band_id.is_none() && row.song.user_id != user_id) {
                insert_held_song(
                    &mut tx,
                    new_setlist.id,
                    row.position,
                    &row.song,
                    Some(user_id),
                    new_setlist.created_at,
                )
                .await?;
                continue;
            }
            let resolved = if row.song.band_id.is_none() {
                Some(row.song.id)
            } else {
                forker.personal_copy(&mut tx, &row.song).await?
            };
            match resolved {
                Some(song_id) if !song_ids.contains(&song_id) => {
                    song_ids.push(song_id);
                    positions.push(row.position);
                    // The copy plays every song in the same key.
                    transposes.push(row.song.transpose.unwrap_or(0));
                }
                Some(_) => {}
                None => skipped += 1,
            }
        }

        if !song_ids.is_empty() {
            sqlx::query(
                "INSERT INTO setlist_songs (setlist_id, song_id, position, transpose, added_by, added_at)
                 SELECT $1, t.song_id, t.position, t.transpose, $5, $6
                 FROM UNNEST($2::uuid[], $3::int[], $4::smallint[]) AS t(song_id, position, transpose)
                 ON CONFLICT (setlist_id, song_id) DO NOTHING",
            )
            .bind(new_setlist.id)
            .bind(&song_ids)
            .bind(&positions)
            .bind(&transposes)
            .bind(user_id)
            .bind(new_setlist.created_at)
            .execute(&mut *tx)
            .await?;
        }

        for marker in &source_markers {
            sqlx::query(
                "INSERT INTO setlist_markers (id, setlist_id, marker_type, label, duration_minutes, position, created_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
            )
            .bind(Uuid::new_v4())
            .bind(new_setlist.id)
            .bind(marker.marker_type)
            .bind(&marker.label)
            .bind(marker.duration_minutes)
            .bind(marker.position)
            .bind(new_setlist.created_at)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;

        // Re-fetch so total_duration comes from the same computation as
        // every other read path.
        let setlist = self
            .find_by_id(new_setlist.id, user_id)
            .await?
            .ok_or(ApiError::NotFound)?;
        Ok((setlist, skipped))
    }

    async fn get_markers(&self, setlist_id: Uuid) -> Result<Vec<SetlistMarker>, ApiError> {
        let markers = sqlx::query_as::<_, SetlistMarker>(
            "SELECT id, setlist_id, marker_type, label, duration_minutes, position, created_at
             FROM setlist_markers WHERE setlist_id = $1 ORDER BY position ASC;",
        )
        .bind(setlist_id)
        .fetch_all(&self.db)
        .await?;

        Ok(markers)
    }

    async fn get_items(&self, setlist_id: Uuid) -> Result<Vec<SetlistItem>, ApiError> {
        let (song_rows, markers) =
            tokio::try_join!(self.song_rows(setlist_id), self.get_markers(setlist_id))?;
        Ok(merge_items(song_rows, markers))
    }

    async fn get_items_capped(
        &self,
        setlist_id: Uuid,
        max_items: usize,
    ) -> Result<Vec<SetlistItem>, ApiError> {
        let limit = i64::try_from(max_items).unwrap_or(i64::MAX - 1) + 1;
        let (song_rows, markers) = tokio::try_join!(
            self.song_rows_limited(setlist_id, Some(limit)),
            self.markers_limited(setlist_id, limit)
        )?;
        Ok(merge_items(song_rows, markers))
    }

    async fn get_items_capped_summary(
        &self,
        setlist_id: Uuid,
        max_items: usize,
    ) -> Result<Vec<SetlistItem>, ApiError> {
        let limit = i64::try_from(max_items).unwrap_or(i64::MAX - 1) + 1;
        let (song_rows, markers) = tokio::try_join!(
            self.song_summary_rows_limited(setlist_id, limit),
            self.markers_limited(setlist_id, limit)
        )?;
        Ok(merge_items(song_rows, markers))
    }

    async fn create_block(
        &self,
        setlist_id: Uuid,
        name: &str,
        quota: &[QuotaGuard],
    ) -> Result<SetlistMarker, ApiError> {
        self.create_marker(
            setlist_id,
            SetlistMarkerType::Block,
            Some(name.to_string()),
            None,
            quota,
        )
        .await
    }

    async fn update_block(
        &self,
        setlist_id: Uuid,
        marker_id: Uuid,
        name: &str,
    ) -> Result<SetlistMarker, ApiError> {
        self.update_marker(setlist_id, marker_id, Some(name.to_string()), None, false)
            .await
    }

    async fn create_break(
        &self,
        setlist_id: Uuid,
        label: Option<String>,
        duration_minutes: Option<i32>,
        quota: &[QuotaGuard],
    ) -> Result<SetlistMarker, ApiError> {
        self.create_marker(
            setlist_id,
            SetlistMarkerType::Break,
            label,
            duration_minutes,
            quota,
        )
        .await
    }

    async fn update_break(
        &self,
        setlist_id: Uuid,
        marker_id: Uuid,
        label: Option<String>,
        duration_minutes: Option<i32>,
    ) -> Result<SetlistMarker, ApiError> {
        self.update_marker(setlist_id, marker_id, label, duration_minutes, true)
            .await
    }

    async fn delete_marker(&self, setlist_id: Uuid, marker_id: Uuid) -> Result<(), ApiError> {
        let result = sqlx::query("DELETE FROM setlist_markers WHERE id = $1 AND setlist_id = $2;")
            .bind(marker_id)
            .bind(setlist_id)
            .execute(&self.db)
            .await?;

        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }

        Ok(())
    }

    async fn reorder_items(
        &self,
        setlist_id: Uuid,
        items: &[SetlistItemRef],
    ) -> Result<(), ApiError> {
        let song_ids: Vec<Uuid> = items
            .iter()
            .filter(|i| i.item_type == SetlistItemType::Song)
            .map(|i| i.id)
            .collect();
        let song_positions: Vec<i32> = items
            .iter()
            .enumerate()
            .filter(|(_, i)| i.item_type == SetlistItemType::Song)
            .map(|(idx, _)| idx as i32 + 1)
            .collect();

        let marker_ids: Vec<Uuid> = items
            .iter()
            .filter(|i| i.item_type != SetlistItemType::Song)
            .map(|i| i.id)
            .collect();
        let marker_positions: Vec<i32> = items
            .iter()
            .enumerate()
            .filter(|(_, i)| i.item_type != SetlistItemType::Song)
            .map(|(idx, _)| idx as i32 + 1)
            .collect();

        let mut tx = self.db.begin().await?;

        if !song_ids.is_empty() {
            sqlx::query(
                r#"
                WITH u AS (
                    SELECT unnest($1::uuid[]) AS id, unnest($2::int[]) AS new_position
                ), linked AS (
                    UPDATE setlist_songs AS ss SET position = u.new_position
                    FROM u WHERE ss.setlist_id = $3 AND ss.song_id = u.id
                )
                UPDATE setlist_held_songs AS h SET position = u.new_position
                FROM u WHERE h.setlist_id = $3 AND h.id = u.id
                "#,
            )
            .bind(&song_ids)
            .bind(&song_positions)
            .bind(setlist_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| {
                tracing::error!("Failed to reorder setlist songs: {e}");
                ApiError::DatabaseError(e)
            })?;
        }

        if !marker_ids.is_empty() {
            sqlx::query(
                r#"
                UPDATE setlist_markers AS sm
                SET position = u.new_position
                FROM (
                    SELECT unnest($1::uuid[]) AS id, unnest($2::int[]) AS new_position
                ) AS u
                WHERE sm.setlist_id = $3 AND sm.id = u.id
                "#,
            )
            .bind(&marker_ids)
            .bind(&marker_positions)
            .bind(setlist_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| {
                tracing::error!("Failed to reorder setlist markers: {e}");
                ApiError::DatabaseError(e)
            })?;
        }

        tx.commit().await?;

        Ok(())
    }

    async fn copy_song_to_library(
        &self,
        setlist_id: Uuid,
        song_id: Uuid,
        user_id: Uuid,
        limits: Option<QuotaLimits>,
        adopt: bool,
    ) -> Result<CopiedSetlistSong, ApiError> {
        let mut tx = self.db.begin().await?;
        // The setlist row first, like every other change to its order.
        sqlx::query("SELECT 1 FROM setlists WHERE id = $1 AND deleted_at IS NULL FOR UPDATE")
            .bind(setlist_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(ApiError::NotFound)?;

        let row = sqlx::query_as::<_, SongItemRow>(concat!(
            "SELECT x.* FROM (",
            setlist_song_rows!(),
            ") x WHERE x.id = $2"
        ))
        .bind(setlist_id)
        .bind(song_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ApiError::NotFound)?;
        let source = &row.song;

        if source.band_id.is_some() {
            // Band songs leave the band through a duplicate, which needs
            // the band's `export_pdf` permission.
            return Err(ApiError::NotFound);
        }
        if !source.held && source.user_id == user_id {
            return Err(ApiError::rule(
                StatusCode::CONFLICT,
                codes::SONG_ALREADY_IN_LIBRARY,
                "This song is already in your library.",
            ));
        }

        lock_scope(&mut tx, QuotaResource::Songs, user_id).await?;
        lock_scope(&mut tx, QuotaResource::Artists, user_id).await?;
        let mut forker = PersonalForker::new(&mut tx, user_id, limits).await?;
        let Some(copy_id) = forker.personal_copy(&mut tx, source).await? else {
            let limits = limits.expect("only limits refuse a copy");
            return Err(if forker.songs_used + 1 > limits.songs {
                ApiError::quota_exceeded("songs", limits.songs)
            } else {
                ApiError::quota_exceeded("artists", limits.artists)
            });
        };

        let adopted = adopt && source.held;
        if adopted {
            sqlx::query(
                "INSERT INTO setlist_songs (setlist_id, song_id, position, transpose, added_by, added_at)
                 SELECT setlist_id, $3, position, transpose, added_by, added_at
                 FROM setlist_held_songs WHERE setlist_id = $1 AND id = $2
                 ON CONFLICT (setlist_id, song_id) DO NOTHING",
            )
            .bind(setlist_id)
            .bind(song_id)
            .bind(copy_id)
            .execute(&mut *tx)
            .await?;
            sqlx::query("DELETE FROM setlist_held_songs WHERE setlist_id = $1 AND id = $2")
                .bind(setlist_id)
                .bind(song_id)
                .execute(&mut *tx)
                .await?;
        }

        tx.commit().await?;
        Ok(CopiedSetlistSong {
            song_id: copy_id,
            adopted,
        })
    }

    async fn find_repertoire_id(&self, band_id: Uuid) -> Result<Option<Uuid>, ApiError> {
        let id = sqlx::query_scalar("SELECT id FROM setlists WHERE band_id = $1 AND is_repertoire")
            .bind(band_id)
            .fetch_optional(&self.db)
            .await?;
        Ok(id)
    }

    async fn get_repertoire_songs(
        &self,
        band_id: Uuid,
        search: Option<&str>,
        page: i64,
        size: i64,
    ) -> Result<(Vec<SongWithArtist>, i64), ApiError> {
        let offset = (page - 1) * size;
        let search = search
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(like_pattern);

        let count = sqlx::query_scalar(concat!(
            "SELECT COUNT(*) FROM setlist_songs ss
             INNER JOIN setlists st ON st.id = ss.setlist_id
             INNER JOIN songs s ON s.id = ss.song_id
             INNER JOIN artists a ON a.id = s.artist_id
             WHERE st.band_id = $1 AND st.is_repertoire AND ",
            scoped_songs!(),
            " AND ($2::text IS NULL OR s.title ILIKE $2 OR a.name ILIKE $2)"
        ))
        .bind(band_id)
        .bind(&search)
        .fetch_one(&self.db);

        let songs = sqlx::query_as::<_, SongWithArtist>(concat!(
            "SELECT ",
            song_columns!(),
            ", a.name AS artist_name
             FROM setlist_songs ss
             INNER JOIN setlists st ON st.id = ss.setlist_id
             INNER JOIN songs s ON s.id = ss.song_id
             INNER JOIN artists a ON a.id = s.artist_id
             WHERE st.band_id = $1 AND st.is_repertoire AND ",
            scoped_songs!(),
            " AND ($2::text IS NULL OR s.title ILIKE $2 OR a.name ILIKE $2)
             ORDER BY LOWER(s.title) ASC, s.id ASC
             LIMIT $3 OFFSET $4"
        ))
        .bind(band_id)
        .bind(&search)
        .bind(size)
        .bind(offset)
        .fetch_all(&self.db);

        let (count, songs) = tokio::try_join!(count, songs)?;
        Ok((songs, count))
    }
}

#[derive(sqlx::FromRow)]
struct SongItemRow {
    position: i32,
    #[sqlx(flatten)]
    song: SongWithArtist,
}

/// Songs and markers merged into one running order, by position.
fn merge_items(song_rows: Vec<SongItemRow>, markers: Vec<SetlistMarker>) -> Vec<SetlistItem> {
    let mut items: Vec<SetlistItem> = song_rows
        .into_iter()
        .map(|r| SetlistItem::Song {
            position: r.position,
            song: Box::new(r.song),
        })
        .collect();

    items.extend(markers.into_iter().map(|m| match m.marker_type {
        SetlistMarkerType::Block => SetlistItem::Block {
            position: m.position,
            id: m.id,
            name: m.label.unwrap_or_default(),
        },
        SetlistMarkerType::Break => SetlistItem::Break {
            position: m.position,
            id: m.id,
            label: m.label,
            duration_minutes: m.duration_minutes,
        },
    }));

    items.sort_by_key(|i| match i {
        SetlistItem::Song { position, .. } => *position,
        SetlistItem::Block { position, .. } => *position,
        SetlistItem::Break { position, .. } => *position,
    });
    items
}

impl SetlistRepositoryImpl {
    /// Live songs of the setlist's own scope, with positions, in order.
    async fn song_rows(&self, setlist_id: Uuid) -> Result<Vec<SongItemRow>, ApiError> {
        self.song_rows_limited(setlist_id, None).await
    }

    /// The first `limit` of [`song_rows`](Self::song_rows) (`None` = all;
    /// `LIMIT NULL` is `LIMIT ALL` in Postgres).
    async fn song_rows_limited(
        &self,
        setlist_id: Uuid,
        limit: Option<i64>,
    ) -> Result<Vec<SongItemRow>, ApiError> {
        let rows = sqlx::query_as::<_, SongItemRow>(concat!(
            "SELECT x.* FROM (",
            setlist_song_rows!(),
            ") x ORDER BY x.position ASC LIMIT $2"
        ))
        .bind(setlist_id)
        .bind(limit)
        .fetch_all(&self.db)
        .await?;
        Ok(rows)
    }

    /// The first `limit` of the setlist's songs without their lyrics and
    /// notes (see [`setlist_song_summary_rows!`]).
    async fn song_summary_rows_limited(
        &self,
        setlist_id: Uuid,
        limit: i64,
    ) -> Result<Vec<SongItemRow>, ApiError> {
        let rows = sqlx::query_as::<_, SongItemRow>(concat!(
            "SELECT x.* FROM (",
            setlist_song_summary_rows!(),
            ") x ORDER BY x.position ASC LIMIT $2"
        ))
        .bind(setlist_id)
        .bind(limit)
        .fetch_all(&self.db)
        .await?;
        Ok(rows)
    }

    /// The first `limit` markers of the setlist, by position.
    async fn markers_limited(
        &self,
        setlist_id: Uuid,
        limit: i64,
    ) -> Result<Vec<SetlistMarker>, ApiError> {
        Ok(sqlx::query_as::<_, SetlistMarker>(
            "SELECT id, setlist_id, marker_type, label, duration_minutes, position, created_at
             FROM setlist_markers WHERE setlist_id = $1 ORDER BY position ASC LIMIT $2",
        )
        .bind(setlist_id)
        .bind(limit)
        .fetch_all(&self.db)
        .await?)
    }

    /// Whether `user_id` may do `action` to the setlist: its personal
    /// owner, a band member whose role clears the band's permission (admin
    /// and owner always do), or a collaborator with a high enough role.
    /// Trashed setlists are not found.
    async fn check_permission(
        &self,
        id: Uuid,
        user_id: Uuid,
        action: SetlistAction,
    ) -> Result<(), ApiError> {
        let permission = action.band_permission();
        let row = sqlx::query_as::<_, SetlistAccessRow>(
            r#"
            SELECT
                s.user_id AS owner_id,
                s.band_id,
                bm.role AS band_role,
                brp.allowed AS role_permission_allowed,
                co.role AS collaborator_role
            FROM setlists s
            LEFT JOIN band_members bm ON bm.band_id = s.band_id AND bm.user_id = $2
            LEFT JOIN band_role_permissions brp
                ON brp.band_id = s.band_id
                AND brp.role = bm.role
                AND brp.permission = $3::band_permission
            LEFT JOIN setlist_collaborators co
                ON co.setlist_id = s.id AND co.user_id = $2
                AND co.accepted_at IS NOT NULL AND s.band_id IS NULL
            WHERE s.id = $1 AND s.deleted_at IS NULL
            "#,
        )
        .bind(id)
        .bind(user_id)
        .bind(permission)
        .fetch_optional(&self.db)
        .await?
        .ok_or(ApiError::NotFound)?;

        // Admin/owner are always allowed; member/moderator follow the
        // band's configurable permission (defaulting to denied if no row
        // exists at all — e.g. a race with band creation).
        let allowed = match row.band_id {
            None => {
                row.owner_id == user_id
                    || match (row.collaborator_role, action.collaborator_minimum()) {
                        (Some(role), Some(minimum)) => role >= minimum,
                        _ => false,
                    }
            }
            Some(_) => match row.band_role {
                Some(role) if role.satisfies(BandRole::Admin) => true,
                Some(_) => row.role_permission_allowed.unwrap_or(false),
                None => false,
            },
        };

        let knows_it = match row.band_id {
            None => row.collaborator_role.is_some(),
            Some(_) => row.band_role.is_some(),
        };

        match (allowed, knows_it) {
            (true, _) => Ok(()),
            // Someone else's personal setlist, or a band the caller isn't
            // in: don't reveal that it exists.
            (false, false) => Err(ApiError::NotFound),
            (false, true) => {
                error!(%id, %user_id, ?action, "Caller lacks the permission for this setlist.");
                Err(ApiError::Forbidden)
            }
        }
    }

    async fn create_marker(
        &self,
        setlist_id: Uuid,
        marker_type: SetlistMarkerType,
        label: Option<String>,
        duration_minutes: Option<i32>,
        quota: &[QuotaGuard],
    ) -> Result<SetlistMarker, ApiError> {
        let id = Uuid::new_v4();
        let now = chrono::Utc::now().naive_utc();

        let mut tx = self.db.begin().await?;
        // Serializes appends to the setlist (positions and the repertoire
        // cap below are computed from what's there).
        let is_repertoire: bool = sqlx::query_scalar(
            "SELECT is_repertoire FROM setlists WHERE id = $1 AND deleted_at IS NULL FOR UPDATE",
        )
        .bind(setlist_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ApiError::NotFound)?;
        QuotaGuard::enforce_all(quota, &mut tx).await?;
        if is_repertoire {
            let markers: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM setlist_markers WHERE setlist_id = $1")
                    .bind(setlist_id)
                    .fetch_one(&mut *tx)
                    .await?;
            if markers >= MAX_REPERTOIRE_MARKERS {
                return Err(ApiError::quota_exceeded(
                    "repertoire_markers",
                    MAX_REPERTOIRE_MARKERS,
                ));
            }
        }

        // Append at the end of the shared song/marker ordering space.
        let next_position: i32 = sqlx::query_scalar(concat!("SELECT ", next_position!()))
            .bind(setlist_id)
            .fetch_one(&mut *tx)
            .await?;

        sqlx::query(
            "INSERT INTO setlist_markers (id, setlist_id, marker_type, label, duration_minutes, position, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(id)
        .bind(setlist_id)
        .bind(marker_type)
        .bind(&label)
        .bind(duration_minutes)
        .bind(next_position)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;

        Ok(SetlistMarker {
            id,
            setlist_id,
            marker_type,
            label,
            duration_minutes,
            position: next_position,
            created_at: now,
        })
    }

    async fn update_marker(
        &self,
        setlist_id: Uuid,
        marker_id: Uuid,
        label: Option<String>,
        duration_minutes: Option<i32>,
        allow_null_duration: bool,
    ) -> Result<SetlistMarker, ApiError> {
        let result = if allow_null_duration {
            sqlx::query(
                "UPDATE setlist_markers SET label = $1, duration_minutes = $2
                 WHERE id = $3 AND setlist_id = $4",
            )
            .bind(&label)
            .bind(duration_minutes)
            .bind(marker_id)
            .bind(setlist_id)
            .execute(&self.db)
            .await?
        } else {
            sqlx::query("UPDATE setlist_markers SET label = $1 WHERE id = $2 AND setlist_id = $3")
                .bind(&label)
                .bind(marker_id)
                .bind(setlist_id)
                .execute(&self.db)
                .await?
        };

        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }

        let marker = sqlx::query_as::<_, SetlistMarker>(
            "SELECT id, setlist_id, marker_type, label, duration_minutes, position, created_at
             FROM setlist_markers WHERE id = $1;",
        )
        .bind(marker_id)
        .fetch_one(&self.db)
        .await?;

        Ok(marker)
    }
}

/// Adds `song` to `setlist_id` as a song the setlist holds (see
/// `0018_setlist_held_songs.sql`), at `position`, in the key it is played
/// in there.
async fn insert_held_song(
    tx: &mut Transaction<'_, Postgres>,
    setlist_id: Uuid,
    position: i32,
    song: &SongWithArtist,
    added_by: Option<Uuid>,
    now: chrono::NaiveDateTime,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO setlist_held_songs (id, setlist_id, position, transpose, added_by, added_at,
             held_at, title, artist_name, version_label, tempo, lyrics, tonality, genre, duration,
             energy, time_signature, capo, tuning, performance_notes, links, tags, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17,
                 $18, $19, $20, $21, $6)",
    )
    .bind(Uuid::new_v4())
    .bind(setlist_id)
    .bind(position)
    .bind(song.transpose.unwrap_or(0))
    .bind(added_by)
    .bind(now)
    .bind(&song.title)
    .bind(&song.artist_name)
    .bind(&song.version_label)
    .bind(song.tempo)
    .bind(&song.lyrics)
    .bind(song.tonality)
    .bind(song.genre)
    .bind(song.duration)
    .bind(song.energy)
    .bind(&song.time_signature)
    .bind(song.capo)
    .bind(&song.tuning)
    .bind(&song.performance_notes)
    .bind(sqlx::types::Json(song.links.to_stored()))
    .bind(&song.tags)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Detaches the songs `from_user` contributed to other people's personal
/// setlists into those setlists, within `tx`: each becomes a song the
/// setlist holds (see `0018_setlist_held_songs.sql`), in the same place,
/// key and attribution, belonging to no one's library.
///
/// Called whenever the link between a setlist and a contributor's song is
/// about to break — they leave or are removed, move the song (or its
/// artist) to the trash, or delete their account — so what they
/// contributed never vanishes from someone else's show. Narrowed to one
/// setlist and/or some songs; only live personal songs are concerned.
/// Returns how many songs were detached.
pub(crate) async fn detach_contributed_songs(
    tx: &mut Transaction<'_, Postgres>,
    from_user: Uuid,
    setlist_id: Option<Uuid>,
    song_ids: Option<&[Uuid]>,
) -> Result<u64, ApiError> {
    let detached = sqlx::query(
        "WITH moved AS (
             DELETE FROM setlist_songs ss
             USING songs s, setlists st
             WHERE s.id = ss.song_id AND st.id = ss.setlist_id
               AND s.user_id = $1 AND s.band_id IS NULL AND s.deleted_at IS NULL
               AND st.band_id IS NULL AND st.user_id <> $1
               AND ($2::uuid IS NULL OR ss.setlist_id = $2)
               AND ($3::uuid[] IS NULL OR ss.song_id = ANY($3))
             RETURNING ss.setlist_id, ss.song_id, ss.position, ss.transpose, ss.added_by, ss.added_at
         )
         INSERT INTO setlist_held_songs (id, setlist_id, position, transpose, added_by, added_at,
             held_at, title, artist_name, version_label, tempo, lyrics, tonality, genre, duration,
             energy, time_signature, capo, tuning, performance_notes, links, tags, created_at)
         SELECT gen_random_uuid(), m.setlist_id, m.position, m.transpose, m.added_by, m.added_at,
                $4, s.title, a.name, s.version_label, s.tempo, s.lyrics, s.tonality, s.genre,
                s.duration, s.energy, s.time_signature, s.capo, s.tuning, s.performance_notes,
                s.links,
                COALESCE((SELECT array_agg(t.tag ORDER BY t.tag) FROM song_tags t WHERE t.song_id = s.id), '{}'),
                $4
         FROM moved m
         INNER JOIN songs s ON s.id = m.song_id
         INNER JOIN artists a ON a.id = s.artist_id",
    )
    .bind(from_user)
    .bind(setlist_id)
    .bind(song_ids)
    .bind(chrono::Utc::now().naive_utc())
    .execute(&mut **tx)
    .await?;
    Ok(detached.rows_affected())
}

/// Copies band songs into one user's personal library while duplicating a
/// setlist, within that transaction, honoring their song/artist quotas.
struct PersonalForker {
    user_id: Uuid,
    limits: Option<QuotaLimits>,
    songs_used: i64,
    artists_used: i64,
}

impl PersonalForker {
    async fn new(
        tx: &mut Transaction<'_, Postgres>,
        user_id: Uuid,
        limits: Option<QuotaLimits>,
    ) -> Result<Self, ApiError> {
        let (songs_used, artists_used): (i64, i64) = sqlx::query_as(
            "SELECT
                (SELECT COUNT(*) FROM songs WHERE user_id = $1 AND band_id IS NULL AND deleted_at IS NULL),
                (SELECT COUNT(*) FROM artists WHERE user_id = $1 AND band_id IS NULL AND deleted_at IS NULL)",
        )
        .bind(user_id)
        .fetch_one(&mut **tx)
        .await?;
        Ok(Self {
            user_id,
            limits,
            songs_used,
            artists_used,
        })
    }

    /// The caller's personal song matching `source` (same title, artist
    /// name and version label), created when missing. `None` when creating it would
    /// exceed a quota.
    async fn personal_copy(
        &mut self,
        tx: &mut Transaction<'_, Postgres>,
        source: &SongWithArtist,
    ) -> Result<Option<Uuid>, ApiError> {
        let artist: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM artists
             WHERE user_id = $1 AND band_id IS NULL AND deleted_at IS NULL AND LOWER(name) = LOWER($2)
             ORDER BY created_at LIMIT 1",
        )
        .bind(self.user_id)
        .bind(source.artist_name.trim())
        .fetch_optional(&mut **tx)
        .await?;

        if let Some(artist_id) = artist {
            let existing: Option<Uuid> = sqlx::query_scalar(
                "SELECT id FROM songs
                 WHERE user_id = $1 AND band_id IS NULL AND deleted_at IS NULL AND artist_id = $2
                   AND LOWER(TRIM(title)) = LOWER(TRIM($3))
                   AND LOWER(TRIM(COALESCE(version_label, ''))) = LOWER(TRIM(COALESCE($4, '')))
                 LIMIT 1",
            )
            .bind(self.user_id)
            .bind(artist_id)
            .bind(&source.title)
            .bind(&source.version_label)
            .fetch_optional(&mut **tx)
            .await?;
            if existing.is_some() {
                return Ok(existing);
            }
        }

        let (song_limit, artist_limit) = match self.limits {
            Some(limits) => (limits.songs, limits.artists),
            None => (i64::MAX, i64::MAX),
        };
        let needs_artist = artist.is_none();
        if self.songs_used + 1 > song_limit
            || (needs_artist && self.artists_used + 1 > artist_limit)
        {
            return Ok(None);
        }

        let artist_id = match artist {
            Some(id) => id,
            None => {
                let new =
                    crate::models::artist::Artist::new(source.artist_name.trim(), self.user_id);
                sqlx::query(
                    "INSERT INTO artists (id, name, user_id, created_at, updated_at) VALUES ($1, $2, $3, $4, $4)",
                )
                .bind(new.id)
                .bind(&new.name)
                .bind(new.user_id)
                .bind(new.created_at)
                .execute(&mut **tx)
                .await?;
                self.artists_used += 1;
                new.id
            }
        };

        let song = Song::fork_for_user(source, artist_id, self.user_id);
        sqlx::query(
            "INSERT INTO songs (id, title, artist_id, user_id, band_id, forked_from, tempo, lyrics, tonality, genre, duration,
                                energy, time_signature, capo, tuning, performance_notes, links, created_at, updated_at,
                                version_label, version_of)
             VALUES ($1, $2, $3, $4, NULL, NULL, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $16, $17,
                     -- A version joins the caller's own original of the song, if they have one.
                     CASE WHEN $17::text IS NULL THEN NULL ELSE
                         (SELECT o.id FROM songs o
                          WHERE o.user_id = $4 AND o.band_id IS NULL AND o.deleted_at IS NULL
                            AND o.artist_id = $3 AND o.version_label IS NULL
                            AND LOWER(TRIM(o.title)) = LOWER(TRIM($2))
                          LIMIT 1)
                     END)",
        )
        .bind(song.id)
        .bind(&song.title)
        .bind(song.artist_id)
        .bind(song.user_id)
        .bind(song.tempo)
        .bind(&song.lyrics)
        .bind(song.tonality)
        .bind(song.genre)
        .bind(song.duration)
        .bind(song.energy)
        .bind(&song.time_signature)
        .bind(song.capo)
        .bind(&song.tuning)
        .bind(&song.performance_notes)
        .bind(sqlx::types::Json(song.links.to_stored()))
        .bind(song.created_at)
        .bind(&song.version_label)
        .execute(&mut **tx)
        .await?;
        self.songs_used += 1;
        Ok(Some(song.id))
    }
}
