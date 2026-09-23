//! Offensive-term detection for usernames and other short public text.
//!
//! Matching works on a normalized form of the text, so the usual evasion
//! tricks don't help:
//!
//! - lower case, accents stripped (`Café` → `cafe`);
//! - leetspeak mapped back (`0→o 1→i 3→e 4→a 5→s 7→t @→a $→s`);
//! - repeated letters collapsed (`fuuuck` → `fuck`);
//! - separators removed between single letters (`f.u.c.k` → `fuck`).
//!
//! A term matches a whole token, or, for terms marked as substring-safe
//! (by default the ones of 5+ letters), anywhere inside a token. Tokens in
//! the allowlist are skipped entirely, which avoids the classic
//! "Scunthorpe problem" (innocent words containing a listed term).
//!
//! The list is intentionally focused on unambiguous slurs, hate terms,
//! explicit sexual terms and common obscene insults in Portuguese, English
//! and Spanish. Everything except exact slurs in usernames only *flags* the
//! value for a human moderator, so a false positive costs a review, not a
//! blocked user.

use std::{collections::HashSet, sync::LazyLock};

/// How a listed term is classified (and reported in flag reasons).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TermKind {
    /// Slurs and hate terms (racist, homophobic, transphobic, ableist,
    /// nazi references). Reason `hate_term`.
    Hate,
    /// Explicit sexual terms. Reason `sexual_term`.
    Sexual,
    /// Obscene insults and profanity. Reason `offensive_term`.
    Obscene,
}

impl TermKind {
    pub fn reason(&self) -> &'static str {
        match self {
            TermKind::Hate => "hate_term",
            TermKind::Sexual => "sexual_term",
            TermKind::Obscene => "offensive_term",
        }
    }

    /// Confidence reported on automatic flags.
    pub fn score(&self) -> f32 {
        match self {
            TermKind::Hate => 1.0,
            TermKind::Sexual => 0.9,
            TermKind::Obscene => 0.7,
        }
    }
}

/// Matching mode of a term.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Length decides: substring when the normalized term has 5+ letters.
    Auto,
    /// Whole token only (short or ambiguous terms).
    Token,
    /// Also inside tokens, whatever the length (unambiguous short terms).
    Substring,
}

use Mode::{Auto, Substring, Token};
use TermKind::{Hate, Obscene, Sexual};

/// `(term, kind, mode)`. Written in plain lower-case ASCII; normalized at
/// start-up with the same pipeline as the input.
const TERMS: &[(&str, TermKind, Mode)] = &[
    // --- Slurs and hate terms: English ---
    ("nigger", Hate, Auto),
    ("nigga", Hate, Auto),
    ("faggot", Hate, Auto),
    ("fag", Hate, Token),
    ("chink", Hate, Token),
    ("spic", Hate, Token),
    ("kike", Hate, Token),
    ("wetback", Hate, Auto),
    ("tranny", Hate, Auto),
    ("retard", Hate, Token),
    ("dyke", Hate, Token),
    ("gook", Hate, Token),
    ("raghead", Hate, Auto),
    ("towelhead", Hate, Auto),
    ("beaner", Hate, Token),
    ("paki", Hate, Token),
    // --- Slurs and hate terms: Portuguese ---
    ("viado", Hate, Token),
    ("bicha", Hate, Token),
    ("sapatao", Hate, Auto),
    ("traveco", Hate, Auto),
    ("crioulo", Hate, Token),
    ("baitola", Hate, Auto),
    ("boiola", Hate, Auto),
    ("retardado", Hate, Token),
    ("retardada", Hate, Token),
    ("mongoloide", Hate, Auto),
    // --- Slurs and hate terms: Spanish ---
    ("maricon", Hate, Auto),
    ("marica", Hate, Token),
    ("sudaca", Hate, Auto),
    ("negrata", Hate, Auto),
    ("bollera", Hate, Auto),
    // --- Nazi and hate references ---
    ("hitler", Hate, Auto),
    ("nazi", Hate, Token),
    ("nazista", Hate, Auto),
    ("heil", Hate, Token),
    ("siegheil", Hate, Auto),
    ("whitepower", Hate, Auto),
    ("whitesupremacy", Hate, Auto),
    // --- Sexual: English ---
    ("porn", Sexual, Substring),
    ("porno", Sexual, Auto),
    ("xxx", Sexual, Token),
    ("dick", Sexual, Token),
    ("cock", Sexual, Token),
    ("pussy", Sexual, Auto),
    ("cunt", Sexual, Token),
    ("blowjob", Sexual, Auto),
    ("handjob", Sexual, Auto),
    ("cumshot", Sexual, Auto),
    ("dildo", Sexual, Auto),
    ("boobs", Sexual, Auto),
    ("tits", Sexual, Token),
    ("titties", Sexual, Auto),
    ("anal", Sexual, Token),
    ("orgasm", Sexual, Auto),
    ("nude", Sexual, Token),
    ("nudes", Sexual, Token),
    ("nsfw", Sexual, Substring),
    ("hentai", Sexual, Auto),
    ("milf", Sexual, Token),
    ("rimjob", Sexual, Auto),
    ("bukkake", Sexual, Auto),
    ("gangbang", Sexual, Auto),
    ("horny", Sexual, Auto),
    ("penis", Sexual, Auto),
    ("onlyfans", Sexual, Auto),
    ("camgirl", Sexual, Auto),
    ("hooker", Sexual, Token),
    ("slut", Sexual, Token),
    ("whore", Sexual, Auto),
    // --- Sexual: Portuguese ---
    ("buceta", Sexual, Auto),
    ("boceta", Sexual, Auto),
    ("xoxota", Sexual, Auto),
    ("xereca", Sexual, Auto),
    ("piroca", Sexual, Auto),
    ("pica", Sexual, Token),
    ("punheta", Sexual, Auto),
    ("siririca", Sexual, Auto),
    ("gozada", Sexual, Token),
    ("putaria", Sexual, Auto),
    ("puta", Sexual, Token),
    ("putinha", Sexual, Auto),
    // --- Sexual: Spanish ---
    ("polla", Sexual, Token),
    ("verga", Sexual, Token),
    ("follar", Sexual, Auto),
    ("mamada", Sexual, Token),
    ("chocho", Sexual, Token),
    ("zorra", Sexual, Token),
    // --- Obscene: English ---
    ("fuck", Obscene, Substring),
    ("motherfucker", Obscene, Auto),
    ("shit", Obscene, Token),
    ("bullshit", Obscene, Auto),
    ("asshole", Obscene, Auto),
    ("bitch", Obscene, Auto),
    ("bastard", Obscene, Token),
    ("dickhead", Obscene, Auto),
    ("wanker", Obscene, Auto),
    ("twat", Obscene, Token),
    ("prick", Obscene, Token),
    ("cocksucker", Obscene, Auto),
    ("jackass", Obscene, Auto),
    // --- Obscene: Portuguese ---
    ("caralho", Obscene, Auto),
    ("porra", Obscene, Token),
    ("foda", Obscene, Token),
    ("fodase", Obscene, Auto),
    ("foder", Obscene, Token),
    ("fuder", Obscene, Token),
    ("merda", Obscene, Token),
    ("cu", Obscene, Token),
    ("cuzao", Obscene, Auto),
    ("arrombado", Obscene, Auto),
    ("vadia", Obscene, Token),
    ("vagabunda", Obscene, Auto),
    ("puto", Obscene, Token),
    ("otario", Obscene, Token),
    ("babaca", Obscene, Token),
    // --- Obscene: Spanish ---
    ("cabron", Obscene, Auto),
    ("pendejo", Obscene, Auto),
    ("chingar", Obscene, Auto),
    ("chingada", Obscene, Auto),
    ("joder", Obscene, Token),
    ("gilipollas", Obscene, Auto),
    ("hijoputa", Obscene, Auto),
    ("hijodeputa", Obscene, Auto),
    ("malparido", Obscene, Auto),
    ("culero", Obscene, Auto),
    ("mierda", Obscene, Auto),
    ("culo", Obscene, Token),
    ("pajero", Obscene, Token),
];

/// Innocent words that contain a listed term. Compared against the
/// normalized (not collapsed) token.
const ALLOWLIST: &[&str] = &[
    "scunthorpe",
    "penistone",
    "assessor",
    "assessora",
    "assassin",
    "assassino",
    "assassinato",
    "assistant",
    "classic",
    "classico",
    "classica",
    "cocktail",
    "coquetel",
    "peacock",
    "hancock",
    "analise",
    "analises",
    "analysis",
    "analista",
    "pistola",
    "pistolas",
    "niger",
    "nigeria",
    "nigerian",
    "shiitake",
    "shitake",
    "cumprimento",
    "pussycat",
    "thorny",
    "retardant",
    "retardar",
    "retardo",
    "dickens",
    "dickinson",
    "therapist",
    "grape",
    "skyscraper",
    "matsushita",
    "scrapbook",
    "sussex",
    "essex",
    "middlesex",
    "sextet",
    "sexteto",
    "sexta",
    "sexto",
];

/// A term, normalized once.
#[derive(Debug)]
struct Entry {
    collapsed: String,
    raw: String,
    kind: TermKind,
    substring: bool,
}

static ENTRIES: LazyLock<Vec<Entry>> = LazyLock::new(|| {
    TERMS
        .iter()
        .map(|(term, kind, mode)| {
            let raw: String = term.chars().filter_map(fold_char).collect();
            let collapsed = collapse(&raw);
            let substring = match mode {
                Auto => collapsed.chars().count() >= 5,
                Token => false,
                Substring => true,
            };
            Entry {
                collapsed,
                raw,
                kind: *kind,
                substring,
            }
        })
        .collect()
});

static ALLOWED: LazyLock<HashSet<String>> = LazyLock::new(|| {
    ALLOWLIST
        .iter()
        .map(|w| w.chars().filter_map(fold_char).collect())
        .collect()
});

/// Folds one character: lower case, accents removed, leetspeak mapped.
/// Returns `None` for characters that are dropped entirely (combining
/// marks), and a separator (`' '`) for anything that isn't a letter or a
/// digit.
fn fold_char(c: char) -> Option<char> {
    let lower = c.to_lowercase().next().unwrap_or(c);
    let mapped = match lower {
        'á' | 'à' | 'â' | 'ã' | 'ä' | 'å' | 'ā' => 'a',
        'é' | 'è' | 'ê' | 'ë' | 'ē' => 'e',
        'í' | 'ì' | 'î' | 'ï' | 'ī' => 'i',
        'ó' | 'ò' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' => 'o',
        'ú' | 'ù' | 'û' | 'ü' | 'ū' => 'u',
        'ç' => 'c',
        'ñ' => 'n',
        'ý' | 'ÿ' => 'y',
        'ß' => 's',
        '0' => 'o',
        '1' => 'i',
        '3' => 'e',
        '4' => 'a',
        '5' => 's',
        '7' => 't',
        '@' => 'a',
        '$' => 's',
        '\u{0300}'..='\u{036f}' => return None,
        c if c.is_ascii_alphanumeric() => c,
        _ => ' ',
    };
    Some(mapped)
}

/// Collapses runs of the same character (`fuuuck` → `fuck`).
fn collapse(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut previous = None;
    for c in value.chars() {
        if Some(c) != previous {
            out.push(c);
        }
        previous = Some(c);
    }
    out
}

/// Normalized tokens of `text` (see the module docs). Runs of
/// single-letter tokens are joined, so `f.u.c.k` becomes `fuck`.
pub fn normalize(text: &str) -> Vec<String> {
    let folded: String = text.chars().filter_map(fold_char).collect();
    let mut tokens: Vec<String> = Vec::new();
    let mut run = String::new();
    for token in folded.split(' ').filter(|t| !t.is_empty()) {
        if token.chars().count() == 1 {
            run.push_str(token);
            continue;
        }
        if run.chars().count() > 1 {
            tokens.push(std::mem::take(&mut run));
        } else {
            run.clear();
        }
        tokens.push(token.to_string());
    }
    if run.chars().count() > 1 {
        tokens.push(run);
    }
    tokens
}

/// One listed term found in a text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermMatch {
    pub term: String,
    pub kind: TermKind,
    /// `true` when the whole token is the term (not just contained in it).
    pub exact: bool,
}

/// Every listed term found in `text`.
pub fn find_terms(text: &str) -> Vec<TermMatch> {
    let mut matches: Vec<TermMatch> = Vec::new();
    for token in normalize(text) {
        if ALLOWED.contains(&token) {
            continue;
        }
        let collapsed = collapse(&token);
        for entry in ENTRIES.iter() {
            // Very short collapsed forms ("xxx" → "x") only match the raw
            // token, or they would match almost anything.
            let exact = if entry.collapsed.chars().count() >= 3 {
                collapsed == entry.collapsed || token == entry.raw
            } else {
                token == entry.raw
            };
            let contained = !exact && entry.substring && collapsed.contains(&entry.collapsed);
            if (exact || contained)
                && !matches
                    .iter()
                    .any(|m| m.term == entry.raw && m.exact == exact)
            {
                matches.push(TermMatch {
                    term: entry.raw.clone(),
                    kind: entry.kind,
                    exact,
                });
            }
        }
    }
    matches
}

/// `true` when `text` contains a slur as a whole token. Such usernames are
/// refused outright instead of merely flagged.
pub fn contains_exact_slur(text: &str) -> bool {
    find_terms(text)
        .iter()
        .any(|m| m.exact && m.kind == TermKind::Hate)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flagged(text: &str) -> bool {
        !find_terms(text).is_empty()
    }

    #[test]
    fn normalization_strips_accents_leetspeak_and_separators() {
        assert_eq!(normalize("Café Açaí"), vec!["cafe", "acai"]);
        assert_eq!(normalize("h3ll0_w0rld"), vec!["hello", "world"]);
        assert_eq!(normalize("f.u.c.k you"), vec!["fuck", "you"]);
        assert_eq!(normalize("a-b"), vec!["ab"]);
        assert_eq!(normalize("-"), Vec::<String>::new());
        assert_eq!(normalize("$h1t"), vec!["shit"]);
        assert_eq!(collapse("fuuuuck"), "fuck");
    }

    #[test]
    fn detects_evasions() {
        for text in [
            "fuck",
            "FUCK",
            "fuuuuck",
            "f.u.c.k",
            "f_u_c_k",
            "phuck.fuck",
            "xfuckx",
            "p0rn",
            "caralh0",
            "m3rda",
            "Buc3ta",
            "sh1t",
            "@sshole",
            "puta",
        ] {
            assert!(flagged(text), "{text} should be flagged");
        }
    }

    #[test]
    fn classifies_terms() {
        assert_eq!(find_terms("n1gg3r")[0].kind, TermKind::Hate);
        assert!(find_terms("n1gg3r")[0].exact);
        assert_eq!(find_terms("hentai.lover")[0].kind, TermKind::Sexual);
        assert_eq!(find_terms("bitchy")[0].kind, TermKind::Obscene);
        assert!(!find_terms("bitchy")[0].exact);
        assert_eq!(TermKind::Hate.reason(), "hate_term");
    }

    #[test]
    fn avoids_common_false_positives() {
        for text in [
            "Scunthorpe",
            "assessor",
            "classic_rock",
            "cocktail",
            "analise",
            "pistola",
            "Niger",
            "shiitake",
            "computador",
            "disputa",
            "deputado",
            "enviado",
            "desviado",
            "therapist",
            "hancock",
            "peacock",
            "cumprimento",
            "grape",
            "sexteto",
            "johnleehooker",
            "dickdale.fan",
            "retardar",
            "bichano",
            "joao.silva",
            "ana_maria",
            "dj-kiko",
            "user42",
        ] {
            assert!(
                !flagged(text),
                "{text} should not be flagged: {:?}",
                find_terms(text)
            );
        }
    }

    #[test]
    fn exact_slurs_are_detected_for_rejection() {
        assert!(contains_exact_slur("nigger"));
        assert!(contains_exact_slur("big.faggot"));
        assert!(contains_exact_slur("V1ado"));
        // Contained, not exact: flagged, not refused.
        assert!(!contains_exact_slur("niggerlover"));
        assert!(flagged("niggerlover"));
        // Obscene words are never refused outright.
        assert!(!contains_exact_slur("fuck"));
        assert!(!contains_exact_slur("Niger"));
    }
}
