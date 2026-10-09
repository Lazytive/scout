//! Rules for names people type: clip file names (rename) and tags.

/// Longest clip name (without `.mp4`), in characters.
pub const MAX_NAME_CHARS: usize = 120;
/// Most tags on one clip.
pub const MAX_TAGS: usize = 32;
/// Longest tag, in characters.
pub const MAX_TAG_CHARS: usize = 40;

/// A clip name (file name without `.mp4`) that is safe on Windows and Linux, or why not.
/// Leading and trailing spaces are dropped; nothing else is changed silently.
pub fn check_clip_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    let name = name
        .strip_suffix(".mp4")
        .or_else(|| name.strip_suffix(".MP4"))
        .unwrap_or(name)
        .trim();
    if name.is_empty() {
        return Err("the name is empty".into());
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(format!(
            "the name is longer than {MAX_NAME_CHARS} characters"
        ));
    }
    if let Some(c) = name.chars().find(|c| {
        c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
    }) {
        return Err(if c.is_control() {
            "the name has a control character".into()
        } else {
            format!("a name can't contain {c}")
        });
    }
    if name.starts_with('.') {
        return Err("a name can't start with a dot".into());
    }
    if name.ends_with('.') {
        return Err("a name can't end with a dot".into());
    }
    let upper = name.to_ascii_uppercase();
    let stem = upper.split('.').next().unwrap_or_default().trim_end();
    let reserved = matches!(stem, "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.chars().count() == 4
            && stem.chars().last().is_some_and(|c| c.is_ascii_digit()));
    if reserved {
        return Err(format!("{name} is a reserved name on Windows"));
    }
    Ok(name.to_string())
}

/// One tag as stored: whitespace collapsed, control characters dropped, at most
/// [`MAX_TAG_CHARS`] characters. `None` if nothing is left.
pub fn clean_tag(tag: &str) -> Option<String> {
    let t: String = tag
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_TAG_CHARS)
        .collect();
    let t = t.trim().to_string();
    (!t.is_empty()).then_some(t)
}

/// Tags as typed → tags as stored: cleaned, duplicates (ignoring case) dropped, order
/// kept. Errors when there are too many or one is too long.
pub fn normalize_tags(tags: &[String]) -> Result<Vec<String>, String> {
    if tags.len() > MAX_TAGS.saturating_mul(4) {
        return Err(format!("a clip can have at most {MAX_TAGS} tags"));
    }
    if let Some(t) = tags
        .iter()
        .find(|t| t.trim().chars().count() > MAX_TAG_CHARS)
    {
        let shown: String = t.trim().chars().take(20).collect();
        return Err(format!(
            "tag \"{shown}…\" is longer than {MAX_TAG_CHARS} characters"
        ));
    }
    let out = lenient_tags(tags.iter().map(String::as_str), usize::MAX);
    if out.len() > MAX_TAGS {
        return Err(format!("a clip can have at most {MAX_TAGS} tags"));
    }
    Ok(out)
}

/// Tags from a file that may be hostile: cleaned, deduplicated, cut to `max` (and at
/// most `8 * max` inputs looked at).
pub fn lenient_tags<'a>(tags: impl IntoIterator<Item = &'a str>, max: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in tags.into_iter().take(max.saturating_mul(8)) {
        if out.len() >= max {
            break;
        }
        if let Some(t) = clean_tag(t)
            && !out.iter().any(|o| o.to_lowercase() == t.to_lowercase())
        {
            out.push(t);
        }
    }
    out
}
