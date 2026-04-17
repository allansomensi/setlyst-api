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
    pub tempo: Option<i32>,
    pub lyrics: Option<String>,
    pub tonality: Option<Tonality>,
    pub genre: Option<Genre>,
    pub duration: Option<i32>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Deserialize, Serialize, ToSchema, Validate)]
pub struct CreateSongPayload {
    #[validate(length(min = 1, max = 255, message = "Title must be between 1 and 255 chars."))]
    pub title: String,
    pub artist_id: Uuid,
    #[validate(range(min = 1, max = 500, message = "Tempo must be a valid BPM."))]
    pub tempo: Option<i32>,
    pub lyrics: Option<String>,
    pub tonality: Option<Tonality>,
    pub genre: Option<Genre>,
    #[validate(range(min = 1, message = "Duration must be positive."))]
    pub duration: Option<i32>,
}

#[derive(Deserialize, Serialize, ToSchema, Validate)]
pub struct UpdateSongPayload {
    #[validate(length(min = 1, max = 255, message = "Title must be between 1 and 255 chars."))]
    pub title: Option<String>,
    pub artist_id: Option<Uuid>,
    #[validate(range(min = 1, max = 500, message = "Tempo must be a valid BPM."))]
    pub tempo: Option<i32>,
    pub lyrics: Option<String>,
    pub tonality: Option<Tonality>,
    pub genre: Option<Genre>,
    #[validate(range(min = 1, message = "Duration must be positive."))]
    pub duration: Option<i32>,
}

impl Song {
    pub fn new(payload: &CreateSongPayload, user_id: Uuid) -> Self {
        let now = Utc::now().naive_utc();
        Self {
            id: Uuid::new_v4(),
            title: payload.title.clone(),
            artist_id: payload.artist_id,
            user_id,
            tempo: payload.tempo,
            lyrics: payload.lyrics.clone(),
            tonality: payload.tonality,
            genre: payload.genre,
            duration: payload.duration,
            created_at: now,
            updated_at: now,
        }
    }
}
