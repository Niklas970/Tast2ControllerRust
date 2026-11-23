//! Names for keyboard keys and mouse inputs. Keys use Windows virtual-key codes, with distinct
//! codes for left/right modifiers; mouse inputs and the numpad Enter use codes above 0xff.

pub type InputId = u16;

pub const MOUSE_LEFT: InputId = 0x101;
pub const MOUSE_RIGHT: InputId = 0x102;
pub const MOUSE_MIDDLE: InputId = 0x103;
pub const MOUSE_4: InputId = 0x104;
pub const MOUSE_5: InputId = 0x105;
pub const WHEEL_UP: InputId = 0x110;
pub const WHEEL_DOWN: InputId = 0x111;
pub const WHEEL_LEFT: InputId = 0x112;
pub const WHEEL_RIGHT: InputId = 0x113;
pub const NUMPAD_ENTER: InputId = 0x10d;

pub const VK_LSHIFT: InputId = 0xa0;
pub const VK_RSHIFT: InputId = 0xa1;
pub const VK_LCONTROL: InputId = 0xa2;
pub const VK_RCONTROL: InputId = 0xa3;
pub const VK_LMENU: InputId = 0xa4;
pub const VK_RMENU: InputId = 0xa5;

const NAMED: &[(&str, InputId)] = &[
    ("Space", 0x20),
    ("Enter", 0x0d),
    ("Return", 0x0d),
    ("NumpadEnter", NUMPAD_ENTER),
    ("Escape", 0x1b),
    ("Esc", 0x1b),
    ("Tab", 0x09),
    ("Backspace", 0x08),
    ("CapsLock", 0x14),
    ("LShift", VK_LSHIFT),
    ("RShift", VK_RSHIFT),
    ("LCtrl", VK_LCONTROL),
    ("RCtrl", VK_RCONTROL),
    ("LAlt", VK_LMENU),
    ("RAlt", VK_RMENU),
    ("LWin", 0x5b),
    ("RWin", 0x5c),
    ("Menu", 0x5d),
    ("Up", 0x26),
    ("Down", 0x28),
    ("Left", 0x25),
    ("Right", 0x27),
    ("Insert", 0x2d),
    ("Delete", 0x2e),
    ("Home", 0x24),
    ("End", 0x23),
    ("PageUp", 0x21),
    ("PageDown", 0x22),
    ("PrintScreen", 0x2c),
    ("ScrollLock", 0x91),
    ("Pause", 0x13),
    ("NumLock", 0x90),
    ("NumpadMultiply", 0x6a),
    ("NumpadAdd", 0x6b),
    ("NumpadSubtract", 0x6d),
    ("NumpadDecimal", 0x6e),
    ("NumpadDivide", 0x6f),
    ("Minus", 0xbd),
    ("Equals", 0xbb),
    ("LBracket", 0xdb),
    ("RBracket", 0xdd),
    ("Backslash", 0xdc),
    ("Semicolon", 0xba),
    ("Quote", 0xde),
    ("Comma", 0xbc),
    ("Period", 0xbe),
    ("Slash", 0xbf),
    ("Grave", 0xc0),
    ("MouseLeft", MOUSE_LEFT),
    ("Mouse1", MOUSE_LEFT),
    ("MouseRight", MOUSE_RIGHT),
    ("Mouse2", MOUSE_RIGHT),
    ("MouseMiddle", MOUSE_MIDDLE),
    ("Mouse3", MOUSE_MIDDLE),
    ("Mouse4", MOUSE_4),
    ("Mouse5", MOUSE_5),
    ("WheelUp", WHEEL_UP),
    ("WheelDown", WHEEL_DOWN),
    ("WheelLeft", WHEEL_LEFT),
    ("WheelRight", WHEEL_RIGHT),
    // aliases (egui key names and other spellings), after the canonical names
    ("OpenBracket", 0xdb),
    ("CloseBracket", 0xdd),
    ("Backtick", 0xc0),
    ("ArrowUp", 0x26),
    ("ArrowDown", 0x28),
    ("ArrowLeft", 0x25),
    ("ArrowRight", 0x27),
];

pub fn parse(name: &str) -> Option<InputId> {
    let n = name.trim();
    if n.len() == 1 {
        let c = n.chars().next()?.to_ascii_uppercase();
        if c.is_ascii_alphanumeric() {
            return Some(c as InputId);
        }
    }
    let lower = n.to_ascii_lowercase();
    if let Some(num) = lower.strip_prefix('f').and_then(|r| r.parse::<u16>().ok()) {
        if (1..=24).contains(&num) {
            return Some(0x70 + num - 1);
        }
    }
    if let Some(num) = lower
        .strip_prefix("numpad")
        .and_then(|r| r.parse::<u16>().ok())
    {
        if num <= 9 {
            return Some(0x60 + num);
        }
    }
    NAMED
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(n))
        .map(|&(_, v)| v)
}

pub fn name(id: InputId) -> String {
    match id {
        0x30..=0x39 | 0x41..=0x5a => (id as u8 as char).to_string(),
        0x70..=0x87 => format!("F{}", id - 0x70 + 1),
        0x60..=0x69 => format!("Numpad{}", id - 0x60),
        _ => NAMED
            .iter()
            .find(|&&(_, v)| v == id)
            .map(|(k, _)| k.to_string())
            .unwrap_or_else(|| format!("Key{id:#x}")),
    }
}

/// Canonical names of every known input, for pickers.
pub fn all_names() -> Vec<String> {
    let mut ids: Vec<InputId> = Vec::new();
    ids.extend(0x41..=0x5a);
    ids.extend(0x30..=0x39);
    ids.extend(0x70..=0x87);
    ids.extend(0x60..=0x69);
    for &(_, id) in NAMED {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids.into_iter().map(name).collect()
}

/// Normalizes a user-typed input name; None if unknown.
pub fn canonical(n: &str) -> Option<String> {
    parse(n).map(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_roundtrip() {
        for n in [
            "W",
            "7",
            "F12",
            "Numpad5",
            "LShift",
            "MouseLeft",
            "WheelDown",
            "Space",
        ] {
            let id = parse(n).unwrap();
            assert_eq!(parse(&name(id)), Some(id), "{n}");
        }
        assert_eq!(parse("w"), parse("W"));
        assert_eq!(parse("mouse1"), Some(MOUSE_LEFT));
        assert_eq!(parse("nonsense"), None);
    }
}
