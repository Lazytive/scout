//! Hotkey syntax: `Ctrl+Alt+Shift+Win+Key` as stored and shown, and the shortcuts-spec
//! trigger format (`CTRL+ALT+SHIFT+LOGO+keysym`) that `klip_platform::GlobalShortcuts`
//! takes. Parsing accepts both, case-insensitively, plus common aliases.

use std::fmt;
use std::str::FromStr;

/// Longest hotkey string accepted.
pub const MAX_HOTKEY_LEN: usize = 64;

/// A key without modifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    /// `A`–`Z` (stored upper case).
    Letter(char),
    /// `0`–`9` on the main row.
    Digit(u8),
    /// `F1`–`F24`.
    F(u8),
    /// Numpad `0`–`9`.
    Numpad(u8),
    NumpadAdd,
    NumpadSubtract,
    NumpadMultiply,
    NumpadDivide,
    NumpadDecimal,
    Space,
    Tab,
    Enter,
    Escape,
    Backspace,
    Insert,
    Delete,
    Home,
    End,
    PageUp,
    PageDown,
    Up,
    Down,
    Left,
    Right,
    PrintScreen,
    Pause,
    ScrollLock,
    Grave,
    Minus,
    Equal,
    BracketLeft,
    BracketRight,
    Backslash,
    Semicolon,
    Quote,
    Comma,
    Period,
    Slash,
}

/// Named keys: (display name, shortcuts-spec keysym, extra lower-case aliases).
const NAMED: &[(Key, &str, &str, &[&str])] = &[
    (
        Key::NumpadAdd,
        "NumAdd",
        "KP_Add",
        &["numpadadd", "numplus"],
    ),
    (
        Key::NumpadSubtract,
        "NumSubtract",
        "KP_Subtract",
        &["numpadsubtract", "numminus"],
    ),
    (
        Key::NumpadMultiply,
        "NumMultiply",
        "KP_Multiply",
        &["numpadmultiply"],
    ),
    (
        Key::NumpadDivide,
        "NumDivide",
        "KP_Divide",
        &["numpaddivide"],
    ),
    (
        Key::NumpadDecimal,
        "NumDecimal",
        "KP_Decimal",
        &["numpaddecimal"],
    ),
    (Key::Space, "Space", "space", &[]),
    (Key::Tab, "Tab", "Tab", &[]),
    (Key::Enter, "Enter", "Return", &[]),
    (Key::Escape, "Esc", "Escape", &[]),
    (Key::Backspace, "Backspace", "BackSpace", &[]),
    (Key::Insert, "Insert", "Insert", &["ins"]),
    (Key::Delete, "Delete", "Delete", &["del"]),
    (Key::Home, "Home", "Home", &[]),
    (Key::End, "End", "End", &[]),
    (Key::PageUp, "PageUp", "Prior", &["pgup"]),
    (Key::PageDown, "PageDown", "Next", &["pgdn"]),
    (Key::Up, "Up", "Up", &["arrowup"]),
    (Key::Down, "Down", "Down", &["arrowdown"]),
    (Key::Left, "Left", "Left", &["arrowleft"]),
    (Key::Right, "Right", "Right", &["arrowright"]),
    (
        Key::PrintScreen,
        "PrintScreen",
        "Print",
        &["prtsc", "printscr"],
    ),
    (Key::Pause, "Pause", "Pause", &["break"]),
    (Key::ScrollLock, "ScrollLock", "Scroll_Lock", &[]),
    (Key::Grave, "`", "grave", &["backquote"]),
    (Key::Minus, "-", "minus", &[]),
    (Key::Equal, "=", "equal", &[]),
    (Key::BracketLeft, "[", "bracketleft", &[]),
    (Key::BracketRight, "]", "bracketright", &[]),
    (Key::Backslash, "\\", "backslash", &[]),
    (Key::Semicolon, ";", "semicolon", &[]),
    (Key::Quote, "'", "apostrophe", &["quote"]),
    (Key::Comma, ",", "comma", &[]),
    (Key::Period, ".", "period", &[]),
    (Key::Slash, "/", "slash", &[]),
];

impl Key {
    /// Parse one key name (display name, keysym or alias; any case).
    pub fn parse(s: &str) -> Option<Key> {
        let lower = s.to_ascii_lowercase();
        let mut chars = s.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            if c.is_ascii_alphabetic() {
                return Some(Key::Letter(c.to_ascii_uppercase()));
            }
            if let Some(d) = c.to_digit(10) {
                return u8::try_from(d).ok().map(Key::Digit);
            }
        }
        let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        if let Some(n) = lower.strip_prefix('f')
            && digits(n)
            && !n.starts_with('0')
            && let Ok(n) = n.parse::<u8>()
            && (1..=24).contains(&n)
        {
            return Some(Key::F(n));
        }
        for prefix in ["num", "numpad", "kp_"] {
            if let Some(n) = lower.strip_prefix(prefix)
                && n.len() == 1
                && digits(n)
                && let Ok(n) = n.parse::<u8>()
            {
                return Some(Key::Numpad(n));
            }
        }
        NAMED
            .iter()
            .find(|(_, name, sym, aliases)| {
                name.eq_ignore_ascii_case(s)
                    || sym.eq_ignore_ascii_case(s)
                    || aliases.contains(&lower.as_str())
            })
            .map(|(k, ..)| *k)
    }

    /// Display name (`F10`, `A`, `NumAdd`, `` ` ``).
    pub fn name(self) -> String {
        match self {
            Key::Letter(c) => c.to_string(),
            Key::Digit(d) => d.to_string(),
            Key::F(n) => format!("F{n}"),
            Key::Numpad(n) => format!("Num{n}"),
            other => named(other).map_or_else(String::new, |(name, _)| name.to_string()),
        }
    }

    /// X keysym name, as the shortcuts spec uses (`F10`, `a`, `KP_Add`, `grave`).
    pub fn keysym(self) -> String {
        match self {
            Key::Letter(c) => c.to_ascii_lowercase().to_string(),
            Key::Digit(d) => d.to_string(),
            Key::F(n) => format!("F{n}"),
            Key::Numpad(n) => format!("KP_{n}"),
            other => named(other).map_or_else(String::new, |(_, sym)| sym.to_string()),
        }
    }
}

fn named(k: Key) -> Option<(&'static str, &'static str)> {
    NAMED
        .iter()
        .find(|(key, ..)| *key == k)
        .map(|(_, name, sym, _)| (*name, *sym))
}

/// A key with modifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Hotkey {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// The Windows / Super / Meta key.
    pub logo: bool,
    pub key: Key,
}

/// Why a hotkey string was rejected.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HotkeyError {
    #[error("empty")]
    Empty,
    #[error("longer than {MAX_HOTKEY_LEN} characters")]
    TooLong,
    #[error("unknown key {0:?}")]
    UnknownKey(String),
    #[error("needs a key besides the modifiers")]
    NoKey,
    #[error("more than one key ({0:?})")]
    TwoKeys(String),
}

impl Hotkey {
    /// Whether any modifier is held (a bare key is taken from every other app).
    pub fn has_modifier(&self) -> bool {
        self.ctrl || self.alt || self.shift || self.logo
    }

    /// Shortcuts-spec trigger, e.g. `ALT+SHIFT+F10`.
    pub fn to_trigger(&self) -> String {
        let mut parts: Vec<String> = Vec::with_capacity(5);
        for (on, name) in [
            (self.ctrl, "CTRL"),
            (self.alt, "ALT"),
            (self.shift, "SHIFT"),
            (self.logo, "LOGO"),
        ] {
            if on {
                parts.push(name.into());
            }
        }
        parts.push(self.key.keysym());
        parts.join("+")
    }
}

impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (on, name) in [
            (self.ctrl, "Ctrl+"),
            (self.alt, "Alt+"),
            (self.shift, "Shift+"),
            (self.logo, "Win+"),
        ] {
            if on {
                f.write_str(name)?;
            }
        }
        f.write_str(&self.key.name())
    }
}

impl FromStr for Hotkey {
    type Err = HotkeyError;

    fn from_str(s: &str) -> Result<Hotkey, HotkeyError> {
        let s = s.trim();
        if s.is_empty() {
            return Err(HotkeyError::Empty);
        }
        if s.len() > MAX_HOTKEY_LEN {
            return Err(HotkeyError::TooLong);
        }
        let (mut ctrl, mut alt, mut shift, mut logo) = (false, false, false, false);
        let mut key: Option<Key> = None;
        // `+` separates parts; a trailing `+` would be a `+` key, which we don't support
        // (it is Shift+= or NumAdd).
        for part in s.split('+').map(str::trim) {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" | "ctl" => ctrl = true,
                "alt" | "option" | "mod1" => alt = true,
                "shift" => shift = true,
                "win" | "super" | "logo" | "meta" | "cmd" | "windows" | "mod4" => logo = true,
                _ => {
                    let k = Key::parse(part)
                        .ok_or_else(|| HotkeyError::UnknownKey(part.to_string()))?;
                    if key.is_some() {
                        return Err(HotkeyError::TwoKeys(part.to_string()));
                    }
                    key = Some(k);
                }
            }
        }
        let key = key.ok_or(HotkeyError::NoKey)?;
        Ok(Hotkey {
            ctrl,
            alt,
            shift,
            logo,
            key,
        })
    }
}

/// Every key, for tests and the key-capture UI.
pub fn all_keys() -> Vec<Key> {
    let mut v: Vec<Key> = ('A'..='Z').map(Key::Letter).collect();
    v.extend((0..=9).map(Key::Digit));
    v.extend((1..=24).map(Key::F));
    v.extend((0..=9).map(Key::Numpad));
    v.extend(NAMED.iter().map(|(k, ..)| *k));
    v
}
