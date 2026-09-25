//! Setlist and song PDF export.
//!
//! Produces a printable running order (and, optionally, a full songbook
//! with lyrics and chords) from a setlist, or a single song sheet. Everything a performer might
//! want to tune for a stage sheet is an option: what to show, typography
//! scale, a compact or two-column layout, paper size and orientation,
//! margins, page numbers and the Setlyst watermark.

use crate::models::setlist::SetlistItem;
use crate::models::song::{SongWithArtist, Tonality};
use chrono::Utc;
use genpdf::{
    Alignment, Document, Element, Margins, PageDecorator, Position, RenderResult, Size,
    elements::{self, LinearLayout, Paragraph, TableLayout},
    error::Error as PdfError,
    fonts, render,
    style::{Color, Style},
};
use serde::Deserialize;
use std::io::Cursor;
use std::sync::OnceLock;
use utoipa::{IntoParams, ToSchema};

// ---------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------

fn default_true() -> bool {
    true
}

fn default_columns() -> u8 {
    1
}

fn default_font_scale() -> u16 {
    100
}

/// How chords inside lyrics are printed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ChordMode {
    /// Lyrics only.
    Hide,
    /// Chords inline, in brackets: `[G]Hello`.
    Inline,
    /// Chords aligned above the syllable they fall on.
    #[default]
    Above,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum PaperFormat {
    #[default]
    A4,
    Letter,
    Legal,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Orientation {
    #[default]
    Portrait,
    Landscape,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum MarginSize {
    Narrow,
    #[default]
    Normal,
    Wide,
}

impl MarginSize {
    fn mm(self) -> f64 {
        match self {
            MarginSize::Narrow => 8.0,
            MarginSize::Normal => 15.0,
            MarginSize::Wide => 22.0,
        }
    }
}

/// Query parameters accepted by the PDF export endpoints. Every field is
/// optional; the defaults reproduce a clean, single-column stage sheet.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ExportQuery {
    /// Print the setlist title as a heading.
    #[serde(default = "default_true")]
    pub show_title: bool,
    /// Optional line under the title (event, venue, date...). Max 120 chars.
    pub subtitle: Option<String>,
    /// Print the setlist description under the title.
    #[serde(default)]
    pub show_description: bool,
    /// Print the band name (band setlists only).
    #[serde(default)]
    pub show_band_name: bool,
    /// Print the export date in the header.
    #[serde(default)]
    pub show_date: bool,
    #[serde(default)]
    pub show_total_duration: bool,
    /// Number the songs (1., 2., ...).
    #[serde(default = "default_true")]
    pub show_numbers: bool,
    #[serde(default)]
    pub show_artist: bool,
    #[serde(default)]
    pub show_key: bool,
    #[serde(default)]
    pub show_bpm: bool,
    /// Each song's own duration.
    #[serde(default)]
    pub show_song_duration: bool,
    #[serde(default)]
    pub show_tags: bool,
    // Blocks and breaks default to shown — they were already part of the
    // running order before these options existed, so an absent query
    // param must not silently hide them.
    #[serde(default = "default_true")]
    pub show_blocks: bool,
    #[serde(default = "default_true")]
    pub show_breaks: bool,
    /// Append a songbook with every song's lyrics after the running order.
    #[serde(default)]
    pub include_lyrics: bool,
    /// How chords are printed in the songbook.
    #[serde(default)]
    pub chords: ChordMode,
    /// Start every song of the songbook on a new page.
    #[serde(default)]
    pub page_break_per_song: bool,
    /// Tighter spacing, one line per song.
    #[serde(default)]
    pub compact: bool,
    /// 1 or 2 columns for the running order.
    #[serde(default = "default_columns")]
    pub columns: u8,
    /// Text size, in percent of the default (60–200).
    #[serde(default = "default_font_scale")]
    pub font_scale: u16,
    #[serde(default)]
    pub uppercase_titles: bool,
    /// Faint Setlyst watermark and footer credit.
    #[serde(default = "default_true")]
    pub watermark: bool,
    #[serde(default = "default_true")]
    pub page_numbers: bool,
    #[serde(default)]
    pub paper: PaperFormat,
    #[serde(default)]
    pub orientation: Orientation,
    #[serde(default)]
    pub margins: MarginSize,
    #[serde(default)]
    pub lang: PdfLocale,
}

/// Normalized, clamped export options.
#[derive(Debug, Clone)]
pub struct PdfExportOptions {
    pub show_title: bool,
    pub subtitle: Option<String>,
    pub show_description: bool,
    pub show_band_name: bool,
    pub show_date: bool,
    pub show_total_duration: bool,
    pub show_numbers: bool,
    pub show_artist: bool,
    pub show_key: bool,
    pub show_bpm: bool,
    pub show_song_duration: bool,
    pub show_tags: bool,
    pub show_blocks: bool,
    pub show_breaks: bool,
    pub include_lyrics: bool,
    pub chords: ChordMode,
    pub page_break_per_song: bool,
    pub compact: bool,
    pub columns: u8,
    pub font_scale: u16,
    pub uppercase_titles: bool,
    pub watermark: bool,
    pub page_numbers: bool,
    pub paper: PaperFormat,
    pub orientation: Orientation,
    pub margins: MarginSize,
    pub locale: PdfLocale,
}

impl PdfExportOptions {
    /// Options that need the `advanced_pdf` plan feature: two columns, the
    /// songbook (`include_lyrics`), no watermark, or non-default margins.
    /// Everything else (what to show, compact layout, text size, paper,
    /// orientation, chord mode, language, page numbers) is basic.
    pub fn is_advanced(&self) -> bool {
        self.columns == 2
            || self.include_lyrics
            || !self.watermark
            || self.margins != MarginSize::Normal
    }

    /// The same options with every advanced one reset to its default.
    pub fn to_basic(mut self) -> Self {
        self.columns = 1;
        self.include_lyrics = false;
        self.page_break_per_song = false;
        self.watermark = true;
        self.margins = MarginSize::Normal;
        self
    }

    /// The options a public share link may use: never the songbook, nor
    /// anything that only shapes it. Whoever holds the link gets the
    /// running order, never the lyrics (they may be someone's copyrighted
    /// or unpublished work).
    pub fn for_public_share(mut self) -> Self {
        self.include_lyrics = false;
        self.page_break_per_song = false;
        self.chords = ChordMode::Hide;
        self
    }
}

impl Default for PdfExportOptions {
    fn default() -> Self {
        serde_urlencoded::from_str::<ExportQuery>("")
            .map(Self::from)
            .expect("an empty query always deserializes")
    }
}

pub const MAX_SUBTITLE_LENGTH: usize = 120;

impl From<ExportQuery> for PdfExportOptions {
    fn from(query: ExportQuery) -> Self {
        let subtitle = query
            .subtitle
            .map(|s| {
                s.trim()
                    .chars()
                    .take(MAX_SUBTITLE_LENGTH)
                    .collect::<String>()
            })
            .filter(|s| !s.is_empty());

        Self {
            show_title: query.show_title,
            subtitle,
            show_description: query.show_description,
            show_band_name: query.show_band_name,
            show_date: query.show_date,
            show_total_duration: query.show_total_duration,
            show_numbers: query.show_numbers,
            show_artist: query.show_artist,
            show_key: query.show_key,
            show_bpm: query.show_bpm,
            show_song_duration: query.show_song_duration,
            show_tags: query.show_tags,
            show_blocks: query.show_blocks,
            show_breaks: query.show_breaks,
            include_lyrics: query.include_lyrics,
            chords: query.chords,
            page_break_per_song: query.page_break_per_song,
            compact: query.compact,
            columns: query.columns.clamp(1, 2),
            font_scale: query.font_scale.clamp(60, 200),
            uppercase_titles: query.uppercase_titles,
            watermark: query.watermark,
            page_numbers: query.page_numbers,
            paper: query.paper,
            orientation: query.orientation,
            margins: query.margins,
            locale: query.lang,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum PdfLocale {
    #[default]
    En,
    #[serde(rename = "pt-BR")]
    PtBr,
    Es,
}

struct PdfLabels {
    estimated_duration: &'static str,
    not_calculated: &'static str,
    key: &'static str,
    break_label: &'static str,
    page: &'static str,
    generated_with: &'static str,
    songs: &'static str,
    lyrics: &'static str,
    no_lyrics: &'static str,
    exported_on: &'static str,
    chorus: &'static str,
    verse: &'static str,
    bridge: &'static str,
    date_format: &'static str,
    capo: &'static str,
    tuning: &'static str,
    time_signature: &'static str,
    notes: &'static str,
}

impl PdfLocale {
    fn labels(&self) -> PdfLabels {
        match self {
            Self::PtBr => PdfLabels {
                estimated_duration: "Duração estimada",
                not_calculated: "Não calculada",
                key: "Tom",
                break_label: "Pausa",
                page: "Página",
                generated_with: "Gerado com Setlyst",
                songs: "músicas",
                lyrics: "Letras e cifras",
                no_lyrics: "Sem letra cadastrada.",
                exported_on: "Exportado em",
                chorus: "Refrão",
                verse: "Verso",
                bridge: "Ponte",
                date_format: "%d/%m/%Y",
                capo: "Capotraste",
                tuning: "Afinação",
                time_signature: "Compasso",
                notes: "Observações",
            },
            Self::En => PdfLabels {
                estimated_duration: "Estimated duration",
                not_calculated: "Not calculated",
                key: "Key",
                break_label: "Break",
                page: "Page",
                generated_with: "Made with Setlyst",
                songs: "songs",
                lyrics: "Lyrics & chords",
                no_lyrics: "No lyrics yet.",
                exported_on: "Exported on",
                chorus: "Chorus",
                verse: "Verse",
                bridge: "Bridge",
                date_format: "%Y-%m-%d",
                capo: "Capo",
                tuning: "Tuning",
                time_signature: "Time",
                notes: "Performance notes",
            },
            Self::Es => PdfLabels {
                estimated_duration: "Duración estimada",
                not_calculated: "No calculada",
                key: "Tono",
                break_label: "Pausa",
                page: "Página",
                generated_with: "Hecho con Setlyst",
                songs: "canciones",
                lyrics: "Letras y acordes",
                no_lyrics: "Sin letra registrada.",
                exported_on: "Exportado el",
                chorus: "Estribillo",
                verse: "Verso",
                bridge: "Puente",
                date_format: "%d/%m/%Y",
                capo: "Cejilla",
                tuning: "Afinación",
                time_signature: "Compás",
                notes: "Notas",
            },
        }
    }
}

// ---------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------

/// Everything the exporter needs to know about the setlist.
pub struct SetlistPdfData<'a> {
    pub title: &'a str,
    pub description: Option<&'a str>,
    pub band_name: Option<&'a str>,
    pub total_duration_secs: i32,
    pub items: &'a [SetlistItem],
}

// ---------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------

fn format_tonality(t: &Tonality) -> String {
    serde_json::to_value(t)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn format_duration(total_secs: i32) -> String {
    let total_secs = total_secs.max(0);
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;
    if hours > 0 {
        format!("{hours}h {minutes:02}m")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// A file name that is safe on every OS, derived from the setlist title.
pub fn pdf_filename(title: &str) -> String {
    slug_filename("setlist", title, "pdf")
}

/// `<prefix>-<slug>.<ext>` from a title (letters and digits kept, in any
/// script; everything else collapsed into single dashes; at most 80
/// characters), or `<prefix>.<ext>` when nothing is left.
pub fn slug_filename(prefix: &str, title: &str, ext: &str) -> String {
    let slug: String = title
        .trim()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_lowercase().next().unwrap_or(c)
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");

    let slug: String = slug.chars().take(80).collect();
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        format!("{prefix}.{ext}")
    } else {
        format!("{prefix}-{slug}.{ext}")
    }
}

/// A `Content-Disposition` value carrying both an ASCII fallback and the
/// RFC 5987 UTF-8 name, so accented titles ("Canções") keep their name
/// instead of the header silently being dropped.
pub fn content_disposition(filename: &str) -> String {
    let ascii: String = filename
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "-_.".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let encoded: String = filename
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{encoded}")
}

// ---------------------------------------------------------------------
// Chord / lyric parsing
// ---------------------------------------------------------------------

const QUALITY_TOKENS: [&str; 16] = [
    "maj", "min", "dim", "aug", "sus", "add", "m", "M", "°", "ø", "+", "-", "#", "b", "(", ")",
];

fn is_note(s: &str) -> Option<&str> {
    let mut chars = s.chars();
    let first = chars.next()?;
    if !('A'..='G').contains(&first) {
        return None;
    }
    let rest = chars.as_str();
    Some(
        rest.strip_prefix('#')
            .or_else(|| rest.strip_prefix('b'))
            .unwrap_or(rest),
    )
}

/// `true` for chord symbols like `G`, `F#m7`, `Bbmaj7`, `Asus4`, `D/F#`,
/// `C7(9)`, `N.C.`. Deliberately strict so lyric words ("And", "Come")
/// aren't mistaken for chords.
pub fn is_chord(token: &str) -> bool {
    let token = token.trim();
    if token.eq_ignore_ascii_case("n.c.") || token.eq_ignore_ascii_case("nc") {
        return true;
    }

    let (main, bass) = match token.rsplit_once('/') {
        Some((main, bass)) if !bass.is_empty() && !bass.chars().all(|c| c.is_ascii_digit()) => {
            (main, Some(bass))
        }
        _ => (token, None),
    };

    if let Some(bass) = bass
        && is_note(bass) != Some("")
    {
        return false;
    }

    let Some(mut rest) = is_note(main) else {
        return false;
    };

    while !rest.is_empty() {
        if let Some(stripped) = rest.strip_prefix(|c: char| c.is_ascii_digit() || c == '/') {
            rest = stripped;
            continue;
        }
        match QUALITY_TOKENS.iter().find(|q| rest.starts_with(**q)) {
            Some(q) => rest = &rest[q.len()..],
            None => return false,
        }
    }
    true
}

/// A neutral token allowed on a chord-only line (bar lines, repeats).
fn is_chord_line_filler(token: &str) -> bool {
    matches!(token, "|" | "||" | "-" | "/" | "%")
        || token
            .strip_prefix('x')
            .or_else(|| token.strip_suffix('x'))
            .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

fn is_chord_only_line(line: &str) -> bool {
    let mut has_chord = false;
    for token in line.split_whitespace() {
        let token = token.trim_matches(|c| c == '(' || c == ')');
        if is_chord(token) {
            has_chord = true;
        } else if !is_chord_line_filler(token) {
            return false;
        }
    }
    has_chord
}

/// One segment of a lyric line: an optional chord that starts at this
/// point, followed by the lyric text up to the next chord.
pub type ChordSegment = (Option<String>, String);

#[derive(Debug, Clone, PartialEq)]
pub enum LyricLine {
    Heading(String),
    Comment(String),
    Segments(Vec<ChordSegment>),
    Blank,
}

/// Splits `[G]Hello [C]world` into `[(Some(G), "Hello "), (Some(C), "world")]`.
/// Bracketed text that isn't a chord is kept as lyric text.
fn parse_inline_chords(line: &str) -> Vec<ChordSegment> {
    let mut segments: Vec<ChordSegment> = vec![(None, String::new())];
    let mut rest = line;

    while let Some(open) = rest.find('[') {
        let Some(close_rel) = rest[open..].find(']') else {
            break;
        };
        let close = open + close_rel;
        let inner = &rest[open + 1..close];

        segments
            .last_mut()
            .expect("never empty")
            .1
            .push_str(&rest[..open]);

        if is_chord(inner) {
            segments.push((Some(inner.trim().to_string()), String::new()));
        } else {
            segments
                .last_mut()
                .expect("never empty")
                .1
                .push_str(&rest[open..=close]);
        }
        rest = &rest[close + 1..];
    }
    segments.last_mut().expect("never empty").1.push_str(rest);

    if segments.len() > 1 && segments[0].0.is_none() && segments[0].1.is_empty() {
        segments.remove(0);
    }
    segments
}

/// Merges a chords-above line into the lyric line below it, by column.
fn merge_chord_line(chords: &str, lyric: &str) -> Vec<ChordSegment> {
    let lyric_chars: Vec<char> = lyric.chars().collect();
    let mut positions: Vec<(usize, String)> = Vec::new();

    let chord_chars: Vec<char> = chords.chars().collect();
    let mut i = 0;
    while i < chord_chars.len() {
        if chord_chars[i].is_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        while i < chord_chars.len() && !chord_chars[i].is_whitespace() {
            i += 1;
        }
        let token: String = chord_chars[start..i].iter().collect();
        let trimmed = token.trim_matches(|c| c == '(' || c == ')');
        if is_chord(trimmed) {
            positions.push((start, trimmed.to_string()));
        }
    }

    let mut segments: Vec<ChordSegment> = Vec::new();
    let mut cursor = 0usize;
    for (column, chord) in positions {
        let column = column.min(lyric_chars.len()).max(cursor);
        let text: String = lyric_chars[cursor..column].iter().collect();
        if segments.is_empty() {
            if !text.is_empty() {
                segments.push((None, text));
            }
        } else if let Some(last) = segments.last_mut() {
            last.1.push_str(&text);
        }
        segments.push((Some(chord), String::new()));
        cursor = column;
    }

    let tail: String = lyric_chars[cursor.min(lyric_chars.len())..]
        .iter()
        .collect();
    match segments.last_mut() {
        Some(last) => last.1.push_str(&tail),
        None => segments.push((None, tail)),
    }
    segments
}

fn strip_chords(segments: &[ChordSegment]) -> Vec<ChordSegment> {
    vec![(None, segments.iter().map(|(_, t)| t.as_str()).collect())]
}

fn inline_chords(segments: &[ChordSegment]) -> Vec<ChordSegment> {
    vec![(
        None,
        segments
            .iter()
            .map(|(chord, text)| match chord {
                Some(chord) => format!("[{chord}]{text}"),
                None => text.clone(),
            })
            .collect(),
    )]
}

fn directive(line: &str) -> Option<(String, String)> {
    let inner = line.trim().strip_prefix('{')?.strip_suffix('}')?;
    let (name, value) = match inner.split_once(':') {
        Some((name, value)) => (name, value),
        None => (inner, ""),
    };
    Some((name.trim().to_lowercase(), value.trim().to_string()))
}

/// Turns raw lyrics (ChordPro, Ultimate-Guitar headings or the
/// chords-above-lyrics layout) into printable lines.
pub fn parse_lyrics(raw: &str, mode: ChordMode, locale: PdfLocale) -> Vec<LyricLine> {
    let labels = locale.labels();
    let lines: Vec<&str> = raw.lines().map(|l| l.trim_end()).collect();
    let mut out: Vec<LyricLine> = Vec::new();
    let mut i = 0;

    let apply_mode = |segments: Vec<ChordSegment>| -> LyricLine {
        match mode {
            ChordMode::Above => LyricLine::Segments(segments),
            ChordMode::Inline => LyricLine::Segments(inline_chords(&segments)),
            ChordMode::Hide => LyricLine::Segments(strip_chords(&segments)),
        }
    };

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();
        i += 1;

        if trimmed.is_empty() {
            if out.last() != Some(&LyricLine::Blank) && !out.is_empty() {
                out.push(LyricLine::Blank);
            }
            continue;
        }

        if trimmed.starts_with('#') {
            continue;
        }

        if let Some((name, value)) = directive(trimmed) {
            let with_default = |default: &str| {
                if value.is_empty() {
                    default.to_string()
                } else {
                    value.clone()
                }
            };
            match name.as_str() {
                "c" | "comment" | "ci" | "comment_italic" | "cb" | "comment_box" | "highlight"
                    if !value.is_empty() =>
                {
                    out.push(LyricLine::Comment(value.clone()));
                }
                "soc" | "start_of_chorus" => {
                    out.push(LyricLine::Heading(with_default(labels.chorus)))
                }
                "sov" | "start_of_verse" => {
                    out.push(LyricLine::Heading(with_default(labels.verse)))
                }
                "sob" | "start_of_bridge" => {
                    out.push(LyricLine::Heading(with_default(labels.bridge)))
                }
                "chorus" => out.push(LyricLine::Heading(with_default(labels.chorus))),
                _ => {}
            }
            continue;
        }

        // `[Chorus]`-style heading: a whole line in brackets that isn't a chord.
        if let Some(inner) = trimmed.strip_prefix('[').and_then(|l| l.strip_suffix(']'))
            && !inner.contains('[')
            && !is_chord(inner)
        {
            out.push(LyricLine::Heading(inner.trim().to_string()));
            continue;
        }

        if is_chord_only_line(trimmed) {
            if mode == ChordMode::Hide {
                continue;
            }

            let next = lines.get(i).map(|l| l.trim_end());
            let next_is_lyric = next.is_some_and(|n| {
                let t = n.trim();
                !t.is_empty()
                    && !t.starts_with('{')
                    && !t.starts_with('#')
                    && !t.starts_with('[')
                    && !is_chord_only_line(t)
            });

            if let (true, Some(next)) = (next_is_lyric, next) {
                i += 1;
                out.push(apply_mode(merge_chord_line(line, next)));
            } else {
                let segments: Vec<ChordSegment> = trimmed
                    .split_whitespace()
                    .map(|token| (Some(token.to_string()), "    ".to_string()))
                    .collect();
                out.push(match mode {
                    ChordMode::Above => LyricLine::Segments(segments),
                    _ => LyricLine::Segments(vec![(None, trimmed.to_string())]),
                });
            }
            continue;
        }

        out.push(apply_mode(parse_inline_chords(line)));
    }

    while out.last() == Some(&LyricLine::Blank) {
        out.pop();
    }
    out
}

// ---------------------------------------------------------------------
// Custom elements
// ---------------------------------------------------------------------

/// A thin horizontal separator across the available width.
struct HorizontalRule {
    color: Color,
}

impl Element for HorizontalRule {
    fn render(
        &mut self,
        _context: &genpdf::Context,
        area: render::Area<'_>,
        style: Style,
    ) -> Result<RenderResult, PdfError> {
        let height = genpdf::Mm::from(3.0);
        if area.size().height < height {
            return Ok(RenderResult {
                size: Size::new(0, 0),
                has_more: true,
            });
        }
        let y = genpdf::Mm::from(1.5);
        area.draw_line(
            vec![Position::new(0, y), Position::new(area.size().width, y)],
            style.with_color(self.color),
        );
        Ok(RenderResult {
            size: Size::new(area.size().width, height),
            has_more: false,
        })
    }
}

/// A lyric line with chords printed above the exact point they fall on.
/// Positions are measured with the real font metrics, so alignment holds
/// with a proportional font. Lines too wide for the page fall back to a
/// wrapped paragraph with inline chords.
struct ChordLyricLine {
    segments: Vec<ChordSegment>,
    lyric_style: Style,
    chord_style: Style,
}

impl Element for ChordLyricLine {
    fn render(
        &mut self,
        context: &genpdf::Context,
        area: render::Area<'_>,
        style: Style,
    ) -> Result<RenderResult, PdfError> {
        let lyric_style = style.and(self.lyric_style);
        let chord_style = style.and(self.chord_style);
        let font_cache = &context.font_cache;

        let has_chords = self.segments.iter().any(|(c, _)| c.is_some());
        let has_text = self.segments.iter().any(|(_, t)| !t.trim().is_empty());
        let chord_height = if has_chords {
            chord_style.line_height(font_cache)
        } else {
            genpdf::Mm::from(0.0)
        };
        let lyric_height = if has_text || !has_chords {
            lyric_style.line_height(font_cache)
        } else {
            genpdf::Mm::from(0.0)
        };
        let height = chord_height + lyric_height;

        // Lay out: each chord starts where its lyric segment starts, but
        // never overlaps the previous chord.
        let space = chord_style.str_width(font_cache, " ");
        let mut x = genpdf::Mm::from(0.0);
        let mut last_chord_end = genpdf::Mm::from(0.0);
        let mut placements: Vec<(genpdf::Mm, Option<&str>, &str)> = Vec::new();
        for (chord, text) in &self.segments {
            let mut start = x;
            if let Some(chord) = chord {
                if start < last_chord_end {
                    start = last_chord_end;
                }
                last_chord_end = start + chord_style.str_width(font_cache, chord) + space;
            }
            placements.push((start, chord.as_deref(), text.as_str()));
            x = start + lyric_style.str_width(font_cache, text);
        }
        let total_width = x.max(last_chord_end);

        if total_width > area.size().width {
            let text: String = self
                .segments
                .iter()
                .map(|(chord, text)| match chord {
                    Some(chord) => format!("[{chord}]{text}"),
                    None => text.clone(),
                })
                .collect();
            let mut paragraph = Paragraph::new(text).styled(self.lyric_style);
            return paragraph.render(context, area, style);
        }

        if area.size().height < height {
            return Ok(RenderResult {
                size: Size::new(0, 0),
                has_more: true,
            });
        }

        for (start, chord, text) in placements {
            if let Some(chord) = chord {
                area.print_str(font_cache, Position::new(start, 0), chord_style, chord)?;
            }
            if !text.is_empty() {
                area.print_str(
                    font_cache,
                    Position::new(start, chord_height),
                    lyric_style,
                    text,
                )?;
            }
        }

        Ok(RenderResult {
            size: Size::new(total_width, height),
            has_more: false,
        })
    }
}

/// Margins, the optional watermark and the footer (credit + page number).
struct SetlystPageDecorator {
    page: usize,
    margins: f64,
    watermark: bool,
    page_numbers: bool,
    page_label: &'static str,
    credit: &'static str,
    font_scale: f64,
}

impl PageDecorator for SetlystPageDecorator {
    fn decorate_page<'a>(
        &mut self,
        context: &genpdf::Context,
        mut area: render::Area<'a>,
        style: Style,
    ) -> Result<render::Area<'a>, PdfError> {
        self.page += 1;
        area.add_margins(Margins::all(self.margins));

        let font_cache = &context.font_cache;
        let size = area.size();

        if self.watermark {
            // Drawn first, so page content prints over it.
            let mark_style = style
                .bold()
                .with_font_size(72)
                .with_color(Color::Rgb(243, 243, 243));
            let text = "SETLYST";
            let width = mark_style.str_width(font_cache, text);
            let x = (size.width - width) / 2.0;
            let y = size.height * 0.42;
            area.print_str(
                font_cache,
                Position::new(x.max(0.0.into()), y),
                mark_style,
                text,
            )?;
        }

        if self.watermark || self.page_numbers {
            let footer_size = ((8.0 * self.font_scale).round() as u8).clamp(6, 12);
            let footer_style = style
                .with_font_size(footer_size)
                .with_color(Color::Rgb(140, 140, 140));
            let line_height = footer_style.line_height(font_cache);
            let footer_height = line_height + genpdf::Mm::from(3.0);
            let footer_top = size.height - footer_height;

            area.draw_line(
                vec![
                    Position::new(0, footer_top),
                    Position::new(size.width, footer_top),
                ],
                style.with_color(Color::Rgb(225, 225, 225)),
            );

            let text_y = footer_top + genpdf::Mm::from(1.5);
            if self.watermark {
                area.print_str(
                    font_cache,
                    Position::new(0, text_y),
                    footer_style,
                    self.credit,
                )?;
            }
            if self.page_numbers {
                let label = format!("{} {}", self.page_label, self.page);
                let width = footer_style.str_width(font_cache, &label);
                area.print_str(
                    font_cache,
                    Position::new(size.width - width, text_y),
                    footer_style,
                    label,
                )?;
            }

            area.set_height(footer_top - genpdf::Mm::from(2.0));
        }

        Ok(area)
    }
}

// ---------------------------------------------------------------------
// Document assembly
// ---------------------------------------------------------------------

/// Font files every PDF needs (the Inter family, in `<assets>/fonts`).
pub const REQUIRED_FONT_FILES: [&str; 4] = [
    "Inter-Regular.ttf",
    "Inter-Bold.ttf",
    "Inter-Italic.ttf",
    "Inter-BoldItalic.ttf",
];

/// Where the PDF fonts are read from: `PDF_FONTS_DIR` when set (kept for
/// existing deployments), else `fonts` inside the configured assets
/// directory (`ASSETS_DIR`, see `config::resolve_assets_dir`). Never a
/// path relative to the working directory alone, so the binary can be
/// started from anywhere.
pub fn fonts_dir() -> std::path::PathBuf {
    if let Some(dir) = std::env::var("PDF_FONTS_DIR")
        .ok()
        .filter(|d| !d.trim().is_empty())
    {
        return dir.into();
    }
    match crate::config::Config::try_get() {
        Some(config) => config.assets_dir.join("fonts"),
        None => crate::config::resolve_assets_dir(None).join("fonts"),
    }
}

/// The required font files missing from [`fonts_dir`] (empty when PDF
/// export can work). Checked at startup so a bad deployment is reported
/// right away rather than on the first export.
pub fn missing_fonts() -> Vec<std::path::PathBuf> {
    let dir = fonts_dir();
    REQUIRED_FONT_FILES
        .iter()
        .map(|f| dir.join(f))
        .filter(|p| !p.is_file())
        .collect()
}

fn font_family() -> Result<fonts::FontFamily<fonts::FontData>, PdfError> {
    static FONTS: OnceLock<fonts::FontFamily<fonts::FontData>> = OnceLock::new();
    if let Some(family) = FONTS.get() {
        return Ok(family.clone());
    }
    let family = fonts::from_files(fonts_dir(), "Inter", None)?;
    Ok(FONTS.get_or_init(|| family).clone())
}

struct Styles {
    scale: f64,
}

impl Styles {
    fn size(&self, base: f64) -> u8 {
        (base * self.scale).round().clamp(6.0, 72.0) as u8
    }

    fn text(&self, base: f64) -> Style {
        Style::new().with_font_size(self.size(base))
    }

    fn muted(&self, base: f64) -> Style {
        self.text(base).with_color(Color::Rgb(95, 95, 95))
    }
}

fn song_title_text(song: &SongWithArtist, number: usize, options: &PdfExportOptions) -> String {
    let title = if options.uppercase_titles {
        song.title.to_uppercase()
    } else {
        song.title.clone()
    };
    if options.show_numbers {
        format!("{number}. {title}")
    } else {
        title
    }
}

fn song_meta_parts(song: &SongWithArtist, options: &PdfExportOptions) -> Vec<String> {
    let labels = options.locale.labels();
    let mut parts = Vec::new();
    if options.show_artist {
        parts.push(song.artist_name.clone());
    }
    if options.show_key
        && let Some(key) = &song.tonality
    {
        parts.push(format!("{}: {}", labels.key, format_tonality(key)));
    }
    if options.show_bpm
        && let Some(bpm) = song.tempo
    {
        parts.push(format!("{bpm} BPM"));
    }
    if options.show_song_duration
        && let Some(duration) = song.duration
    {
        parts.push(format_duration(duration));
    }
    if options.show_tags && !song.tags.is_empty() {
        parts.push(
            song.tags
                .iter()
                .map(|t| format!("#{t}"))
                .collect::<Vec<_>>()
                .join(" "),
        );
    }
    parts
}

/// One entry of the running order, as a self-contained element (so it can
/// be placed either in the flow or in a table cell).
fn running_order_entry(
    item: &SetlistItem,
    number: usize,
    options: &PdfExportOptions,
    styles: &Styles,
) -> Option<LinearLayout> {
    let labels = options.locale.labels();
    let mut layout = LinearLayout::vertical();

    match item {
        SetlistItem::Song { song, .. } => {
            let meta = song_meta_parts(song, options);
            if options.compact {
                let mut paragraph = Paragraph::default();
                paragraph.push_styled(
                    song_title_text(song, number, options),
                    styles.text(12.0).bold(),
                );
                if !meta.is_empty() {
                    paragraph.push_styled(format!("   {}", meta.join("  ·  ")), styles.muted(10.0));
                }
                layout.push(elements::PaddedElement::new(
                    paragraph,
                    Margins::trbl(0.6, 0, 0.6, 0),
                ));
            } else {
                layout.push(
                    Paragraph::new(song_title_text(song, number, options))
                        .styled(styles.text(16.0).bold()),
                );
                if !meta.is_empty() {
                    layout.push(Paragraph::new(meta.join("  ·  ")).styled(styles.muted(11.0)));
                }
                layout.push(elements::Break::new(0.6));
            }
        }
        SetlistItem::Block { name, .. } => {
            if !options.show_blocks {
                return None;
            }
            let name = if options.uppercase_titles {
                name.to_uppercase()
            } else {
                name.clone()
            };
            let (before, size) = if options.compact {
                (0.4, 11.5)
            } else {
                (0.8, 14.0)
            };
            layout.push(elements::Break::new(before));
            layout.push(
                Paragraph::new(name)
                    .styled(styles.text(size).bold().with_color(Color::Rgb(40, 40, 40))),
            );
            layout.push(HorizontalRule {
                color: Color::Rgb(200, 200, 200),
            });
        }
        SetlistItem::Break {
            label,
            duration_minutes,
            ..
        } => {
            if !options.show_breaks {
                return None;
            }
            let text = match (label, duration_minutes) {
                (Some(label), Some(minutes)) => format!("— {label} ({minutes} min) —"),
                (Some(label), None) => format!("— {label} —"),
                (None, Some(minutes)) => format!("— {} ({minutes} min) —", labels.break_label),
                (None, None) => format!("— {} —", labels.break_label),
            };
            let size = if options.compact { 10.0 } else { 12.0 };
            layout.push(
                Paragraph::new(text).styled(
                    styles
                        .text(size)
                        .italic()
                        .with_color(Color::Rgb(120, 120, 120)),
                ),
            );
            layout.push(elements::Break::new(if options.compact {
                0.3
            } else {
                0.6
            }));
        }
    }

    Some(layout)
}

fn push_header(
    doc: &mut Document,
    data: &SetlistPdfData<'_>,
    options: &PdfExportOptions,
    styles: &Styles,
) {
    let labels = options.locale.labels();
    let mut pushed = false;

    if options.show_title {
        let title = if options.uppercase_titles {
            data.title.to_uppercase()
        } else {
            data.title.to_string()
        };
        doc.push(
            Paragraph::new(title).aligned(Alignment::Center).styled(
                styles
                    .text(if options.compact { 20.0 } else { 24.0 })
                    .bold(),
            ),
        );
        pushed = true;
    }

    if let Some(subtitle) = &options.subtitle {
        doc.push(
            Paragraph::new(subtitle.clone())
                .aligned(Alignment::Center)
                .styled(styles.text(13.0)),
        );
        pushed = true;
    }

    let mut info: Vec<String> = Vec::new();
    if options.show_band_name
        && let Some(band) = data.band_name
    {
        info.push(band.to_string());
    }
    if options.show_total_duration {
        let songs = data
            .items
            .iter()
            .filter(|i| matches!(i, SetlistItem::Song { .. }))
            .count();
        let duration = if data.total_duration_secs > 0 {
            format_duration(data.total_duration_secs)
        } else {
            labels.not_calculated.to_string()
        };
        info.push(format!(
            "{songs} {}  ·  {}: {duration}",
            labels.songs, labels.estimated_duration
        ));
    }
    if options.show_date {
        info.push(format!(
            "{} {}",
            labels.exported_on,
            Utc::now().format(labels.date_format)
        ));
    }
    if !info.is_empty() {
        doc.push(
            Paragraph::new(info.join("   ·   "))
                .aligned(Alignment::Center)
                .styled(styles.muted(10.5)),
        );
        pushed = true;
    }

    if options.show_description
        && let Some(description) = data.description.map(str::trim).filter(|d| !d.is_empty())
    {
        doc.push(elements::Break::new(0.4));
        for line in description.lines() {
            doc.push(
                Paragraph::new(line.to_string())
                    .aligned(Alignment::Center)
                    .styled(styles.muted(10.5).italic()),
            );
        }
        pushed = true;
    }

    if pushed {
        doc.push(elements::Break::new(0.5));
        doc.push(HorizontalRule {
            color: Color::Rgb(210, 210, 210),
        });
        doc.push(elements::Break::new(if options.compact {
            0.3
        } else {
            0.8
        }));
    }
}

fn push_running_order(
    doc: &mut Document,
    data: &SetlistPdfData<'_>,
    options: &PdfExportOptions,
    styles: &Styles,
) -> Result<(), PdfError> {
    let mut entries: Vec<LinearLayout> = Vec::new();
    let mut number = 0usize;
    for item in data.items {
        if matches!(item, SetlistItem::Song { .. }) {
            number += 1;
        }
        if let Some(entry) = running_order_entry(item, number, options, styles) {
            entries.push(entry);
        }
    }

    if options.columns == 2 && entries.len() > 1 {
        // Column-major, like a printed program: read down the left column,
        // then the right one.
        let rows = entries.len().div_ceil(2);
        let mut right = entries.split_off(rows);
        right.reverse();
        let mut table = TableLayout::new(vec![1, 1]);
        for left in entries {
            let mut row = table.row();
            row.push_element(elements::PaddedElement::new(
                left,
                Margins::trbl(0, 4, 0, 0),
            ));
            match right.pop() {
                Some(right) => row.push_element(elements::PaddedElement::new(
                    right,
                    Margins::trbl(0, 0, 0, 4),
                )),
                None => row.push_element(elements::Break::new(0)),
            }
            row.push()?;
        }
        doc.push(table);
    } else {
        for entry in entries {
            doc.push(entry);
        }
    }
    Ok(())
}

fn push_songbook(
    doc: &mut Document,
    data: &SetlistPdfData<'_>,
    options: &PdfExportOptions,
    styles: &Styles,
) {
    let labels = options.locale.labels();
    let songs: Vec<&SongWithArtist> = data
        .items
        .iter()
        .filter_map(|item| match item {
            SetlistItem::Song { song, .. } => Some(song.as_ref()),
            _ => None,
        })
        .collect();

    if songs.is_empty() {
        return;
    }

    doc.push(elements::PageBreak::new());
    doc.push(
        Paragraph::new(labels.lyrics)
            .aligned(Alignment::Center)
            .styled(styles.text(18.0).bold()),
    );
    doc.push(elements::Break::new(0.8));

    let lyric_style = styles.text(if options.compact { 10.0 } else { 11.5 });
    let chord_style = styles
        .text(if options.compact { 9.0 } else { 10.5 })
        .bold()
        .with_color(Color::Rgb(30, 90, 170));

    for (index, song) in songs.into_iter().enumerate() {
        if index > 0 {
            if options.page_break_per_song {
                doc.push(elements::PageBreak::new());
            } else {
                doc.push(elements::Break::new(1.0));
                doc.push(HorizontalRule {
                    color: Color::Rgb(220, 220, 220),
                });
            }
        }

        doc.push(
            Paragraph::new(song_title_text(song, index + 1, options))
                .styled(styles.text(15.0).bold()),
        );
        let mut meta = vec![song.artist_name.clone()];
        if let Some(key) = &song.tonality {
            meta.push(format!("{}: {}", labels.key, format_tonality(key)));
        }
        if let Some(bpm) = song.tempo {
            meta.push(format!("{bpm} BPM"));
        }
        doc.push(Paragraph::new(meta.join("  ·  ")).styled(styles.muted(10.0)));
        doc.push(elements::Break::new(0.5));

        let lines = song
            .lyrics
            .as_deref()
            .map(|lyrics| parse_lyrics(lyrics, options.chords, options.locale))
            .unwrap_or_default();

        if lines.is_empty() {
            doc.push(Paragraph::new(labels.no_lyrics).styled(styles.muted(10.5).italic()));
            continue;
        }

        let mut layout = LinearLayout::vertical();
        push_lyric_lines(&mut layout, lines, styles, lyric_style, chord_style);
        doc.push(layout);
    }
}

/// Appends printable lyric lines to `layout`.
fn push_lyric_lines(
    layout: &mut LinearLayout,
    lines: Vec<LyricLine>,
    styles: &Styles,
    lyric_style: Style,
    chord_style: Style,
) {
    for line in lines {
        match line {
            LyricLine::Blank => layout.push(elements::Break::new(0.5)),
            LyricLine::Heading(text) => {
                layout.push(elements::Break::new(0.3));
                layout.push(Paragraph::new(text).styled(styles.text(11.0).bold()));
            }
            LyricLine::Comment(text) => {
                layout.push(Paragraph::new(text).styled(styles.muted(10.5).italic()));
            }
            LyricLine::Segments(segments) => layout.push(ChordLyricLine {
                segments,
                lyric_style,
                chord_style,
            }),
        }
    }
}

/// Page setup shared by every export.
struct PageSetup {
    paper: PaperFormat,
    orientation: Orientation,
    margins: MarginSize,
    watermark: bool,
    page_numbers: bool,
    font_scale: u16,
    locale: PdfLocale,
    compact: bool,
}

fn new_document(title: String, setup: &PageSetup) -> Result<(Document, Styles), PdfError> {
    let mut doc = Document::new(font_family()?);
    doc.set_title(title);
    doc.set_line_spacing(if setup.compact { 1.1 } else { 1.25 });

    let paper: Size = match setup.paper {
        PaperFormat::A4 => genpdf::PaperSize::A4.into(),
        PaperFormat::Letter => genpdf::PaperSize::Letter.into(),
        PaperFormat::Legal => genpdf::PaperSize::Legal.into(),
    };
    let paper = match setup.orientation {
        Orientation::Portrait => paper,
        Orientation::Landscape => Size::new(paper.height, paper.width),
    };
    doc.set_paper_size(paper);

    let scale = f64::from(setup.font_scale) / 100.0;
    let labels = setup.locale.labels();
    doc.set_page_decorator(SetlystPageDecorator {
        page: 0,
        margins: setup.margins.mm(),
        watermark: setup.watermark,
        page_numbers: setup.page_numbers,
        page_label: labels.page,
        credit: labels.generated_with,
        font_scale: scale,
    });

    Ok((doc, Styles { scale }))
}

pub fn generate_setlist_pdf(
    data: &SetlistPdfData<'_>,
    options: &PdfExportOptions,
) -> Result<Vec<u8>, PdfError> {
    let (mut doc, styles) = new_document(
        format!("Setlist - {}", data.title),
        &PageSetup {
            paper: options.paper,
            orientation: options.orientation,
            margins: options.margins,
            watermark: options.watermark,
            page_numbers: options.page_numbers,
            font_scale: options.font_scale,
            locale: options.locale,
            compact: options.compact,
        },
    )?;

    push_header(&mut doc, data, options, &styles);
    push_running_order(&mut doc, data, options, &styles)?;
    if options.include_lyrics {
        push_songbook(&mut doc, data, options, &styles);
    }

    let mut buffer = Cursor::new(Vec::new());
    doc.render(&mut buffer)?;

    Ok(buffer.into_inner())
}

// ---------------------------------------------------------------------
// Single-song sheet
// ---------------------------------------------------------------------

/// Query parameters of `GET /songs/{id}/export/pdf`. Every field is
/// optional; the defaults give a one-column sheet with chords above the
/// lyrics and every piece of metadata shown.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SongExportQuery {
    #[serde(default = "default_true")]
    pub show_artist: bool,
    #[serde(default = "default_true")]
    pub show_key: bool,
    #[serde(default = "default_true")]
    pub show_capo: bool,
    #[serde(default = "default_true")]
    pub show_bpm: bool,
    #[serde(default = "default_true")]
    pub show_time_signature: bool,
    #[serde(default = "default_true")]
    pub show_tuning: bool,
    /// Print the performance notes under the header.
    #[serde(default)]
    pub show_notes: bool,
    /// How chords are printed (`chords` is accepted as an alias).
    #[serde(default, alias = "chords")]
    pub chord_mode: ChordMode,
    /// 1 or 2 columns for the lyrics.
    #[serde(default = "default_columns")]
    pub columns: u8,
    /// Text size, in percent of the default (60–200).
    #[serde(default = "default_font_scale")]
    pub font_scale: u16,
    #[serde(default)]
    pub uppercase_titles: bool,
    /// Faint Setlyst watermark and footer credit.
    #[serde(default = "default_true")]
    pub watermark: bool,
    #[serde(default = "default_true")]
    pub page_numbers: bool,
    #[serde(default)]
    pub paper: PaperFormat,
    #[serde(default)]
    pub orientation: Orientation,
    #[serde(default)]
    pub margins: MarginSize,
    #[serde(default)]
    pub lang: PdfLocale,
}

/// Normalized, clamped song-sheet options.
#[derive(Debug, Clone)]
pub struct SongPdfOptions {
    pub show_artist: bool,
    pub show_key: bool,
    pub show_capo: bool,
    pub show_bpm: bool,
    pub show_time_signature: bool,
    pub show_tuning: bool,
    pub show_notes: bool,
    pub chords: ChordMode,
    pub columns: u8,
    pub font_scale: u16,
    pub uppercase_titles: bool,
    pub watermark: bool,
    pub page_numbers: bool,
    pub paper: PaperFormat,
    pub orientation: Orientation,
    pub margins: MarginSize,
    pub locale: PdfLocale,
}

impl From<SongExportQuery> for SongPdfOptions {
    fn from(query: SongExportQuery) -> Self {
        Self {
            show_artist: query.show_artist,
            show_key: query.show_key,
            show_capo: query.show_capo,
            show_bpm: query.show_bpm,
            show_time_signature: query.show_time_signature,
            show_tuning: query.show_tuning,
            show_notes: query.show_notes,
            chords: query.chord_mode,
            columns: query.columns.clamp(1, 2),
            font_scale: query.font_scale.clamp(60, 200),
            uppercase_titles: query.uppercase_titles,
            watermark: query.watermark,
            page_numbers: query.page_numbers,
            paper: query.paper,
            orientation: query.orientation,
            margins: query.margins,
            locale: query.lang,
        }
    }
}

impl Default for SongPdfOptions {
    fn default() -> Self {
        serde_urlencoded::from_str::<SongExportQuery>("")
            .map(Self::from)
            .expect("an empty query always deserializes")
    }
}

impl SongPdfOptions {
    /// Options that need the `advanced_pdf` plan feature: two columns, no
    /// watermark, or non-default margins (see
    /// [`PdfExportOptions::is_advanced`]).
    pub fn is_advanced(&self) -> bool {
        self.columns == 2 || !self.watermark || self.margins != MarginSize::Normal
    }
}

/// Stanzas longer than this are split when laid out in two columns, so a
/// table row always fits on a page.
const MAX_STANZA_LINES: usize = 24;

/// Splits lyric lines into stanzas (separated by blank lines).
fn stanzas(lines: Vec<LyricLine>) -> Vec<Vec<LyricLine>> {
    let mut out: Vec<Vec<LyricLine>> = vec![Vec::new()];
    for line in lines {
        let current = out.last_mut().expect("never empty");
        if line == LyricLine::Blank {
            if !current.is_empty() {
                out.push(Vec::new());
            }
            continue;
        }
        if current.len() >= MAX_STANZA_LINES {
            out.push(Vec::new());
        }
        out.last_mut().expect("never empty").push(line);
    }
    out.retain(|stanza| !stanza.is_empty());
    out
}

/// A single song with its metadata and lyrics.
pub fn generate_song_pdf(
    song: &SongWithArtist,
    options: &SongPdfOptions,
) -> Result<Vec<u8>, PdfError> {
    let labels = options.locale.labels();
    let (mut doc, styles) = new_document(
        format!("{} - {}", song.title, song.artist_name),
        &PageSetup {
            paper: options.paper,
            orientation: options.orientation,
            margins: options.margins,
            watermark: options.watermark,
            page_numbers: options.page_numbers,
            font_scale: options.font_scale,
            locale: options.locale,
            compact: false,
        },
    )?;

    let title = if options.uppercase_titles {
        song.title.to_uppercase()
    } else {
        song.title.clone()
    };
    doc.push(Paragraph::new(title).styled(styles.text(22.0).bold()));
    if options.show_artist {
        doc.push(Paragraph::new(song.artist_name.clone()).styled(styles.muted(13.0)));
    }

    let mut meta: Vec<String> = Vec::new();
    if options.show_key
        && let Some(key) = &song.tonality
    {
        meta.push(format!("{}: {}", labels.key, format_tonality(key)));
    }
    if options.show_capo
        && let Some(capo) = song.capo.filter(|c| *c > 0)
    {
        meta.push(format!("{}: {capo}", labels.capo));
    }
    if options.show_bpm
        && let Some(bpm) = song.tempo
    {
        meta.push(format!("{bpm} BPM"));
    }
    if options.show_time_signature
        && let Some(time) = &song.time_signature
    {
        meta.push(format!("{}: {time}", labels.time_signature));
    }
    if options.show_tuning
        && let Some(tuning) = &song.tuning
    {
        meta.push(format!("{}: {tuning}", labels.tuning));
    }
    if !meta.is_empty() {
        doc.push(Paragraph::new(meta.join("  ·  ")).styled(styles.muted(11.0)));
    }
    if options.show_notes
        && let Some(notes) = song
            .performance_notes
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
    {
        doc.push(elements::Break::new(0.4));
        doc.push(Paragraph::new(format!("{}:", labels.notes)).styled(styles.text(10.5).bold()));
        for line in notes.lines() {
            doc.push(Paragraph::new(line.to_string()).styled(styles.muted(10.5).italic()));
        }
    }

    doc.push(elements::Break::new(0.5));
    doc.push(HorizontalRule {
        color: Color::Rgb(210, 210, 210),
    });
    doc.push(elements::Break::new(0.6));

    let lines = song
        .lyrics
        .as_deref()
        .map(|lyrics| parse_lyrics(lyrics, options.chords, options.locale))
        .unwrap_or_default();

    let lyric_style = styles.text(12.0);
    let chord_style = styles.text(11.0).bold().with_color(Color::Rgb(30, 90, 170));

    if lines.is_empty() {
        doc.push(Paragraph::new(labels.no_lyrics).styled(styles.muted(11.0).italic()));
    } else if options.columns == 2 {
        // Stanzas side by side, two per row, read left to right.
        let mut blocks = stanzas(lines).into_iter();
        let mut table = TableLayout::new(vec![1, 1]);
        while let Some(left) = blocks.next() {
            let mut left_layout = LinearLayout::vertical();
            push_lyric_lines(&mut left_layout, left, &styles, lyric_style, chord_style);
            left_layout.push(elements::Break::new(0.6));
            let mut row = table.row();
            row.push_element(elements::PaddedElement::new(
                left_layout,
                Margins::trbl(0, 4, 0, 0),
            ));
            match blocks.next() {
                Some(right) => {
                    let mut right_layout = LinearLayout::vertical();
                    push_lyric_lines(&mut right_layout, right, &styles, lyric_style, chord_style);
                    right_layout.push(elements::Break::new(0.6));
                    row.push_element(elements::PaddedElement::new(
                        right_layout,
                        Margins::trbl(0, 0, 0, 4),
                    ));
                }
                None => row.push_element(elements::Break::new(0)),
            }
            row.push()?;
        }
        doc.push(table);
    } else {
        let mut layout = LinearLayout::vertical();
        push_lyric_lines(&mut layout, lines, &styles, lyric_style, chord_style);
        doc.push(layout);
    }

    let mut buffer = Cursor::new(Vec::new());
    doc.render(&mut buffer)?;
    Ok(buffer.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;

    fn song(title: &str, lyrics: Option<&str>) -> SongWithArtist {
        let now = Utc::now().naive_utc();
        SongWithArtist {
            id: Uuid::new_v4(),
            title: title.to_string(),
            artist_id: Uuid::new_v4(),
            artist_name: "Artista Ção".to_string(),
            user_id: Uuid::new_v4(),
            band_id: None,
            forked_from: None,
            tempo: Some(120),
            lyrics: lyrics.map(str::to_string),
            tonality: Some(Tonality::FSharpM),
            genre: None,
            duration: Some(215),
            energy: Some(4),
            time_signature: Some("6/8".to_string()),
            capo: Some(2),
            tuning: Some("Drop D".to_string()),
            performance_notes: Some("Entrada suave\nSolo no final".to_string()),
            links: Default::default(),
            tags: vec!["balada".to_string()],
            updated_by: None,
            updated_by_username: None,
            source_synced_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn items() -> Vec<SetlistItem> {
        let mut items = vec![SetlistItem::Block {
            position: 1,
            id: Uuid::new_v4(),
            name: "Abertura".to_string(),
        }];
        for i in 0..25 {
            items.push(SetlistItem::Song {
                position: i + 2,
                song: Box::new(song(
                    &format!("Canção número {i}"),
                    Some("{title: X}\n[Intro]\nG  D  Em  C\n\n[G]Olá, [D/F#]mundo\n    Am       C\nLetra embaixo do acorde\n{c: x2}"),
                )),
            });
        }
        items.push(SetlistItem::Break {
            position: 99,
            id: Uuid::new_v4(),
            label: None,
            duration_minutes: Some(15),
        });
        items
    }

    fn render(query: &str) -> Vec<u8> {
        let options =
            PdfExportOptions::from(serde_urlencoded::from_str::<ExportQuery>(query).unwrap());
        let items = items();
        let data = SetlistPdfData {
            title: "Show de Sábado",
            description: Some("Casa cheia\nSegundo set"),
            band_name: Some("Os Testes"),
            total_duration_secs: 5_400,
            items: &items,
        };
        let pdf = generate_setlist_pdf(&data, &options).expect("pdf renders");

        // `PDF_DUMP_DIR=/tmp/pdfs cargo test pdf` writes every variant to
        // disk for a visual check.
        if let Ok(dir) = std::env::var("PDF_DUMP_DIR") {
            let name: String = query
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                .take(60)
                .collect();
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(format!("{dir}/setlist_{name}.pdf"), &pdf);
        }
        pdf
    }

    #[test]
    fn renders_with_defaults() {
        let pdf = render("");
        assert!(pdf.starts_with(b"%PDF"));
    }

    #[test]
    fn renders_every_option_combination_that_matters() {
        for query in [
            "compact=true&columns=2&font_scale=60&watermark=false&page_numbers=false",
            "include_lyrics=true&chords=above&page_break_per_song=true&paper=letter&orientation=landscape",
            "include_lyrics=true&chords=inline&font_scale=200&margins=narrow&show_artist=true&show_key=true&show_bpm=true&show_song_duration=true&show_tags=true",
            "include_lyrics=true&chords=hide&uppercase_titles=true&show_date=true&show_band_name=true&show_description=true&subtitle=Bar%20do%20Z%C3%A9&lang=pt-BR",
            "columns=9&font_scale=5000&lang=es",
        ] {
            assert!(render(query).starts_with(b"%PDF"), "{query}");
        }
    }

    #[test]
    fn options_are_clamped() {
        let options = PdfExportOptions::from(
            serde_urlencoded::from_str::<ExportQuery>("columns=7&font_scale=10").unwrap(),
        );
        assert_eq!(options.columns, 2);
        assert_eq!(options.font_scale, 60);
        let defaults = PdfExportOptions::default();
        assert!(defaults.show_title && defaults.show_blocks && defaults.watermark);
        assert!(!defaults.include_lyrics);
    }

    #[test]
    fn chord_detection() {
        for chord in [
            "G", "F#m7", "Bbmaj7", "Asus4", "D/F#", "C7(9)", "Em", "N.C.", "Cadd9", "G7/B", "A°",
        ] {
            assert!(is_chord(chord), "{chord} should be a chord");
        }
        for word in ["And", "Come", "Hello", "Amor", "Da", "Be", "Go", "H7", ""] {
            assert!(!is_chord(word), "{word} should not be a chord");
        }
    }

    #[test]
    fn inline_chords_are_split_into_segments() {
        assert_eq!(
            parse_inline_chords("[G]Olá [C]mundo"),
            vec![
                (Some("G".to_string()), "Olá ".to_string()),
                (Some("C".to_string()), "mundo".to_string())
            ]
        );
        assert_eq!(
            parse_inline_chords("Sem acordes [x2]"),
            vec![(None, "Sem acordes [x2]".to_string())]
        );
    }

    #[test]
    fn chords_above_lyrics_are_merged_by_column() {
        let merged = merge_chord_line("G       D", "Hello my friend");
        assert_eq!(
            merged,
            vec![
                (Some("G".to_string()), "Hello my".to_string()),
                (Some("D".to_string()), " friend".to_string())
            ]
        );
    }

    #[test]
    fn lyrics_modes() {
        let raw = "{soc}\n[Refrão]\nG     C\nLá lá lá\n{eoc}";
        let hidden = parse_lyrics(raw, ChordMode::Hide, PdfLocale::PtBr);
        assert_eq!(
            hidden,
            vec![
                LyricLine::Heading("Refrão".to_string()),
                LyricLine::Heading("Refrão".to_string()),
                LyricLine::Segments(vec![(None, "Lá lá lá".to_string())]),
            ]
        );
        let inline = parse_lyrics("[G]Lá", ChordMode::Inline, PdfLocale::En);
        assert_eq!(
            inline,
            vec![LyricLine::Segments(vec![(None, "[G]Lá".to_string())])]
        );
    }

    #[test]
    fn filenames_are_safe_and_utf8_aware() {
        assert_eq!(
            pdf_filename("Show de Sábado!"),
            "setlist-show-de-sábado.pdf"
        );
        assert_eq!(pdf_filename("///"), "setlist.pdf");
        assert_eq!(slug_filename("song", "Ação!", "cho"), "song-ação.cho");
        let header = content_disposition("setlist-canções.pdf");
        assert!(header.contains("filename=\"setlist-can__es.pdf\""));
        assert!(header.contains("filename*=UTF-8''setlist-can%C3%A7%C3%B5es.pdf"));
        assert!(header.is_ascii());
    }

    #[test]
    fn renders_single_song_sheets() {
        let long_lyrics = (0..120)
            .map(|i| {
                if i % 6 == 5 {
                    String::new()
                } else {
                    format!("[G]Linha [D/F#]número {i} da [Em]canção")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let chart = song("Canção longa", Some(&long_lyrics));
        for query in [
            "",
            "columns=2&chord_mode=inline&show_notes=true&lang=pt-BR",
            "chords=hide&watermark=false&margins=wide&paper=letter&orientation=landscape",
            "columns=2&font_scale=200&uppercase_titles=true&show_key=false&show_capo=false",
        ] {
            let options =
                SongPdfOptions::from(serde_urlencoded::from_str::<SongExportQuery>(query).unwrap());
            let pdf = generate_song_pdf(&chart, &options).expect("song pdf renders");
            assert!(pdf.starts_with(b"%PDF"), "{query}");
        }
        let empty = song("Sem letra", None);
        assert!(
            generate_song_pdf(&empty, &SongPdfOptions::default())
                .unwrap()
                .starts_with(b"%PDF")
        );
    }

    #[test]
    fn advanced_options_are_classified() {
        let basic = PdfExportOptions::from(
            serde_urlencoded::from_str::<ExportQuery>(
                "compact=true&font_scale=150&paper=letter&orientation=landscape&show_artist=true&chords=inline",
            )
            .unwrap(),
        );
        assert!(!basic.is_advanced());
        for query in [
            "columns=2",
            "include_lyrics=true",
            "watermark=false",
            "margins=narrow",
        ] {
            let options =
                PdfExportOptions::from(serde_urlencoded::from_str::<ExportQuery>(query).unwrap());
            assert!(options.is_advanced(), "{query}");
            assert!(!options.to_basic().is_advanced(), "{query}");
        }
        assert!(!SongPdfOptions::default().is_advanced());
        let song_advanced = SongPdfOptions::from(
            serde_urlencoded::from_str::<SongExportQuery>("columns=2").unwrap(),
        );
        assert!(song_advanced.is_advanced());
    }

    #[test]
    fn durations() {
        assert_eq!(format_duration(215), "3:35");
        assert_eq!(format_duration(5_400), "1h 30m");
        assert_eq!(format_duration(-5), "0:00");
    }
}
