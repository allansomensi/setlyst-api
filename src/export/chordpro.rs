//! ChordPro export (`.cho`), one song or a whole library.

use crate::models::song::SongExport;

/// Longest directive value written.
const MAX_DIRECTIVE_VALUE: usize = 255;

/// A directive value that can't break out of its `{name: value}` braces
/// or span lines.
fn directive_value(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .map(|c| {
            if c.is_control() || c == '{' || c == '}' {
                ' '
            } else {
                c
            }
        })
        .collect();
    cleaned
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_DIRECTIVE_VALUE)
        .collect()
}

/// `m:ss` (hours roll into minutes: `75:00`).
pub fn format_duration(seconds: i32) -> String {
    let seconds = seconds.max(0);
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// Lyrics safe to embed in a multi-song file: a stored chart can't start a
/// new song of its own.
fn embeddable_lyrics(lyrics: &str) -> String {
    lyrics
        .lines()
        .filter(|line| {
            let directive = line
                .trim()
                .trim_start_matches('{')
                .split([':', '}'])
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase();
            !(line.trim_start().starts_with('{') && (directive == "new_song" || directive == "ns"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// One song as ChordPro: metadata directives, a blank line, the lyrics.
pub fn render_song(song: &SongExport) -> String {
    let mut out = String::new();
    let mut push = |name: &str, value: &str| {
        let value = directive_value(value);
        if !value.is_empty() {
            out.push_str(&format!("{{{name}: {value}}}\n"));
        }
    };

    push("title", &song.title);
    if let Some(artist) = &song.artist_name {
        push("artist", artist);
    }
    if let Some(key) = &song.tonality {
        push("key", key);
    }
    if let Some(tempo) = song.tempo {
        push("tempo", &tempo.to_string());
    }
    if let Some(time) = &song.time_signature {
        push("time", time);
    }
    if let Some(capo) = song.capo {
        push("capo", &capo.to_string());
    }
    if let Some(duration) = song.duration {
        push("duration", &format_duration(duration));
    }
    if let Some(energy) = song.energy {
        push("meta", &format!("energy {energy}"));
    }

    out.push('\n');
    match song.lyrics.as_deref().filter(|l| !l.trim().is_empty()) {
        Some(lyrics) => out.push_str(&embeddable_lyrics(lyrics)),
        None => out.push_str("# No lyrics provided."),
    }
    out.push('\n');
    out
}

/// Several songs in one file, separated by `{new_song}`.
pub fn render_songs(songs: &[SongExport]) -> String {
    songs
        .iter()
        .map(render_song)
        .collect::<Vec<_>>()
        .join("\n{new_song}\n\n")
}

/// A safe download name: `song-<slug>.cho`.
pub fn chordpro_filename(title: &str) -> String {
    crate::export::pdf::slug_filename("song", title, "cho")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song() -> SongExport {
        SongExport {
            title: "Garota {de} Ipanema".to_string(),
            artist_name: Some("Tom\nJobim".to_string()),
            tonality: Some("F".to_string()),
            tempo: Some(128),
            lyrics: Some("[F]Olha que coisa\n{new_song}\n{title: injected}".to_string()),
            time_signature: Some("4/4".to_string()),
            capo: Some(2),
            duration: Some(185),
            energy: Some(3),
        }
    }

    #[test]
    fn renders_every_directive_safely() {
        let text = render_song(&song());
        assert!(text.starts_with("{title: Garota de Ipanema}\n"));
        assert!(text.contains("{artist: Tom Jobim}\n"));
        assert!(text.contains("{key: F}\n{tempo: 128}\n{time: 4/4}\n{capo: 2}\n"));
        assert!(text.contains("{duration: 3:05}\n{meta: energy 3}\n"));
        assert!(!text.contains("{new_song}"));
        assert!(text.contains("[F]Olha que coisa"));
    }

    #[test]
    fn several_songs_are_separated() {
        let text = render_songs(&[song(), song()]);
        assert_eq!(text.matches("{new_song}").count(), 1);
        assert_eq!(format_duration(4_500), "75:00");
        assert_eq!(
            chordpro_filename("Garota de Ipanema"),
            "song-garota-de-ipanema.cho"
        );
    }
}
