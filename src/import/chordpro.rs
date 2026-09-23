//! ChordPro import: a strict, pure parser.
//!
//! The file comes from anywhere (another app, a website, a hand edit), so
//! it is treated as hostile input:
//!
//! - at most [`MAX_CONTENT_BYTES`] bytes (`CHORDPRO_TOO_LARGE`), at most
//!   [`MAX_LINES`] lines and [`MAX_LINE_CHARS`] characters per line
//!   (`CHORDPRO_INVALID` with `reason` `too_many_lines` / `line_too_long`);
//! - NUL bytes are refused (`nul_character`); CRLF/CR become LF, the text
//!   is NFC-normalized, the BOM, zero-width and bidirectional override
//!   characters are removed, tabs become spaces and every other control
//!   character is dropped;
//! - only a whitelist of directives survives: metadata (`title`, `artist`,
//!   `key`, `tempo`, `time`, `capo`, `duration`, `subtitle`) is extracted
//!   and validated, section and comment directives are kept verbatim in
//!   the lyrics, anything else (`image`, `define`, `x_*`...) is dropped
//!   with a warning;
//! - bracketed text must be a chord (see [`crate::export::pdf::is_chord`]);
//!   anything else in brackets is removed with a warning;
//! - one song per file (`multiple_songs`), and a title is required
//!   (`missing_title`) unless the caller supplies one.
//!
//! Nothing is ever interpreted as markup: HTML or script in a lyric stays
//! inert text.

use crate::{export::pdf::is_chord, models::song::Tonality};
use serde::Serialize;
use unicode_normalization::UnicodeNormalization;
use utoipa::ToSchema;

pub const MAX_CONTENT_BYTES: usize = 64 * 1024;
pub const MAX_LINES: usize = 3_000;
pub const MAX_LINE_CHARS: usize = 500;
const MAX_WARNINGS: usize = 100;
const MAX_TITLE_CHARS: usize = 255;
const MAX_ARTIST_CHARS: usize = 255;
const TAB_WIDTH: usize = 4;

/// Why a file was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChordProError {
    /// Over [`MAX_CONTENT_BYTES`].
    TooLarge,
    /// `reason` is a stable code (`nul_character`, `too_many_lines`,
    /// `line_too_long`, `multiple_songs`, `missing_title`); `line` is
    /// 1-based.
    Invalid {
        reason: &'static str,
        line: Option<usize>,
    },
}

/// Something that was changed or dropped while importing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ImportWarning {
    /// `directive_dropped`, `album_dropped`, `invalid_chord`,
    /// `unbalanced_bracket`, `braces_removed`, `invalid_key`,
    /// `invalid_tempo`, `invalid_time`, `invalid_capo`,
    /// `invalid_duration`, `title_truncated`, `artist_truncated`,
    /// `duplicate_directive`, `too_many_warnings`.
    pub code: &'static str,
    /// 1-based line in the (normalized) file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// The parsed song.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ParsedChordPro {
    pub title: String,
    pub artist_name: Option<String>,
    pub tonality: Option<Tonality>,
    pub tempo: Option<i32>,
    pub time_signature: Option<String>,
    pub capo: Option<i16>,
    /// Seconds.
    pub duration: Option<i32>,
    /// The sanitized body (lyrics, chords, kept directives).
    pub lyrics: Option<String>,
    pub warnings: Vec<ImportWarning>,
}

struct Warnings(Vec<ImportWarning>);

impl Warnings {
    fn push(&mut self, code: &'static str, line: Option<usize>, detail: Option<String>) {
        if self.0.len() < MAX_WARNINGS {
            self.0.push(ImportWarning {
                code,
                line,
                detail: detail.map(|d| d.chars().take(80).collect()),
            });
        } else if self.0.len() == MAX_WARNINGS {
            self.0.push(ImportWarning {
                code: "too_many_warnings",
                line: None,
                detail: None,
            });
        }
    }
}

/// Zero-width and bidirectional formatting characters (and the BOM).
fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}'
    )
}

/// Normalizes line endings, Unicode form and invisible/control characters.
fn sanitize(content: &str) -> Result<String, ChordProError> {
    if content.contains('\0') {
        let line = content[..content.find('\0').unwrap_or(0)]
            .matches('\n')
            .count()
            + 1;
        return Err(ChordProError::Invalid {
            reason: "nul_character",
            line: Some(line),
        });
    }
    let unified = content.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = String::with_capacity(unified.len());
    for c in unified.nfc() {
        match c {
            '\n' => out.push('\n'),
            '\t' => out.push_str(&" ".repeat(TAB_WIDTH)),
            c if c.is_control() || is_invisible(c) => {}
            c => out.push(c),
        }
    }
    Ok(out)
}

/// Removes braces and collapses whitespace in a directive value.
fn clean_value(value: &str) -> String {
    value
        .chars()
        .map(|c| if c == '{' || c == '}' { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn truncate(value: &str, max: usize) -> (String, bool) {
    if value.chars().count() > max {
        (value.chars().take(max).collect(), true)
    } else {
        (value.to_string(), false)
    }
}

/// `{name}` or `{name: value}` (also `{name:value}`). `None` for lines
/// that aren't a whole directive.
fn parse_directive(line: &str) -> Option<(String, String)> {
    let inner = line.strip_prefix('{')?.strip_suffix('}')?;
    let (name, value) = match inner.split_once(':') {
        Some((name, value)) => (name, value),
        None => (inner, ""),
    };
    Some((name.trim().to_ascii_lowercase(), clean_value(value)))
}

const SECTION_DIRECTIVES: [&str; 18] = [
    "start_of_chorus",
    "soc",
    "end_of_chorus",
    "eoc",
    "start_of_verse",
    "sov",
    "end_of_verse",
    "eov",
    "start_of_bridge",
    "sob",
    "end_of_bridge",
    "eob",
    "start_of_tab",
    "sot",
    "end_of_tab",
    "eot",
    "chorus",
    "comment",
];

const COMMENT_DIRECTIVES: [&str; 3] = ["c", "comment_italic", "ci"];

/// Maps a free-form key ("A minor", "F♯m", "bb") to a [`Tonality`].
pub fn parse_key(raw: &str) -> Option<Tonality> {
    let value = raw.trim().replace('♯', "#").replace('♭', "b");
    let mut chars = value.chars();
    let root = chars.next()?.to_ascii_uppercase();
    if !('A'..='G').contains(&root) {
        return None;
    }
    let rest: String = chars.collect();
    let (accidental, quality) = match rest.chars().next() {
        Some(c @ ('#' | 'b')) => (c.to_string(), rest[1..].trim().to_string()),
        _ => (String::new(), rest.trim().to_string()),
    };
    let minor = match quality.to_ascii_lowercase().as_str() {
        "" | "maj" | "major" | "dur" => false,
        "m" | "min" | "minor" | "-" | "moll" => true,
        _ => return None,
    };
    let name = format!("{root}{accidental}{}", if minor { "m" } else { "" });
    serde_json::from_value(serde_json::Value::String(name)).ok()
}

fn parse_tempo(raw: &str) -> Option<i32> {
    let lower = raw.trim().to_ascii_lowercase();
    let number = lower.strip_suffix("bpm").unwrap_or(&lower).trim();
    if number.is_empty() || !number.chars().all(|c| c.is_ascii_digit()) || number.len() > 3 {
        return None;
    }
    number.parse().ok().filter(|t| (1..=500).contains(t))
}

fn parse_capo(raw: &str) -> Option<i16> {
    let number = raw.trim();
    if number.is_empty() || !number.chars().all(|c| c.is_ascii_digit()) || number.len() > 2 {
        return None;
    }
    number.parse().ok().filter(|c| (0..=11).contains(c))
}

/// `m:ss` or plain seconds, 1 second to 2 hours.
fn parse_duration(raw: &str) -> Option<i32> {
    let value = raw.trim();
    let digits = |s: &str| !s.is_empty() && s.len() <= 4 && s.chars().all(|c| c.is_ascii_digit());
    let seconds = match value.split_once(':') {
        Some((minutes, seconds)) => {
            if !digits(minutes) || seconds.len() != 2 || !digits(seconds) {
                return None;
            }
            let minutes: i32 = minutes.parse().ok()?;
            let seconds: i32 = seconds.parse().ok()?;
            if seconds >= 60 {
                return None;
            }
            minutes * 60 + seconds
        }
        None => {
            if !digits(value) {
                return None;
            }
            value.parse().ok()?
        }
    };
    Some(seconds).filter(|s| (1..=7_200).contains(s))
}

/// Keeps valid `[chord]` brackets, removes everything else in brackets,
/// stray brackets and braces.
fn clean_lyric_line(line: &str, number: usize, warnings: &mut Warnings) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    let mut removed_braces = false;
    while i < chars.len() {
        match chars[i] {
            '[' => {
                // The closing bracket, unless another one opens first.
                let close = chars[i + 1..]
                    .iter()
                    .position(|c| *c == ']' || *c == '[')
                    .map(|p| p + i + 1)
                    .filter(|p| chars[*p] == ']');
                match close {
                    Some(close) => {
                        let inner: String = chars[i + 1..close].iter().collect();
                        let inner = inner.trim();
                        if !inner.is_empty() && is_chord(inner) {
                            out.push('[');
                            out.push_str(inner);
                            out.push(']');
                        } else {
                            warnings.push("invalid_chord", Some(number), Some(inner.to_string()));
                        }
                        i = close + 1;
                    }
                    None => {
                        warnings.push("unbalanced_bracket", Some(number), None);
                        i += 1;
                    }
                }
            }
            ']' => {
                warnings.push("unbalanced_bracket", Some(number), None);
                i += 1;
            }
            '{' | '}' => {
                removed_braces = true;
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    if removed_braces {
        warnings.push("braces_removed", Some(number), None);
    }
    out.trim_end().to_string()
}

/// Parses one ChordPro song. `title_override` (trimmed, non-empty) wins
/// over the file's `{title}`.
pub fn parse(content: &str, title_override: Option<&str>) -> Result<ParsedChordPro, ChordProError> {
    if content.len() > MAX_CONTENT_BYTES {
        return Err(ChordProError::TooLarge);
    }
    let text = sanitize(content)?;
    let lines: Vec<&str> = text.split('\n').collect();
    if lines.len() > MAX_LINES {
        return Err(ChordProError::Invalid {
            reason: "too_many_lines",
            line: Some(MAX_LINES + 1),
        });
    }
    if let Some(index) = lines
        .iter()
        .position(|l| l.chars().count() > MAX_LINE_CHARS)
    {
        return Err(ChordProError::Invalid {
            reason: "line_too_long",
            line: Some(index + 1),
        });
    }

    let mut warnings = Warnings(Vec::new());
    let mut parsed = ParsedChordPro::default();
    let mut file_title: Option<String> = None;
    let mut artist: Option<String> = None;
    let mut subtitle: Option<String> = None;
    let mut body: Vec<String> = Vec::with_capacity(lines.len());

    for (index, raw) in lines.iter().enumerate() {
        let number = index + 1;
        let line = raw.trim();

        if let Some((name, value)) = parse_directive(line) {
            let known_name = !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
            match name.as_str() {
                _ if !known_name => {
                    warnings.push("directive_dropped", Some(number), Some(name.clone()));
                }
                "new_song" | "ns" => {
                    return Err(ChordProError::Invalid {
                        reason: "multiple_songs",
                        line: Some(number),
                    });
                }
                "title" | "t" => {
                    if file_title.is_some() {
                        return Err(ChordProError::Invalid {
                            reason: "multiple_songs",
                            line: Some(number),
                        });
                    }
                    file_title = Some(value).filter(|v| !v.is_empty());
                }
                "subtitle" | "st" => {
                    if subtitle.is_none() {
                        subtitle = Some(value).filter(|v| !v.is_empty());
                    }
                }
                "artist" => {
                    if artist.is_some() {
                        warnings.push("duplicate_directive", Some(number), Some(name.clone()));
                    } else {
                        artist = Some(value).filter(|v| !v.is_empty());
                    }
                }
                "key" => match parse_key(&value) {
                    Some(key) if parsed.tonality.is_none() => parsed.tonality = Some(key),
                    Some(_) => {
                        warnings.push("duplicate_directive", Some(number), Some(name.clone()))
                    }
                    None => warnings.push("invalid_key", Some(number), Some(value)),
                },
                "tempo" => match parse_tempo(&value) {
                    Some(tempo) if parsed.tempo.is_none() => parsed.tempo = Some(tempo),
                    Some(_) => {
                        warnings.push("duplicate_directive", Some(number), Some(name.clone()))
                    }
                    None => warnings.push("invalid_tempo", Some(number), Some(value)),
                },
                "time" => {
                    let value = value.replace(' ', "");
                    if crate::models::song::TIME_SIGNATURES.contains(&value.as_str()) {
                        if parsed.time_signature.is_none() {
                            parsed.time_signature = Some(value);
                        } else {
                            warnings.push("duplicate_directive", Some(number), Some(name.clone()));
                        }
                    } else {
                        warnings.push("invalid_time", Some(number), Some(value));
                    }
                }
                "capo" => match parse_capo(&value) {
                    Some(capo) if parsed.capo.is_none() => parsed.capo = Some(capo),
                    Some(_) => {
                        warnings.push("duplicate_directive", Some(number), Some(name.clone()))
                    }
                    None => warnings.push("invalid_capo", Some(number), Some(value)),
                },
                "duration" => match parse_duration(&value) {
                    Some(duration) if parsed.duration.is_none() => parsed.duration = Some(duration),
                    Some(_) => {
                        warnings.push("duplicate_directive", Some(number), Some(name.clone()))
                    }
                    None => warnings.push("invalid_duration", Some(number), Some(value)),
                },
                "album" => warnings.push("album_dropped", Some(number), Some(value)),
                n if SECTION_DIRECTIVES.contains(&n) || COMMENT_DIRECTIVES.contains(&n) => {
                    if value.is_empty() {
                        body.push(format!("{{{n}}}"));
                    } else {
                        body.push(format!("{{{n}: {value}}}"));
                    }
                }
                _ => warnings.push("directive_dropped", Some(number), Some(name.clone())),
            }
            continue;
        }

        body.push(clean_lyric_line(raw, number, &mut warnings));
    }

    let title = title_override
        .map(clean_value)
        .filter(|t| !t.is_empty())
        .or(file_title)
        .ok_or(ChordProError::Invalid {
            reason: "missing_title",
            line: None,
        })?;
    let (title, cut) = truncate(&title, MAX_TITLE_CHARS);
    if cut {
        warnings.push("title_truncated", None, None);
    }
    parsed.title = title;

    parsed.artist_name = artist.or(subtitle).map(|a| {
        let (artist, cut) = truncate(&a, MAX_ARTIST_CHARS);
        if cut {
            warnings.push("artist_truncated", None, None);
        }
        artist
    });

    // Trim blank lines at both ends and collapse long runs of them.
    let mut lyrics: Vec<String> = Vec::with_capacity(body.len());
    let mut blank_run = 0;
    for line in body {
        if line.trim().is_empty() {
            blank_run += 1;
            if blank_run > 2 || lyrics.is_empty() {
                continue;
            }
            lyrics.push(String::new());
        } else {
            blank_run = 0;
            lyrics.push(line);
        }
    }
    while lyrics.last().is_some_and(|l| l.is_empty()) {
        lyrics.pop();
    }
    parsed.lyrics = if lyrics.is_empty() {
        None
    } else {
        Some(lyrics.join("\n"))
    };
    parsed.warnings = warnings.0;
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(parsed: &ParsedChordPro) -> Vec<&'static str> {
        parsed.warnings.iter().map(|w| w.code).collect()
    }

    #[test]
    fn parses_a_complete_song() {
        let parsed = parse(
            "{title: Garota de Ipanema}\n{artist: Tom Jobim}\n{key: F}\n{tempo: 128}\n{time: 4/4}\n{capo: 2}\n{duration: 3:05}\n\n{start_of_verse: Verso 1}\n[F]Olha que coisa mais [G7]linda\n{end_of_verse}\n{c: x2}\n",
            None,
        )
        .unwrap();
        assert_eq!(parsed.title, "Garota de Ipanema");
        assert_eq!(parsed.artist_name.as_deref(), Some("Tom Jobim"));
        assert_eq!(parsed.tonality, Some(Tonality::F));
        assert_eq!(parsed.tempo, Some(128));
        assert_eq!(parsed.time_signature.as_deref(), Some("4/4"));
        assert_eq!(parsed.capo, Some(2));
        assert_eq!(parsed.duration, Some(185));
        assert_eq!(
            parsed.lyrics.as_deref(),
            Some(
                "{start_of_verse: Verso 1}\n[F]Olha que coisa mais [G7]linda\n{end_of_verse}\n{c: x2}"
            )
        );
        assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);
    }

    #[test]
    fn short_directive_names_and_overrides() {
        let parsed = parse(
            "{t:Old}\n{st: Artist From Subtitle}\nLa la",
            Some("  New  "),
        )
        .unwrap();
        assert_eq!(parsed.title, "New");
        assert_eq!(parsed.artist_name.as_deref(), Some("Artist From Subtitle"));
        let parsed = parse("{t:Old}\nLa", Some("   ")).unwrap();
        assert_eq!(parsed.title, "Old");
    }

    #[test]
    fn refuses_what_it_cannot_import() {
        assert_eq!(
            parse(&"a".repeat(MAX_CONTENT_BYTES + 1), None),
            Err(ChordProError::TooLarge)
        );
        assert_eq!(
            parse("{title: X}\nfine\n\0evil", None),
            Err(ChordProError::Invalid {
                reason: "nul_character",
                line: Some(3)
            })
        );
        let long = format!("{{title: X}}\n{}", "a".repeat(MAX_LINE_CHARS + 1));
        assert_eq!(
            parse(&long, None),
            Err(ChordProError::Invalid {
                reason: "line_too_long",
                line: Some(2)
            })
        );
        let many = format!("{{title: X}}\n{}", "a\n".repeat(MAX_LINES));
        assert!(matches!(
            parse(&many, None),
            Err(ChordProError::Invalid {
                reason: "too_many_lines",
                ..
            })
        ));
        assert_eq!(
            parse("{title: A}\nx\n{new_song}\n{title: B}", None),
            Err(ChordProError::Invalid {
                reason: "multiple_songs",
                line: Some(3)
            })
        );
        assert_eq!(
            parse("{title: A}\nx\n{title: B}", None),
            Err(ChordProError::Invalid {
                reason: "multiple_songs",
                line: Some(3)
            })
        );
        assert_eq!(
            parse("[G]no title here", None),
            Err(ChordProError::Invalid {
                reason: "missing_title",
                line: None
            })
        );
        assert!(parse("{title:   }", None).is_err());
    }

    #[test]
    fn drops_unknown_directives_and_bad_chords() {
        let parsed = parse(
            "{title: X}\n{image: src=http://evil/x.png}\n{define: G base-fret 1 frets 3 2 0 0 0 3}\n{x_custom: 1}\n{meta: foo}\n{album: Y}\n{title-guitar: Z}\n[G]ok [Hello]world [C\n] stray",
            None,
        )
        .unwrap();
        let codes = codes(&parsed);
        assert_eq!(
            codes.iter().filter(|c| **c == "directive_dropped").count(),
            5
        );
        assert!(codes.contains(&"album_dropped"));
        assert!(codes.contains(&"invalid_chord"));
        assert!(codes.contains(&"unbalanced_bracket"));
        let lyrics = parsed.lyrics.unwrap();
        assert!(lyrics.starts_with("[G]ok world C"), "{lyrics}");
        assert!(!lyrics.contains("image"));
        assert!(!lyrics.contains("Hello"));
    }

    #[test]
    fn invalid_metadata_becomes_warnings() {
        let parsed = parse(
            "{title: X}\n{key: H}\n{tempo: fast}\n{tempo: 999}\n{time: 11/8}\n{capo: 12}\n{duration: 9:99}\n{duration: 99999}",
            None,
        )
        .unwrap();
        assert_eq!(parsed.tonality, None);
        assert_eq!(parsed.tempo, None);
        assert_eq!(parsed.time_signature, None);
        assert_eq!(parsed.capo, None);
        assert_eq!(parsed.duration, None);
        let codes = codes(&parsed);
        for code in [
            "invalid_key",
            "invalid_tempo",
            "invalid_time",
            "invalid_capo",
            "invalid_duration",
        ] {
            assert!(codes.contains(&code), "{code}");
        }
    }

    #[test]
    fn keys_are_normalized() {
        for (raw, key) in [
            ("Am", Tonality::Am),
            ("A minor", Tonality::Am),
            ("A min", Tonality::Am),
            ("a-", Tonality::Am),
            ("F♯m", Tonality::FSharpM),
            ("B♭", Tonality::Bb),
            ("bb", Tonality::Bb),
            ("C major", Tonality::C),
            ("Ebm", Tonality::Ebm),
        ] {
            assert_eq!(parse_key(raw), Some(key), "{raw}");
        }
        for raw in ["H", "", "Amaj7", "X#", "C##"] {
            assert_eq!(parse_key(raw), None, "{raw}");
        }
        assert_eq!(parse_tempo("120 BPM"), Some(120));
        assert_eq!(parse_duration("185"), Some(185));
        assert_eq!(parse_duration("120:00"), Some(7_200));
        assert_eq!(parse_duration("120:01"), None);
        assert_eq!(parse_duration("-5"), None);
        assert_eq!(parse_capo("0"), Some(0));
    }

    #[test]
    fn normalizes_text_and_strips_invisible_characters() {
        let content = "\u{FEFF}{title: Cafe\u{0301}}\r\n{artist: A\u{202E}B\u{200B}C}\rLine\twith tab\u{0007}\r\n\u{2066}hidden\u{2069}";
        let parsed = parse(content, None).unwrap();
        assert_eq!(parsed.title, "Café");
        assert_eq!(parsed.title.chars().count(), 4);
        assert_eq!(parsed.artist_name.as_deref(), Some("ABC"));
        assert_eq!(parsed.lyrics.as_deref(), Some("Line    with tab\nhidden"));
    }

    #[test]
    fn markup_stays_inert_text() {
        let parsed = parse(
            "{title: <script>alert(1)</script>}\n<img src=x onerror=alert(1)>\n{c: <b>bold</b>}\n{comment: {nested}}",
            None,
        )
        .unwrap();
        assert_eq!(parsed.title, "<script>alert(1)</script>");
        let lyrics = parsed.lyrics.unwrap();
        assert!(lyrics.contains("<img src=x onerror=alert(1)>"));
        assert!(lyrics.contains("{c: <b>bold</b>}"));
        assert!(lyrics.contains("{comment: nested}"));
    }

    #[test]
    fn braces_in_lyrics_are_removed_and_lengths_are_capped() {
        let parsed = parse(
            &format!(
                "{{title: {}}}\n{{artist: {}}}\nhello {{world\n{{unclosed: x",
                "t".repeat(300),
                "a".repeat(300)
            ),
            None,
        );
        // A 300-character directive line is still within the line limit.
        let parsed = parsed.unwrap();
        assert_eq!(parsed.title.chars().count(), MAX_TITLE_CHARS);
        assert_eq!(
            parsed.artist_name.as_deref().map(|a| a.chars().count()),
            Some(MAX_ARTIST_CHARS)
        );
        let codes = codes(&parsed);
        assert!(codes.contains(&"title_truncated"));
        assert!(codes.contains(&"artist_truncated"));
        assert!(codes.contains(&"braces_removed"));
        assert_eq!(parsed.lyrics.as_deref(), Some("hello world\nunclosed: x"));
    }

    #[test]
    fn warnings_are_bounded() {
        let content = format!("{{title: X}}\n{}", "[nope] \n".repeat(500));
        let parsed = parse(&content, None).unwrap();
        assert_eq!(parsed.warnings.len(), MAX_WARNINGS + 1);
        assert_eq!(parsed.warnings.last().unwrap().code, "too_many_warnings");
    }

    #[test]
    fn empty_bodies_have_no_lyrics() {
        let parsed = parse("{title: X}\n\n\n", None).unwrap();
        assert_eq!(parsed.lyrics, None);
        let parsed = parse("{title: X}\n\n\na\n\n\n\n\nb\n\n", None).unwrap();
        assert_eq!(parsed.lyrics.as_deref(), Some("a\n\n\nb"));
    }
}
