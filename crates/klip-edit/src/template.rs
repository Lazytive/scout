//! Templates: a few edits applied in one step.

use crate::format::{Format, FormatKind};
use crate::project::Project;
use crate::text::{self, Position, TextLayer};
use crate::timeline;

/// How long the template's title shows, ms.
const TITLE_MS: u64 = 3000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Template {
    /// Vertical 9:16, centred, with the clip's name as a caption for the first seconds.
    Shorts,
}

impl Template {
    pub const ALL: [Template; 1] = [Template::Shorts];

    pub fn id(self) -> &'static str {
        match self {
            Template::Shorts => "shorts",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Template::Shorts => "Shorts",
        }
    }

    pub fn parse(id: &str) -> Option<Template> {
        Template::ALL.into_iter().find(|t| t.id() == id)
    }
}

/// Apply a template to a project. `title` is shown by templates that add a caption
/// (usually the clip's name); an empty title adds none.
pub fn apply(p: &mut Project, template: Template, title: &str) {
    match template {
        Template::Shorts => {
            p.format = Format {
                kind: FormatKind::Tall,
                crop_x: 0.5,
            };
            let title = text::clean_text(title);
            if title.is_empty() {
                return;
            }
            let duration = timeline::output_duration_ms(p);
            let end_ms = if duration == 0 {
                TITLE_MS
            } else {
                TITLE_MS.min(duration)
            };
            // One template caption at a time: replace the one from the last apply.
            p.text
                .retain(|l| !(l.preset == "caption" && l.start_ms == 0 && l.text == title));
            if p.text.len() < crate::project::MAX_TEXT_LAYERS {
                p.text.push(TextLayer {
                    id: p.next_text_id(),
                    preset: "caption".to_string(),
                    text: title,
                    position: Position::Bottom,
                    start_ms: 0,
                    end_ms,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shorts_sets_tall_and_a_caption() {
        let mut p = Project::new(2000);
        apply(&mut p, Template::Shorts, "  My clip ");
        assert_eq!(p.format.kind, FormatKind::Tall);
        assert_eq!(p.text.len(), 1);
        assert_eq!(p.text[0].text, "My clip");
        assert_eq!(p.text[0].end_ms, 2000, "no longer than the edit");
        apply(&mut p, Template::Shorts, "My clip");
        assert_eq!(p.text.len(), 1, "applying again doesn't stack captions");
        apply(&mut p, Template::Shorts, "");
        assert_eq!(p.text.len(), 1);
        assert_eq!(Template::parse("shorts"), Some(Template::Shorts));
        assert_eq!(Template::parse("x"), None);
        let mut unknown = Project::new(0);
        apply(&mut unknown, Template::Shorts, "t");
        assert_eq!(unknown.text[0].end_ms, TITLE_MS);
    }
}
