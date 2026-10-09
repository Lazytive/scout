//! Shortcuts-spec triggers (`CTRL+ALT+SHIFT+LOGO+keysym`) → Windows modifier flags and
//! virtual-key codes for `RegisterHotKey`. Punctuation keys are the US-layout positions.

/// `MOD_ALT`, `MOD_CONTROL`, `MOD_SHIFT`, `MOD_WIN` (winuser.h).
pub(crate) const MOD_ALT: u32 = 0x1;
pub(crate) const MOD_CONTROL: u32 = 0x2;
pub(crate) const MOD_SHIFT: u32 = 0x4;
pub(crate) const MOD_WIN: u32 = 0x8;

/// A parsed trigger.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Trigger {
    pub mods: u32,
    pub vk: u32,
    /// Normalised spelling, e.g. `CTRL+SHIFT+F10`.
    pub canonical: String,
}

const NAMED: &[(&str, u32)] = &[
    ("KP_Add", 0x6B),
    ("KP_Subtract", 0x6D),
    ("KP_Multiply", 0x6A),
    ("KP_Divide", 0x6F),
    ("KP_Decimal", 0x6E),
    ("space", 0x20),
    ("Tab", 0x09),
    ("Return", 0x0D),
    ("Escape", 0x1B),
    ("BackSpace", 0x08),
    ("Insert", 0x2D),
    ("Delete", 0x2E),
    ("Home", 0x24),
    ("End", 0x23),
    ("Prior", 0x21),
    ("Next", 0x22),
    ("Up", 0x26),
    ("Down", 0x28),
    ("Left", 0x25),
    ("Right", 0x27),
    ("Print", 0x2C),
    ("Pause", 0x13),
    ("Scroll_Lock", 0x91),
    ("grave", 0xC0),
    ("minus", 0xBD),
    ("equal", 0xBB),
    ("bracketleft", 0xDB),
    ("bracketright", 0xDD),
    ("backslash", 0xDC),
    ("semicolon", 0xBA),
    ("apostrophe", 0xDE),
    ("comma", 0xBC),
    ("period", 0xBE),
    ("slash", 0xBF),
];

/// Virtual-key code and canonical keysym of one keysym.
fn key(sym: &str) -> Option<(u32, String)> {
    let mut chars = sym.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c.is_ascii_alphabetic() {
            return Some((
                u32::from(c.to_ascii_uppercase()),
                c.to_ascii_lowercase().to_string(),
            ));
        }
        if c.is_ascii_digit() {
            return Some((u32::from(c), c.to_string()));
        }
    }
    let digits = |s: &str| !s.is_empty() && s.len() <= 2 && s.bytes().all(|b| b.is_ascii_digit());
    if let Some(n) = sym.strip_prefix(['F', 'f'])
        && digits(n)
        && !n.starts_with('0')
        && let Ok(n) = n.parse::<u32>()
        && (1..=24).contains(&n)
    {
        // VK_F1 = 0x70 … VK_F24 = 0x87.
        return Some((0x6F_u32.checked_add(n)?, format!("F{n}")));
    }
    if let Some(n) = sym
        .get(..3)
        .filter(|p| p.eq_ignore_ascii_case("KP_"))
        .and_then(|_| sym.get(3..))
        && n.len() == 1
        && digits(n)
        && let Ok(n) = n.parse::<u32>()
    {
        // VK_NUMPAD0 = 0x60.
        return Some((0x60_u32.checked_add(n)?, format!("KP_{n}")));
    }
    NAMED
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(sym))
        .map(|(name, vk)| (*vk, (*name).to_string()))
}

/// Parse a trigger such as `ALT+SHIFT+F10` (modifiers and keysym in any case).
pub(crate) fn parse_trigger(s: &str) -> Result<Trigger, String> {
    let mut mods = 0u32;
    let mut found: Option<(u32, String)> = None;
    for part in s.split('+').map(str::trim) {
        let flag = match part.to_ascii_uppercase().as_str() {
            "CTRL" | "CONTROL" => Some(MOD_CONTROL),
            "ALT" => Some(MOD_ALT),
            "SHIFT" => Some(MOD_SHIFT),
            "LOGO" | "SUPER" | "WIN" => Some(MOD_WIN),
            _ => None,
        };
        match flag {
            Some(f) => mods |= f,
            None => {
                let k = key(part).ok_or_else(|| format!("unknown key {part:?}"))?;
                if found.is_some() {
                    return Err(format!("more than one key in {s:?}"));
                }
                found = Some(k);
            }
        }
    }
    let (vk, sym) = found.ok_or_else(|| format!("no key in {s:?}"))?;
    let mut parts: Vec<String> = Vec::with_capacity(5);
    for (flag, name) in [
        (MOD_CONTROL, "CTRL"),
        (MOD_ALT, "ALT"),
        (MOD_SHIFT, "SHIFT"),
        (MOD_WIN, "LOGO"),
    ] {
        if mods & flag != 0 {
            parts.push(name.into());
        }
    }
    parts.push(sym);
    Ok(Trigger {
        mods,
        vk,
        canonical: parts.join("+"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triggers_map_to_virtual_keys() {
        let t = parse_trigger("ALT+F10").unwrap();
        assert_eq!(
            (t.mods, t.vk, t.canonical.as_str()),
            (MOD_ALT, 0x79, "ALT+F10")
        );
        let t = parse_trigger("shift+ctrl+a").unwrap();
        assert_eq!((t.mods, t.vk), (MOD_CONTROL | MOD_SHIFT, 0x41));
        assert_eq!(t.canonical, "CTRL+SHIFT+a");
        assert_eq!(parse_trigger("LOGO+KP_7").unwrap().vk, 0x67);
        assert_eq!(parse_trigger("F24").unwrap().vk, 0x87);
        assert_eq!(parse_trigger("F1").unwrap().vk, 0x70);
        assert_eq!(parse_trigger("CTRL+grave").unwrap().vk, 0xC0);
        assert_eq!(parse_trigger("Print").unwrap().vk, 0x2C);
        assert_eq!(parse_trigger("9").unwrap().vk, 0x39);
        for bad in [
            "", "ALT", "ALT+F25", "F0", "a+b", "HYPER+a", "KP_10", "ALT++",
        ] {
            assert!(parse_trigger(bad).is_err(), "{bad}");
        }
    }
}
