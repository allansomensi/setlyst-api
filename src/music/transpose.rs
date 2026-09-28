//! Chord and key transposition, for the places the API renders a song in
//! the key a setlist plays it in (the setlist PDF and the public share).
//!
//! Mirrors the web app's `lib/music/chords.ts`: spelling (sharps or flats)
//! is decided once, from the *destination key's* signature, and applied to
//! every chord — a song in C taken up a semitone is written in Db, never
//! C#, and its fourth degree is Gb, never F#.

use crate::{
    export::pdf::{is_chord, is_chord_only_line},
    models::song::{SongWithArtist, Tonality},
};

/// An octave either way is the whole useful range.
pub const MAX_TRANSPOSE: i16 = 11;

const SHARP_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];
const FLAT_NAMES: [&str; 12] = [
    "C", "Db", "D", "Eb", "E", "F", "Gb", "G", "Ab", "A", "Bb", "B",
];

/// The conventional written form of each major and minor key (fewest
/// accidentals; F# over Gb on the tie).
const MAJOR_KEYS: [&str; 12] = [
    "C", "Db", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B",
];
const MINOR_KEYS: [&str; 12] = [
    "Cm", "C#m", "Dm", "Ebm", "Em", "Fm", "F#m", "Gm", "G#m", "Am", "Bbm", "Bm",
];

/// Each key's signature: positive counts sharps, negative counts flats.
const MAJOR_SIGNATURE: [i8; 12] = [0, -5, 2, -3, 4, -1, 6, 1, -4, 3, -2, 5];
const MINOR_SIGNATURE: [i8; 12] = [-3, 4, -1, -6, 1, -4, 3, -2, 5, 0, -5, 2];

fn wrap(pitch: i32) -> usize {
    pitch.rem_euclid(12) as usize
}

/// A note name at the start of `s` ("F#m7" -> pitch 6, 2 bytes).
fn leading_note(s: &str) -> Option<(i32, usize)> {
    let mut chars = s.chars();
    let base = match chars.next()? {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    };
    match chars.next() {
        Some('#') => Some((base + 1, 2)),
        Some('b') => Some((base - 1, 2)),
        _ => Some((base, 1)),
    }
}

fn spell(pitch: i32, flats: bool) -> &'static str {
    if flats {
        FLAT_NAMES[wrap(pitch)]
    } else {
        SHARP_NAMES[wrap(pitch)]
    }
}

fn tonality_name(tonality: Tonality) -> String {
    serde_json::to_value(tonality)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// `(pitch class, is_minor)` of a key or a chord's root and quality.
fn key_of(symbol: &str) -> Option<(i32, bool)> {
    let (pitch, len) = leading_note(symbol)?;
    let rest = &symbol[len..];
    let minor = rest.starts_with('m') && !rest.starts_with("maj");
    Some((pitch, minor))
}

/// The key reached by moving `tonality` by `semitones`.
pub fn transpose_tonality(tonality: Tonality, semitones: i16) -> Tonality {
    if semitones == 0 {
        return tonality;
    }
    let Some((pitch, minor)) = key_of(&tonality_name(tonality)) else {
        return tonality;
    };
    let target = pitch + i32::from(semitones);
    let name = if minor {
        MINOR_KEYS[wrap(target)]
    } else {
        MAJOR_KEYS[wrap(target)]
    };
    serde_json::from_value(serde_json::Value::String(name.to_string())).unwrap_or(tonality)
}

/// Whether the transposed song is written with flats: decided by the
/// destination key (the stored one, or the first chord as a stand-in).
pub fn prefers_flats(
    tonality: Option<Tonality>,
    semitones: i16,
    first_chord: Option<&str>,
) -> bool {
    let key = tonality
        .map(tonality_name)
        .as_deref()
        .and_then(key_of)
        .or_else(|| first_chord.and_then(key_of));
    let Some((pitch, minor)) = key else {
        return false;
    };
    let index = wrap(pitch + i32::from(semitones));
    let signature = if minor {
        MINOR_SIGNATURE[index]
    } else {
        MAJOR_SIGNATURE[index]
    };
    signature < 0
}

/// Transposes one chord symbol (its bass note too: `C/E` up two is
/// `D/F#`). Anything that isn't a chord comes back unchanged.
pub fn transpose_chord(symbol: &str, semitones: i16, flats: bool) -> String {
    let trimmed = symbol.trim();
    if semitones == 0 || !is_chord(trimmed) {
        return symbol.to_string();
    }
    let Some((root, root_len)) = leading_note(trimmed) else {
        // "N.C."
        return symbol.to_string();
    };
    let shift = i32::from(semitones);

    let (main, bass) = match trimmed.rsplit_once('/') {
        Some((main, bass)) if leading_note(bass).is_some_and(|(_, len)| len == bass.len()) => {
            (main, Some(bass))
        }
        _ => (trimmed, None),
    };
    let mut out = String::with_capacity(trimmed.len() + 2);
    out.push_str(spell(root + shift, flats));
    out.push_str(&main[root_len..]);
    if let Some((bass_pitch, _)) = bass.and_then(leading_note) {
        out.push('/');
        out.push_str(spell(bass_pitch + shift, flats));
    }
    out
}

/// The first chord of a chart (bracketed or on a chords-only line).
fn first_chord(lyrics: &str) -> Option<String> {
    for line in lyrics.lines() {
        let mut rest = line;
        while let Some(open) = rest.find('[') {
            let Some(close) = rest[open..].find(']') else {
                break;
            };
            let inner = rest[open + 1..open + close].trim();
            if is_chord(inner) && leading_note(inner).is_some() {
                return Some(inner.to_string());
            }
            rest = &rest[open + close + 1..];
        }
        if !line.contains('[') && is_chord_only_line(line) {
            let found = line
                .split_whitespace()
                .map(|t| t.trim_matches(|c| c == '(' || c == ')'))
                .find(|t| is_chord(t) && leading_note(t).is_some());
            if let Some(chord) = found {
                return Some(chord.to_string());
            }
        }
    }
    None
}

/// Rewrites every chord of a chart — `[G]` inline chords and the lines of
/// chords written above the lyrics — leaving lyrics, headings and
/// directives alone. Chords above the lyrics keep their columns, so they
/// still sit over the right syllable.
pub fn transpose_lyrics(lyrics: &str, semitones: i16, flats: bool) -> String {
    if semitones == 0 {
        return lyrics.to_string();
    }
    let mut out = String::with_capacity(lyrics.len() + 16);
    for (index, line) in lyrics.split('\n').enumerate() {
        if index > 0 {
            out.push('\n');
        }
        let trimmed = line.trim();
        if trimmed.starts_with('{') || trimmed.starts_with('#') {
            out.push_str(line);
        } else if line.contains('[') {
            out.push_str(&transpose_bracketed(line, semitones, flats));
        } else if is_chord_only_line(line) {
            out.push_str(&transpose_chord_line(line, semitones, flats));
        } else {
            out.push_str(line);
        }
    }
    out
}

fn transpose_bracketed(line: &str, semitones: i16, flats: bool) -> String {
    let mut out = String::with_capacity(line.len() + 8);
    let mut rest = line;
    while let Some(open) = rest.find('[') {
        let Some(close) = rest[open..].find(']') else {
            break;
        };
        let inner = &rest[open + 1..open + close];
        out.push_str(&rest[..=open]);
        out.push_str(&transpose_chord(inner, semitones, flats));
        out.push(']');
        rest = &rest[open + close + 1..];
    }
    out.push_str(rest);
    out
}

/// A chords-only line, each chord kept at its column (or right after the
/// previous one, when a longer name pushes it along).
fn transpose_chord_line(line: &str, semitones: i16, flats: bool) -> String {
    let mut out = String::with_capacity(line.len() + 8);
    let mut column = 0usize;
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && !chars[i].is_whitespace() {
            i += 1;
        }
        let token: String = chars[start..i].iter().collect();
        let (open, core, close) = split_parens(&token);
        let rewritten = format!("{open}{}{close}", transpose_chord(core, semitones, flats));

        let target = if column == 0 {
            start
        } else {
            start.max(column + 1)
        };
        while column < target {
            out.push(' ');
            column += 1;
        }
        column += rewritten.chars().count();
        out.push_str(&rewritten);
    }
    out
}

/// "(G)" -> ("(", "G", ")").
fn split_parens(token: &str) -> (&str, &str, &str) {
    let open_len = token.len() - token.trim_start_matches('(').len();
    let inner = &token[open_len..];
    let core = inner.trim_end_matches(')');
    (&token[..open_len], core, &inner[core.len()..])
}

/// Puts a setlist song in the key the setlist plays it in (its
/// `transpose`): the key shown and every chord of the chart. The offset is
/// consumed, so applying it twice is harmless.
pub fn apply_setlist_key(song: &mut SongWithArtist) {
    let semitones = song
        .transpose
        .take()
        .unwrap_or(0)
        .clamp(-MAX_TRANSPOSE, MAX_TRANSPOSE);
    if semitones == 0 {
        return;
    }
    if let Some(lyrics) = song.lyrics.as_deref() {
        let flats = prefers_flats(song.tonality, semitones, first_chord(lyrics).as_deref());
        song.lyrics = Some(transpose_lyrics(lyrics, semitones, flats));
    }
    song.tonality = song.tonality.map(|t| transpose_tonality(t, semitones));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_spelled_conventionally() {
        assert_eq!(transpose_tonality(Tonality::C, 1), Tonality::Db);
        assert_eq!(transpose_tonality(Tonality::G, -1), Tonality::FSharp);
        assert_eq!(transpose_tonality(Tonality::Am, 3), Tonality::Cm);
        assert_eq!(transpose_tonality(Tonality::E, -2), Tonality::D);
        assert_eq!(transpose_tonality(Tonality::Em, 4), Tonality::GSharpM);
        assert_eq!(transpose_tonality(Tonality::A, 0), Tonality::A);
    }

    #[test]
    fn chords_follow_the_destination_key() {
        // C -> Db major: flats.
        let flats = prefers_flats(Some(Tonality::C), 1, None);
        assert!(flats);
        assert_eq!(transpose_chord("C", 1, flats), "Db");
        assert_eq!(transpose_chord("F#m7", 1, flats), "Gm7");
        // G -> A major: sharps.
        let flats = prefers_flats(Some(Tonality::G), 2, None);
        assert!(!flats);
        assert_eq!(transpose_chord("C/E", 2, flats), "D/F#");
        assert_eq!(transpose_chord("Bbmaj7", 2, flats), "Cmaj7");
        assert_eq!(transpose_chord("A7/9", 2, flats), "B7/9");
        // Not chords.
        assert_eq!(transpose_chord("Chorus", 2, flats), "Chorus");
        assert_eq!(transpose_chord("N.C.", 2, flats), "N.C.");
    }

    #[test]
    fn inline_and_above_lyrics_charts() {
        let chart =
            "{title: X}\n[Intro]\n[G]Hello [D/F#]world\nG      D\nHello old friend\n  E\nBye";
        let out = transpose_lyrics(chart, 2, false);
        assert_eq!(
            out,
            "{title: X}\n[Intro]\n[A]Hello [E/G#]world\nA      E\nHello old friend\n  F#\nBye"
        );
    }

    #[test]
    fn longer_names_push_later_chords_along() {
        assert_eq!(transpose_chord_line("C D", 1, true), "Db Eb");
        // "B" keeps its column.
        assert_eq!(transpose_chord_line("(G)  C", -1, false), "(F#) B");
    }

    #[test]
    fn applying_a_setlist_key_consumes_it() {
        let now = chrono::Utc::now().naive_utc();
        let mut song = SongWithArtist {
            id: uuid::Uuid::new_v4(),
            title: "Song".to_string(),
            artist_id: uuid::Uuid::new_v4(),
            artist_name: "Artist".to_string(),
            user_id: uuid::Uuid::new_v4(),
            band_id: None,
            forked_from: None,
            version_label: None,
            version_of: None,
            tempo: None,
            lyrics: Some("[C]La [G]la".to_string()),
            tonality: Some(Tonality::C),
            genre: None,
            duration: None,
            energy: None,
            time_signature: None,
            capo: None,
            tuning: None,
            performance_notes: None,
            links: Default::default(),
            tags: Vec::new(),
            updated_by: None,
            updated_by_username: None,
            source_synced_at: None,
            transpose: Some(-3),
            added_by: None,
            added_by_username: None,
            added_by_avatar_url: None,
            added_at: None,
            created_at: now,
            updated_at: now,
        };
        apply_setlist_key(&mut song);
        assert_eq!(song.tonality, Some(Tonality::A));
        assert_eq!(song.lyrics.as_deref(), Some("[A]La [E]la"));
        assert_eq!(song.transpose, None);
        apply_setlist_key(&mut song);
        assert_eq!(song.tonality, Some(Tonality::A));
    }
}
