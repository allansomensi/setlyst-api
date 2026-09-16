use crate::models::setlist::SetlistItem;
use crate::models::song::Tonality;
use genpdf::{Alignment, Document, Element, SimplePageDecorator, elements, fonts, style};
use serde::Deserialize;
use std::io::Cursor;
use utoipa::{IntoParams, ToSchema};

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ExportQuery {
    #[serde(default)]
    pub show_title: bool,
    #[serde(default)]
    pub show_total_duration: bool,
    #[serde(default)]
    pub show_key: bool,
    #[serde(default)]
    pub show_bpm: bool,
    // Blocks and breaks default to shown — they were already part of the
    // setlist's running order before this option existed, so an absent
    // query param must not silently hide them.
    #[serde(default = "default_true")]
    pub show_blocks: bool,
    #[serde(default = "default_true")]
    pub show_breaks: bool,
    #[serde(default)]
    pub lang: PdfLocale,
}

impl From<ExportQuery> for PdfExportOptions {
    fn from(query: ExportQuery) -> Self {
        Self {
            show_title: query.show_title,
            show_total_duration: query.show_total_duration,
            show_key: query.show_key,
            show_bpm: query.show_bpm,
            show_blocks: query.show_blocks,
            show_breaks: query.show_breaks,
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

impl PdfLocale {
    fn labels(&self) -> PdfLabels {
        match self {
            Self::PtBr => PdfLabels {
                estimated_duration: "Duração estimada",
                not_calculated: "Não calculada",
                key: "Tom",
                break_label: "Pausa",
            },
            Self::En => PdfLabels {
                estimated_duration: "Estimated duration",
                not_calculated: "Not calculated",
                key: "Key",
                break_label: "Break",
            },
            Self::Es => PdfLabels {
                estimated_duration: "Duración estimada",
                not_calculated: "No calculada",
                key: "Tono",
                break_label: "Pausa",
            },
        }
    }
}

struct PdfLabels {
    estimated_duration: &'static str,
    not_calculated: &'static str,
    key: &'static str,
    break_label: &'static str,
}

#[derive(Debug, Clone, Default)]
pub struct PdfExportOptions {
    pub show_title: bool,
    pub show_total_duration: bool,
    pub show_key: bool,
    pub show_bpm: bool,
    pub show_blocks: bool,
    pub show_breaks: bool,
    pub locale: PdfLocale,
}

fn format_tonality(t: &Tonality) -> &'static str {
    match t {
        Tonality::C => "C",
        Tonality::CSharp => "C#",
        Tonality::Db => "Db",
        Tonality::D => "D",
        Tonality::DSharp => "D#",
        Tonality::Eb => "Eb",
        Tonality::E => "E",
        Tonality::ESharp => "E#",
        Tonality::F => "F",
        Tonality::FSharp => "F#",
        Tonality::Gb => "Gb",
        Tonality::G => "G",
        Tonality::GSharp => "G#",
        Tonality::Ab => "Ab",
        Tonality::A => "A",
        Tonality::ASharp => "A#",
        Tonality::Bb => "Bb",
        Tonality::B => "B",
        Tonality::BSharp => "B#",
        Tonality::Cm => "Cm",
        Tonality::CSharpM => "C#m",
        Tonality::Dbm => "Dbm",
        Tonality::Dm => "Dm",
        Tonality::DSharpM => "D#m",
        Tonality::Ebm => "Ebm",
        Tonality::Em => "Em",
        Tonality::ESharpM => "E#m",
        Tonality::Fm => "Fm",
        Tonality::FSharpM => "F#m",
        Tonality::Gbm => "Gbm",
        Tonality::Gm => "Gm",
        Tonality::GSharpM => "G#m",
        Tonality::Abm => "Abm",
        Tonality::Am => "Am",
        Tonality::ASharpM => "A#m",
        Tonality::Bbm => "Bbm",
        Tonality::Bm => "Bm",
        Tonality::BSharpM => "B#m",
    }
}

pub fn generate_setlist_pdf(
    setlist_title: &str,
    total_duration_secs: i32,
    items: &[SetlistItem],
    options: &PdfExportOptions,
) -> Result<Vec<u8>, genpdf::error::Error> {
    let font_family = fonts::from_files("assets/fonts", "Inter", None)?;
    let mut doc = Document::new(font_family);

    doc.set_title(format!("Setlist - {setlist_title}"));

    let mut decorator = SimplePageDecorator::new();
    decorator.set_margins(15);
    doc.set_page_decorator(decorator);

    let labels = options.locale.labels();

    if options.show_title || options.show_total_duration {
        if options.show_title {
            let mut title_style = style::Style::new().bold();
            title_style.set_font_size(24);
            doc.push(
                elements::Paragraph::new(setlist_title)
                    .aligned(Alignment::Center)
                    .styled(title_style),
            );
        }

        if options.show_total_duration {
            let duration_text = if total_duration_secs > 0 {
                let minutes = total_duration_secs / 60;
                let seconds = total_duration_secs % 60;
                format!("{}: {minutes}m {seconds}s", labels.estimated_duration)
            } else {
                format!("{}: {}", labels.estimated_duration, labels.not_calculated)
            };

            let mut duration_style = style::Style::new();
            duration_style.set_font_size(14);
            doc.push(
                elements::Paragraph::new(duration_text)
                    .aligned(Alignment::Center)
                    .styled(duration_style),
            );
        }

        doc.push(elements::Break::new(2));
    }

    let mut title_style = style::Style::new().bold();
    title_style.set_font_size(18);

    let mut meta_style = style::Style::new();
    meta_style.set_font_size(14);
    meta_style.set_color(style::Color::Rgb(80, 80, 80));

    let mut block_style = style::Style::new().bold();
    block_style.set_font_size(16);
    block_style.set_color(style::Color::Rgb(30, 30, 30));

    let mut break_style = style::Style::new().italic();
    break_style.set_font_size(13);
    break_style.set_color(style::Color::Rgb(120, 120, 120));

    let mut song_number = 0usize;

    for item in items {
        match item {
            SetlistItem::Song { song, .. } => {
                song_number += 1;
                let text_title = format!("{song_number}. {}", song.title);
                doc.push(elements::Paragraph::new(text_title).styled(title_style));

                let mut meta_parts = Vec::new();

                if options.show_key
                    && let Some(key) = &song.tonality
                {
                    meta_parts.push(format!("{}: {}", labels.key, format_tonality(key)));
                }

                if options.show_bpm
                    && let Some(bpm) = song.tempo
                {
                    meta_parts.push(format!("{bpm} BPM"));
                }

                if !meta_parts.is_empty() {
                    let text_meta = meta_parts.join("  •  ");
                    doc.push(elements::Paragraph::new(text_meta).styled(meta_style));
                }

                doc.push(elements::Break::new(1));
            }
            SetlistItem::Block { name, .. } => {
                if !options.show_blocks {
                    continue;
                }

                doc.push(elements::Break::new(1));
                doc.push(elements::Paragraph::new(name.clone()).styled(block_style));
                doc.push(elements::Break::new(1));
            }
            SetlistItem::Break {
                label,
                duration_minutes,
                ..
            } => {
                if !options.show_breaks {
                    continue;
                }

                let text = match (label, duration_minutes) {
                    (Some(label), Some(minutes)) => format!("{label} ({minutes} min)"),
                    (Some(label), None) => label.clone(),
                    (None, Some(minutes)) => format!("{} ({minutes} min)", labels.break_label),
                    (None, None) => labels.break_label.to_string(),
                };

                doc.push(elements::Paragraph::new(text).styled(break_style));
                doc.push(elements::Break::new(1));
            }
        }
    }

    let mut buffer = Cursor::new(Vec::new());
    doc.render(&mut buffer)?;

    Ok(buffer.into_inner())
}
