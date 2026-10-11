//! Text layers: what the user typed, where and when, in one of a few preset styles.
//! Styles are data; [`render`] draws them to bitmaps (ADR 0017: Scout draws the text,
//! so preview and export show the same pixels).

use serde::{Deserialize, Serialize};

pub mod render;

/// Longest text of one layer, in characters.
pub const MAX_TEXT_CHARS: usize = 200;
/// Most lines of one layer.
pub const MAX_TEXT_LINES: usize = 3;
/// The style used when a project names an unknown one.
pub const DEFAULT_PRESET: &str = "bold";

/// Where a layer sits on the frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Position {
    Top,
    #[default]
    Middle,
    Bottom,
}

/// One text layer. Times are output time (after cuts and speed).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TextLayer {
    pub id: u32,
    /// A [`TextStyle::id`].
    pub preset: String,
    pub text: String,
    pub position: Position,
    pub start_ms: u64,
    pub end_ms: u64,
}

impl Default for TextLayer {
    fn default() -> Self {
        TextLayer {
            id: 0,
            preset: DEFAULT_PRESET.to_string(),
            text: String::new(),
            position: Position::Middle,
            start_ms: 0,
            end_ms: 0,
        }
    }
}

/// A preset style. Sizes are fractions of the output height, colours RGBA.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct TextStyle {
    pub id: &'static str,
    pub name: &'static str,
    /// Font size as a fraction of the frame height.
    pub size: f32,
    /// Font weight (CSS scale, 400 regular, 700 bold, 800 extra bold).
    pub weight: u16,
    pub fill: [u8; 4],
    /// Outline colour and width (fraction of the font size).
    pub outline: Option<([u8; 4], f32)>,
    /// Background box colour (a rounded "pill" behind each line).
    pub background: Option<[u8; 4]>,
    /// Drop shadow colour and offset (fraction of the font size, down and right).
    pub shadow: Option<([u8; 4], f32)>,
    /// Where the style usually goes (the layer can override it).
    pub position: Position,
    pub uppercase: bool,
}

/// The built-in styles, in menu order.
pub const PRESETS: &[TextStyle] = &[
    TextStyle {
        id: "bold",
        name: "Bold",
        size: 0.09,
        weight: 800,
        fill: [255, 255, 255, 255],
        outline: Some(([0, 0, 0, 255], 0.12)),
        background: None,
        shadow: Some(([0, 0, 0, 160], 0.06)),
        position: Position::Middle,
        uppercase: false,
    },
    TextStyle {
        id: "pill",
        name: "Pill",
        size: 0.06,
        weight: 700,
        fill: [255, 255, 255, 255],
        outline: None,
        background: Some([0, 0, 0, 178]),
        shadow: None,
        position: Position::Bottom,
        uppercase: false,
    },
    TextStyle {
        id: "caption",
        name: "Caption",
        size: 0.055,
        weight: 800,
        fill: [255, 214, 10, 255],
        outline: Some(([0, 0, 0, 255], 0.14)),
        background: None,
        shadow: Some(([0, 0, 0, 160], 0.06)),
        position: Position::Bottom,
        uppercase: true,
    },
    TextStyle {
        id: "title",
        name: "Title",
        size: 0.12,
        weight: 800,
        fill: [255, 255, 255, 255],
        outline: None,
        background: Some([20, 20, 24, 200]),
        shadow: None,
        position: Position::Top,
        uppercase: true,
    },
];

/// A style by id.
pub fn preset(id: &str) -> Option<&'static TextStyle> {
    PRESETS.iter().find(|s| s.id == id)
}

/// What is kept of typed text: line breaks (at most [`MAX_TEXT_LINES`] lines), no other
/// control characters, tabs as spaces, trimmed, at most [`MAX_TEXT_CHARS`] characters.
pub fn clean_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len().min(MAX_TEXT_CHARS.saturating_mul(4)));
    let mut chars: usize = 0;
    let mut lines: usize = 1;
    let mut prev_cr = false;
    for ch in s.chars() {
        if chars >= MAX_TEXT_CHARS {
            break;
        }
        let ch = match ch {
            '\n' if prev_cr => {
                prev_cr = false;
                continue;
            }
            '\r' | '\n' => {
                prev_cr = ch == '\r';
                if lines >= MAX_TEXT_LINES {
                    ' '
                } else {
                    lines = lines.saturating_add(1);
                    '\n'
                }
            }
            '\t' => {
                prev_cr = false;
                ' '
            }
            c if c.is_control() => {
                prev_cr = false;
                continue;
            }
            c => {
                prev_cr = false;
                c
            }
        };
        out.push(ch);
        chars = chars.saturating_add(1);
    }
    let trimmed: String = out.lines().map(str::trim).collect::<Vec<_>>().join("\n");
    trimmed.trim_matches('\n').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_are_unique_and_findable() {
        for (i, s) in PRESETS.iter().enumerate() {
            assert_eq!(preset(s.id), Some(s));
            assert!(PRESETS.iter().skip(i + 1).all(|o| o.id != s.id));
            assert!(s.size > 0.0 && s.size < 0.5);
        }
        assert!(preset(DEFAULT_PRESET).is_some());
        assert!(preset("nope").is_none());
    }

    #[test]
    fn clean_text_rules() {
        assert_eq!(clean_text("  hello \t world  "), "hello   world");
        assert_eq!(clean_text("a\r\nb\rc\nd"), "a\nb\nc d");
        assert_eq!(clean_text("x\u{0}y\u{7}z"), "xyz");
        assert_eq!(clean_text("\n\n  \n"), "");
        let long = "é".repeat(500);
        assert_eq!(clean_text(&long).chars().count(), MAX_TEXT_CHARS);
        assert_eq!(clean_text("🎮 gg\nwp"), "🎮 gg\nwp");
    }
}
