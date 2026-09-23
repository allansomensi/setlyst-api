use chrono::{NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Type;
use sqlx::prelude::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

#[derive(ToSchema, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[sqlx(type_name = "song_tonality")]
pub enum Tonality {
    #[serde(rename = "C")]
    #[sqlx(rename = "C")]
    C,
    #[serde(rename = "C#")]
    #[sqlx(rename = "C#")]
    CSharp,
    #[serde(rename = "Db")]
    #[sqlx(rename = "Db")]
    Db,
    #[serde(rename = "D")]
    #[sqlx(rename = "D")]
    D,
    #[serde(rename = "D#")]
    #[sqlx(rename = "D#")]
    DSharp,
    #[serde(rename = "Eb")]
    #[sqlx(rename = "Eb")]
    Eb,
    #[serde(rename = "E")]
    #[sqlx(rename = "E")]
    E,
    #[serde(rename = "E#")]
    #[sqlx(rename = "E#")]
    ESharp,
    #[serde(rename = "F")]
    #[sqlx(rename = "F")]
    F,
    #[serde(rename = "F#")]
    #[sqlx(rename = "F#")]
    FSharp,
    #[serde(rename = "Gb")]
    #[sqlx(rename = "Gb")]
    Gb,
    #[serde(rename = "G")]
    #[sqlx(rename = "G")]
    G,
    #[serde(rename = "G#")]
    #[sqlx(rename = "G#")]
    GSharp,
    #[serde(rename = "Ab")]
    #[sqlx(rename = "Ab")]
    Ab,
    #[serde(rename = "A")]
    #[sqlx(rename = "A")]
    A,
    #[serde(rename = "A#")]
    #[sqlx(rename = "A#")]
    ASharp,
    #[serde(rename = "Bb")]
    #[sqlx(rename = "Bb")]
    Bb,
    #[serde(rename = "B")]
    #[sqlx(rename = "B")]
    B,
    #[serde(rename = "B#")]
    #[sqlx(rename = "B#")]
    BSharp,
    #[serde(rename = "Cm")]
    #[sqlx(rename = "Cm")]
    Cm,
    #[serde(rename = "C#m")]
    #[sqlx(rename = "C#m")]
    CSharpM,
    #[serde(rename = "Dbm")]
    #[sqlx(rename = "Dbm")]
    Dbm,
    #[serde(rename = "Dm")]
    #[sqlx(rename = "Dm")]
    Dm,
    #[serde(rename = "D#m")]
    #[sqlx(rename = "D#m")]
    DSharpM,
    #[serde(rename = "Ebm")]
    #[sqlx(rename = "Ebm")]
    Ebm,
    #[serde(rename = "Em")]
    #[sqlx(rename = "Em")]
    Em,
    #[serde(rename = "E#m")]
    #[sqlx(rename = "E#m")]
    ESharpM,
    #[serde(rename = "Fm")]
    #[sqlx(rename = "Fm")]
    Fm,
    #[serde(rename = "F#m")]
    #[sqlx(rename = "F#m")]
    FSharpM,
    #[serde(rename = "Gbm")]
    #[sqlx(rename = "Gbm")]
    Gbm,
    #[serde(rename = "Gm")]
    #[sqlx(rename = "Gm")]
    Gm,
    #[serde(rename = "G#m")]
    #[sqlx(rename = "G#m")]
    GSharpM,
    #[serde(rename = "Abm")]
    #[sqlx(rename = "Abm")]
    Abm,
    #[serde(rename = "Am")]
    #[sqlx(rename = "Am")]
    Am,
    #[serde(rename = "A#m")]
    #[sqlx(rename = "A#m")]
    ASharpM,
    #[serde(rename = "Bbm")]
    #[sqlx(rename = "Bbm")]
    Bbm,
    #[serde(rename = "Bm")]
    #[sqlx(rename = "Bm")]
    Bm,
    #[serde(rename = "B#m")]
    #[sqlx(rename = "B#m")]
    BSharpM,
}

#[derive(ToSchema, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[sqlx(type_name = "song_genre")]
pub enum Genre {
    #[serde(rename = "Acoustic")]
    #[sqlx(rename = "Acoustic")]
    Acoustic,
    #[serde(rename = "Alternative")]
    #[sqlx(rename = "Alternative")]
    Alternative,
    #[serde(rename = "Axe")]
    #[sqlx(rename = "Axe")]
    Axe,
    #[serde(rename = "Blues")]
    #[sqlx(rename = "Blues")]
    Blues,
    #[serde(rename = "BossaNova")]
    #[sqlx(rename = "BossaNova")]
    BossaNova,
    #[serde(rename = "Choro")]
    #[sqlx(rename = "Choro")]
    Choro,
    #[serde(rename = "Classical")]
    #[sqlx(rename = "Classical")]
    Classical,
    #[serde(rename = "Country")]
    #[sqlx(rename = "Country")]
    Country,
    #[serde(rename = "DeathMetal")]
    #[sqlx(rename = "DeathMetal")]
    DeathMetal,
    #[serde(rename = "Disco")]
    #[sqlx(rename = "Disco")]
    Disco,
    #[serde(rename = "Electronic")]
    #[sqlx(rename = "Electronic")]
    Electronic,
    #[serde(rename = "Emo")]
    #[sqlx(rename = "Emo")]
    Emo,
    #[serde(rename = "Folk")]
    #[sqlx(rename = "Folk")]
    Folk,
    #[serde(rename = "Forro")]
    #[sqlx(rename = "Forro")]
    Forro,
    #[serde(rename = "Funk")]
    #[sqlx(rename = "Funk")]
    Funk,
    #[serde(rename = "Gaucho")]
    #[sqlx(rename = "Gaucho")]
    Gaucho,
    #[serde(rename = "Gospel")]
    #[sqlx(rename = "Gospel")]
    Gospel,
    #[serde(rename = "Grunge")]
    #[sqlx(rename = "Grunge")]
    Grunge,
    #[serde(rename = "HardRock")]
    #[sqlx(rename = "HardRock")]
    HardRock,
    #[serde(rename = "HeavyMetal")]
    #[sqlx(rename = "HeavyMetal")]
    HeavyMetal,
    #[serde(rename = "HipHop")]
    #[sqlx(rename = "HipHop")]
    HipHop,
    #[serde(rename = "House")]
    #[sqlx(rename = "House")]
    House,
    #[serde(rename = "Indie")]
    #[sqlx(rename = "Indie")]
    Indie,
    #[serde(rename = "Jazz")]
    #[sqlx(rename = "Jazz")]
    Jazz,
    #[serde(rename = "KPop")]
    #[sqlx(rename = "KPop")]
    KPop,
    #[serde(rename = "Latin")]
    #[sqlx(rename = "Latin")]
    Latin,
    #[serde(rename = "LoFi")]
    #[sqlx(rename = "LoFi")]
    LoFi,
    #[serde(rename = "Metal")]
    #[sqlx(rename = "Metal")]
    Metal,
    #[serde(rename = "MPB")]
    #[sqlx(rename = "MPB")]
    MPB,
    #[serde(rename = "Pagode")]
    #[sqlx(rename = "Pagode")]
    Pagode,
    #[serde(rename = "Pop")]
    #[sqlx(rename = "Pop")]
    Pop,
    #[serde(rename = "PowerMetal")]
    #[sqlx(rename = "PowerMetal")]
    PowerMetal,
    #[serde(rename = "ProgressiveRock")]
    #[sqlx(rename = "ProgressiveRock")]
    ProgressiveRock,
    #[serde(rename = "PsychedelicRock")]
    #[sqlx(rename = "PsychedelicRock")]
    PsychedelicRock,
    #[serde(rename = "Punk")]
    #[sqlx(rename = "Punk")]
    Punk,
    #[serde(rename = "Reggae")]
    #[sqlx(rename = "Reggae")]
    Reggae,
    #[serde(rename = "Reggaeton")]
    #[sqlx(rename = "Reggaeton")]
    Reggaeton,
    #[serde(rename = "RnB")]
    #[sqlx(rename = "RnB")]
    RnB,
    #[serde(rename = "Rock")]
    #[sqlx(rename = "Rock")]
    Rock,
    #[serde(rename = "Samba")]
    #[sqlx(rename = "Samba")]
    Samba,
    #[serde(rename = "Sertanejo")]
    #[sqlx(rename = "Sertanejo")]
    Sertanejo,
    #[serde(rename = "Ska")]
    #[sqlx(rename = "Ska")]
    Ska,
    #[serde(rename = "Soul")]
    #[sqlx(rename = "Soul")]
    Soul,
    #[serde(rename = "SymphonicMetal")]
    #[sqlx(rename = "SymphonicMetal")]
    SymphonicMetal,
    #[serde(rename = "Techno")]
    #[sqlx(rename = "Techno")]
    Techno,
    #[serde(rename = "ThrashMetal")]
    #[sqlx(rename = "ThrashMetal")]
    ThrashMetal,
    #[serde(rename = "SoftRock")]
    #[sqlx(rename = "SoftRock")]
    SoftRock,
    #[serde(rename = "ClassicRock")]
    #[sqlx(rename = "ClassicRock")]
    ClassicRock,
    #[serde(rename = "PopRock")]
    #[sqlx(rename = "PopRock")]
    PopRock,
    #[serde(rename = "PowerBallad")]
    #[sqlx(rename = "PowerBallad")]
    PowerBallad,
    #[serde(rename = "FolkRock")]
    #[sqlx(rename = "FolkRock")]
    FolkRock,
    #[serde(rename = "ArenaRock")]
    #[sqlx(rename = "ArenaRock")]
    ArenaRock,
    #[serde(rename = "GarageRock")]
    #[sqlx(rename = "GarageRock")]
    GarageRock,
    #[serde(rename = "IndieRock")]
    #[sqlx(rename = "IndieRock")]
    IndieRock,
    #[serde(rename = "PostRock")]
    #[sqlx(rename = "PostRock")]
    PostRock,
    #[serde(rename = "SurfRock")]
    #[sqlx(rename = "SurfRock")]
    SurfRock,
    #[serde(rename = "GlamRock")]
    #[sqlx(rename = "GlamRock")]
    GlamRock,
    #[serde(rename = "StonerRock")]
    #[sqlx(rename = "StonerRock")]
    StonerRock,
    #[serde(rename = "SouthernRock")]
    #[sqlx(rename = "SouthernRock")]
    SouthernRock,
    #[serde(rename = "BluesRock")]
    #[sqlx(rename = "BluesRock")]
    BluesRock,
    #[serde(rename = "RockAndRoll")]
    #[sqlx(rename = "RockAndRoll")]
    RockAndRoll,
    #[serde(rename = "AlternativeRock")]
    #[sqlx(rename = "AlternativeRock")]
    AlternativeRock,
    #[serde(rename = "IndustrialRock")]
    #[sqlx(rename = "IndustrialRock")]
    IndustrialRock,
    #[serde(rename = "NuMetal")]
    #[sqlx(rename = "NuMetal")]
    NuMetal,
    #[serde(rename = "BlackMetal")]
    #[sqlx(rename = "BlackMetal")]
    BlackMetal,
    #[serde(rename = "DoomMetal")]
    #[sqlx(rename = "DoomMetal")]
    DoomMetal,
    #[serde(rename = "GrooveMetal")]
    #[sqlx(rename = "GrooveMetal")]
    GrooveMetal,
    #[serde(rename = "Metalcore")]
    #[sqlx(rename = "Metalcore")]
    Metalcore,
    #[serde(rename = "Deathcore")]
    #[sqlx(rename = "Deathcore")]
    Deathcore,
    #[serde(rename = "Grindcore")]
    #[sqlx(rename = "Grindcore")]
    Grindcore,
    #[serde(rename = "IndustrialMetal")]
    #[sqlx(rename = "IndustrialMetal")]
    IndustrialMetal,
    #[serde(rename = "GothicMetal")]
    #[sqlx(rename = "GothicMetal")]
    GothicMetal,
    #[serde(rename = "FolkMetal")]
    #[sqlx(rename = "FolkMetal")]
    FolkMetal,
    #[serde(rename = "PostPunk")]
    #[sqlx(rename = "PostPunk")]
    PostPunk,
    #[serde(rename = "PopPunk")]
    #[sqlx(rename = "PopPunk")]
    PopPunk,
    #[serde(rename = "SkaPunk")]
    #[sqlx(rename = "SkaPunk")]
    SkaPunk,
    #[serde(rename = "HardcorePunk")]
    #[sqlx(rename = "HardcorePunk")]
    HardcorePunk,
    #[serde(rename = "NewWave")]
    #[sqlx(rename = "NewWave")]
    NewWave,
    #[serde(rename = "Dance")]
    #[sqlx(rename = "Dance")]
    Dance,
    #[serde(rename = "EDM")]
    #[sqlx(rename = "EDM")]
    EDM,
    #[serde(rename = "DrumAndBass")]
    #[sqlx(rename = "DrumAndBass")]
    DrumAndBass,
    #[serde(rename = "Dubstep")]
    #[sqlx(rename = "Dubstep")]
    Dubstep,
    #[serde(rename = "Trance")]
    #[sqlx(rename = "Trance")]
    Trance,
    #[serde(rename = "Ambient")]
    #[sqlx(rename = "Ambient")]
    Ambient,
    #[serde(rename = "Chillout")]
    #[sqlx(rename = "Chillout")]
    Chillout,
    #[serde(rename = "Synthpop")]
    #[sqlx(rename = "Synthpop")]
    Synthpop,
    #[serde(rename = "Industrial")]
    #[sqlx(rename = "Industrial")]
    Industrial,
    #[serde(rename = "Trap")]
    #[sqlx(rename = "Trap")]
    Trap,
    #[serde(rename = "Drill")]
    #[sqlx(rename = "Drill")]
    Drill,
    #[serde(rename = "Afrobeat")]
    #[sqlx(rename = "Afrobeat")]
    Afrobeat,
    #[serde(rename = "Grime")]
    #[sqlx(rename = "Grime")]
    Grime,
    #[serde(rename = "FunkCarioca")]
    #[sqlx(rename = "FunkCarioca")]
    FunkCarioca,
    #[serde(rename = "Piseiro")]
    #[sqlx(rename = "Piseiro")]
    Piseiro,
    #[serde(rename = "Brega")]
    #[sqlx(rename = "Brega")]
    Brega,
    #[serde(rename = "Frevo")]
    #[sqlx(rename = "Frevo")]
    Frevo,
    #[serde(rename = "Arrocha")]
    #[sqlx(rename = "Arrocha")]
    Arrocha,
    #[serde(rename = "WorldMusic")]
    #[sqlx(rename = "WorldMusic")]
    WorldMusic,
    #[serde(rename = "Flamenco")]
    #[sqlx(rename = "Flamenco")]
    Flamenco,
    #[serde(rename = "Tango")]
    #[sqlx(rename = "Tango")]
    Tango,
    #[serde(rename = "Fado")]
    #[sqlx(rename = "Fado")]
    Fado,
    #[serde(rename = "Other")]
    #[sqlx(rename = "Other")]
    Other,
}

#[derive(ToSchema, Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Song {
    pub id: Uuid,
    pub title: String,
    pub artist_id: Uuid,
    pub user_id: Uuid,
    /// The band that owns this song as an independent, shared copy, or
    /// `None` for a personal song. Set only via [`Song::fork_for_band`],
    /// when a member contributes one of their own songs to a band setlist —
    /// this decouples the band's setlist from that member's personal
    /// catalog, so deleting (or editing) their own copy later never affects
    /// the band's.
    pub band_id: Option<Uuid>,
    /// The personal song this band-owned copy was forked from, or `None`
    /// for a personal song (or a fork whose source was later deleted — the
    /// link is cleared, not the copy).
    pub forked_from: Option<Uuid>,
    pub tempo: Option<i32>,
    pub lyrics: Option<String>,
    pub tonality: Option<Tonality>,
    pub genre: Option<Genre>,
    pub duration: Option<i32>,
    /// Normalized (lowercase) tags, alphabetically sorted.
    #[sqlx(default)]
    pub tags: Vec<String>,
    /// Who last changed the song (`None` if never edited since creation,
    /// or if that account was deleted).
    #[sqlx(default)]
    pub updated_by: Option<Uuid>,
    #[sqlx(default)]
    pub updated_by_username: Option<String>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

/// Longest song duration accepted, in seconds (2 hours).
pub const MAX_SONG_DURATION_SECS: i32 = 7_200;

#[derive(Deserialize, Serialize, ToSchema, Validate)]
pub struct CreateSongPayload {
    #[validate(length(min = 1, max = 255, message = "Title must be between 1 and 255 chars."))]
    pub title: String,
    pub artist_id: Uuid,
    #[validate(range(min = 1, max = 500, message = "Tempo must be a valid BPM."))]
    pub tempo: Option<i32>,
    #[validate(custom(function = "crate::validations::text::validate_lyrics"))]
    pub lyrics: Option<String>,
    pub tonality: Option<Tonality>,
    pub genre: Option<Genre>,
    #[validate(range(
        min = 1,
        max = 7200,
        message = "Duration must be between 1 second and 2 hours."
    ))]
    pub duration: Option<i32>,
    /// Free-form tags; normalized server-side (see `validations::tag`).
    pub tags: Option<Vec<String>>,
}

/// Every nullable field uses `Option<Option<T>>`: absent leaves it
/// unchanged, `null` clears it (see [`crate::models::patch`]).
#[derive(Deserialize, Serialize, ToSchema, Validate, Default)]
pub struct UpdateSongPayload {
    #[validate(length(min = 1, max = 255, message = "Title must be between 1 and 255 chars."))]
    pub title: Option<String>,
    pub artist_id: Option<Uuid>,
    #[serde(default, deserialize_with = "crate::models::patch::double_option")]
    #[validate(range(min = 1, max = 500, message = "Tempo must be a valid BPM."))]
    pub tempo: Option<Option<i32>>,
    #[serde(default, deserialize_with = "crate::models::patch::double_option")]
    #[validate(custom(function = "crate::validations::text::validate_lyrics"))]
    pub lyrics: Option<Option<String>>,
    #[serde(default, deserialize_with = "crate::models::patch::double_option")]
    pub tonality: Option<Option<Tonality>>,
    #[serde(default, deserialize_with = "crate::models::patch::double_option")]
    pub genre: Option<Option<Genre>>,
    #[serde(default, deserialize_with = "crate::models::patch::double_option")]
    #[validate(range(
        min = 1,
        max = 7200,
        message = "Duration must be between 1 second and 2 hours."
    ))]
    pub duration: Option<Option<i32>>,
    /// Replaces the song's whole tag set when present.
    pub tags: Option<Vec<String>>,
}

impl Song {
    pub fn new(payload: &CreateSongPayload, user_id: Uuid) -> Self {
        let now = Utc::now().naive_utc();
        Self {
            id: Uuid::new_v4(),
            title: payload.title.clone(),
            artist_id: payload.artist_id,
            user_id,
            band_id: None,
            forked_from: None,
            tempo: payload.tempo,
            lyrics: payload.lyrics.clone(),
            tonality: payload.tonality,
            genre: payload.genre,
            duration: payload.duration,
            tags: Vec::new(),
            updated_by: None,
            updated_by_username: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// Creates an independent, band-owned copy of `source`, pointing at
    /// `artist_id` (a band-owned artist resolved separately). `creator_id`
    /// is kept for audit purposes only, mirroring [`super::artist::Artist::new_for_band`].
    pub fn fork_for_band(
        source: &SongWithArtist,
        band_id: Uuid,
        artist_id: Uuid,
        creator_id: Uuid,
    ) -> Self {
        let now = Utc::now().naive_utc();
        Self {
            id: Uuid::new_v4(),
            title: source.title.clone(),
            artist_id,
            user_id: creator_id,
            band_id: Some(band_id),
            forked_from: Some(source.id),
            tempo: source.tempo,
            lyrics: source.lyrics.clone(),
            tonality: source.tonality,
            genre: source.genre,
            duration: source.duration,
            tags: source.tags.clone(),
            updated_by: None,
            updated_by_username: None,
            created_at: now,
            updated_at: now,
        }
    }
}

#[derive(Debug, Serialize, ToSchema, FromRow)]
pub struct SongExport {
    pub title: String,
    pub artist_name: Option<String>,
    pub tonality: Option<String>,
    pub tempo: Option<i32>,
    pub lyrics: Option<String>,
}

/// A [`Song`] with its artist's name resolved via a join, so callers don't
/// need a separate (and possibly ownership-scoped) artist lookup to display
/// it — e.g. a band setlist can contain songs owned by different members,
/// none of whom can see each other's `/artists` list.
#[derive(ToSchema, Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct SongWithArtist {
    pub id: Uuid,
    pub title: String,
    pub artist_id: Uuid,
    pub artist_name: String,
    pub user_id: Uuid,
    pub band_id: Option<Uuid>,
    pub forked_from: Option<Uuid>,
    pub tempo: Option<i32>,
    pub lyrics: Option<String>,
    pub tonality: Option<Tonality>,
    pub genre: Option<Genre>,
    pub duration: Option<i32>,
    #[sqlx(default)]
    pub tags: Vec<String>,
    #[sqlx(default)]
    pub updated_by: Option<Uuid>,
    #[sqlx(default)]
    pub updated_by_username: Option<String>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

/// One tag in the caller's vocabulary, with how many songs use it.
#[derive(ToSchema, Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct TagCount {
    pub tag: String,
    pub song_count: i64,
}

#[derive(Deserialize, Serialize, ToSchema, Validate)]
pub struct RenameTagPayload {
    #[validate(length(min = 1, max = 30, message = "Tags must be between 1 and 30 chars."))]
    pub new_name: String,
}

/// Filters for `GET /songs`.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SongListQuery {
    pub page: Option<i64>,
    pub per_page: Option<i64>,
    /// Case-insensitive search over title and artist name.
    pub q: Option<String>,
    /// Only songs carrying *all* of these tags (comma-separated).
    pub tags: Option<String>,
}
