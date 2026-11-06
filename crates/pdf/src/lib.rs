//! A message as a PDF: its headers, its text and the list of its attachments on A4 pages, with
//! the original message attached inside the PDF as `message.eml`, so the PDF carries the raw
//! bytes it was made from.
//!
//! Text is set in DejaVu Sans, which covers Latin, Greek and Cyrillic; characters it lacks show
//! as empty boxes. Lines are written left to right without bidirectional reordering.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use krilla::Document;
use krilla::embed::{AssociationKind, EmbeddedFile, MimeType};
use krilla::geom::{PathBuilder, Point, Rect};
use krilla::metadata::Metadata;
use krilla::page::PageSettings;
use krilla::paint::Fill;
use krilla::text::{Font, TextDirection};
use skrifa::instance::{LocationRef, Size};
use skrifa::{FontRef, MetadataProvider};

const PAGE_WIDTH: f32 = 595.0;
const PAGE_HEIGHT: f32 = 842.0;
const MARGIN: f32 = 56.0;
const FOOTER: f32 = 28.0;
const TITLE: f32 = 15.0;
const HEADER: f32 = 9.5;
const BODY: f32 = 10.0;
const LEADING: f32 = 1.4;
const LABEL_WIDTH: f32 = 54.0;

/// The most text laid out; the rest of a longer body is left out and said so.
pub const MAX_TEXT: usize = 2 << 20;

/// What goes into the PDF. Every field is shown as given.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MessagePdf {
    /// The subject.
    pub subject: Option<String>,
    /// Header lines shown under the subject, in order, such as `("From", "Ada <ada@example.com>")`.
    pub headers: Vec<(String, String)>,
    /// The body as plain text.
    pub text: String,
    /// Attachments as `(name, size in bytes)`.
    pub attachments: Vec<(String, u64)>,
    /// The original message, attached inside the PDF as `message.eml`.
    pub raw: Vec<u8>,
}

/// Why no PDF was made.
#[derive(Debug, thiserror::Error)]
pub enum PdfError {
    /// The PDF writer failed.
    #[error("the PDF could not be written: {0}")]
    Write(String),
}

struct Face {
    font: Font,
    data: &'static [u8],
    advances: Mutex<HashMap<char, f32>>,
}

impl Face {
    fn load(data: &'static [u8]) -> Self {
        Self {
            font: Font::new(data.into(), 0).expect("DejaVu Sans loads"),
            data,
            advances: Mutex::new(HashMap::new()),
        }
    }

    fn width(&self, text: &str, size: f32) -> f32 {
        let mut advances = self.advances.lock().expect("advances lock");
        let mut font = None;
        let mut total = 0.0;
        for c in text.chars() {
            let advance = *advances.entry(c).or_insert_with(|| {
                let font = font
                    .get_or_insert_with(|| FontRef::new(self.data).expect("DejaVu Sans parses"));
                font.charmap()
                    .map(c)
                    .and_then(|g| {
                        font.glyph_metrics(Size::new(1.0), LocationRef::default())
                            .advance_width(g)
                    })
                    .unwrap_or(0.6)
            });
            total += advance;
        }
        total * size
    }
}

fn regular() -> &'static Face {
    static FACE: OnceLock<Face> = OnceLock::new();
    FACE.get_or_init(|| Face::load(dejavu::sans::regular()))
}

fn bold() -> &'static Face {
    static FACE: OnceLock<Face> = OnceLock::new();
    FACE.get_or_init(|| Face::load(dejavu::sans::bold()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Style {
    Title,
    Label,
    Header,
    Body,
    Muted,
}

impl Style {
    fn face(self) -> &'static Face {
        match self {
            Self::Title | Self::Label => bold(),
            _ => regular(),
        }
    }

    fn size(self) -> f32 {
        match self {
            Self::Title => TITLE,
            Self::Body => BODY,
            _ => HEADER,
        }
    }

    fn gray(self) -> u8 {
        match self {
            Self::Label | Self::Muted => 100,
            _ => 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Piece {
    x: f32,
    y: f32,
    style: Style,
    text: String,
}

#[derive(Default)]
struct Layout {
    pages: Vec<Vec<Piece>>,
    rules: Vec<Vec<f32>>,
    y: f32,
}

fn clean(text: &str) -> String {
    text.replace('\t', "    ")
        .chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .collect()
}

/// Breaks `text` into lines no wider than `width`, at spaces where it can and inside words
/// where it must.
fn wrap(text: &str, style: Style, width: f32) -> Vec<String> {
    let face = style.face();
    let size = style.size();
    let space = face.width(" ", size);
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        let mut line_width = 0.0;
        for word in paragraph.split(' ') {
            let word_width = face.width(word, size);
            let gap = if line.is_empty() { 0.0 } else { space };
            if line_width + gap + word_width <= width {
                if !line.is_empty() {
                    line.push(' ');
                }
                line.push_str(word);
                line_width += gap + word_width;
                continue;
            }
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
                line_width = 0.0;
            }
            for c in word.chars() {
                let advance = face.width(c.encode_utf8(&mut [0; 4]), size);
                if line_width + advance > width && !line.is_empty() {
                    lines.push(std::mem::take(&mut line));
                    line_width = 0.0;
                }
                line.push(c);
                line_width += advance;
            }
        }
        lines.push(line.trim_end().to_owned());
    }
    lines
}

impl Layout {
    fn new() -> Self {
        Self {
            pages: vec![Vec::new()],
            rules: vec![Vec::new()],
            y: MARGIN,
        }
    }

    fn break_page(&mut self) {
        self.pages.push(Vec::new());
        self.rules.push(Vec::new());
        self.y = MARGIN;
    }

    fn line(&mut self, x: f32, style: Style, text: String) {
        let height = style.size() * LEADING;
        if self.y + height > PAGE_HEIGHT - MARGIN - FOOTER {
            self.break_page();
        }
        self.y += height;
        let y = self.y - style.size() * (LEADING - 1.0);
        self.pages
            .last_mut()
            .expect("a page")
            .push(Piece { x, y, style, text });
    }

    fn gap(&mut self, points: f32) {
        self.y += points;
    }

    fn rule(&mut self) {
        self.gap(6.0);
        let y = self.y;
        self.rules.last_mut().expect("a page").push(y);
        self.gap(10.0);
    }
}

fn size_text(bytes: u64) -> String {
    match bytes {
        b if b < 1024 => format!("{b} B"),
        b if b < 1024 * 1024 => format!("{:.1} KB", b as f64 / 1024.0),
        b => format!("{:.1} MB", b as f64 / 1024.0 / 1024.0),
    }
}

fn lay_out(message: &MessagePdf) -> Layout {
    let width = PAGE_WIDTH - 2.0 * MARGIN;
    let mut layout = Layout::new();
    let subject = clean(message.subject.as_deref().unwrap_or("(no subject)"));
    for line in wrap(&subject, Style::Title, width) {
        layout.line(MARGIN, Style::Title, line);
    }
    layout.gap(6.0);
    for (label, value) in &message.headers {
        let lines = wrap(&clean(value), Style::Header, width - LABEL_WIDTH);
        for (n, line) in lines.into_iter().enumerate() {
            if n == 0 {
                layout.line(MARGIN, Style::Label, clean(label));
                let y = layout.y;
                layout.y -= Style::Header.size() * LEADING;
                layout.line(MARGIN + LABEL_WIDTH, Style::Header, line);
                layout.y = y;
            } else {
                layout.line(MARGIN + LABEL_WIDTH, Style::Header, line);
            }
        }
    }
    layout.rule();
    let mut text = clean(&message.text);
    let cut = text.len() > MAX_TEXT;
    if cut {
        let mut end = MAX_TEXT;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    for line in wrap(text.trim_end(), Style::Body, width) {
        layout.line(MARGIN, Style::Body, line);
    }
    if cut {
        layout.gap(6.0);
        layout.line(
            MARGIN,
            Style::Muted,
            "The rest of the text is left out; the original message inside this PDF has all of it."
                .into(),
        );
    }
    if !message.attachments.is_empty() {
        layout.rule();
        layout.line(MARGIN, Style::Label, "Attachments".into());
        for (name, size) in &message.attachments {
            let entry = format!("{}  ({})", clean(name), size_text(*size));
            for line in wrap(&entry, Style::Header, width) {
                layout.line(MARGIN, Style::Header, line);
            }
        }
    }
    layout
}

fn fill(gray: u8) -> Fill {
    Fill {
        paint: krilla::color::luma::Color::new(gray).into(),
        ..Fill::default()
    }
}

/// Writes `message` as a PDF.
pub fn render(message: &MessagePdf) -> Result<Vec<u8>, PdfError> {
    let layout = lay_out(message);
    let total = layout.pages.len();
    let mut document = Document::new();
    for (index, (pieces, rules)) in layout.pages.iter().zip(&layout.rules).enumerate() {
        let settings = PageSettings::from_wh(PAGE_WIDTH, PAGE_HEIGHT).expect("A4");
        let mut page = document.start_page_with(settings);
        let mut surface = page.surface();
        for y in rules {
            let mut path = PathBuilder::new();
            path.push_rect(
                Rect::from_xywh(MARGIN, *y, PAGE_WIDTH - 2.0 * MARGIN, 0.6).expect("rule"),
            );
            if let Some(path) = path.finish() {
                surface.set_fill(Some(fill(190)));
                surface.draw_path(&path);
            }
        }
        for piece in pieces {
            surface.set_fill(Some(fill(piece.style.gray())));
            surface.draw_text(
                Point::from_xy(piece.x, piece.y),
                piece.style.face().font.clone(),
                piece.style.size(),
                &piece.text,
                false,
                TextDirection::Auto,
            );
        }
        let footer = format!("{} of {total}", index + 1);
        let footer_width = regular().width(&footer, 8.0);
        surface.set_fill(Some(fill(120)));
        surface.draw_text(
            Point::from_xy(
                PAGE_WIDTH - MARGIN - footer_width,
                PAGE_HEIGHT - MARGIN + 8.0,
            ),
            regular().font.clone(),
            8.0,
            &footer,
            false,
            TextDirection::Auto,
        );
        surface.finish();
        page.finish();
    }
    document.embed_file(EmbeddedFile {
        path: "message.eml".into(),
        mime_type: MimeType::new("message/rfc822"),
        description: Some("The original message".into()),
        association_kind: AssociationKind::Source,
        data: message.raw.clone().into(),
        modification_date: None,
        compress: Some(true),
        location: None,
    });
    document.set_metadata(
        Metadata::new()
            .title(clean(message.subject.as_deref().unwrap_or("(no subject)")))
            .creator("Holtmail".into()),
    );
    document
        .finish()
        .map_err(|error| PdfError::Write(format!("{error:?}")))
}

#[cfg(test)]
mod tests;
