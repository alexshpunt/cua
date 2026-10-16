//! Closed keyboard plans for explicit DOM press requests. No layout guessing or input dispatch.

/// One CDP key pair. Modifiers are event flags, never permission for a cross-call hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DomPressKey {
    pub(crate) key: String,
    pub(crate) code: String,
    pub(crate) windows_virtual_key_code: u32,
    pub(crate) modifiers: u8,
    pub(crate) text: Option<String>,
}

/// Parse canonical named keys or ASCII letters/digits with optional explicit modifiers.
/// Unsupported text/layouts refuse; they are never silently replaced by insertText.
pub(crate) fn parse_press_key(value: &str) -> Result<DomPressKey, &'static str> {
    const UNAVAILABLE: &str = "dom_action_key_unavailable";
    if value.is_empty() || value.len() > 64 {
        return Err(UNAVAILABLE);
    }
    let mut parts = value.split('+').peekable();
    let mut modifiers = 0u8;
    let mut final_key = "";
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            final_key = part;
            break;
        }
        let bit = match part {
            "Alt" => 1,
            "Control" => 2,
            "Meta" => 4,
            "Shift" => 8,
            _ => return Err(UNAVAILABLE),
        };
        if modifiers & bit != 0 {
            return Err(UNAVAILABLE);
        }
        modifiers |= bit;
    }
    let (key, code, virtual_key, mut text) = match final_key {
        "Enter" => ("Enter".into(), "Enter".into(), 13, Some("\r".into())),
        "Space" => (" ".into(), "Space".into(), 32, Some(" ".into())),
        "Tab" => ("Tab".into(), "Tab".into(), 9, None),
        "Backspace" => ("Backspace".into(), "Backspace".into(), 8, None),
        "Escape" => ("Escape".into(), "Escape".into(), 27, None),
        "Delete" => ("Delete".into(), "Delete".into(), 46, None),
        "Home" => ("Home".into(), "Home".into(), 36, None),
        "End" => ("End".into(), "End".into(), 35, None),
        "PageUp" => ("PageUp".into(), "PageUp".into(), 33, None),
        "PageDown" => ("PageDown".into(), "PageDown".into(), 34, None),
        "ArrowLeft" => ("ArrowLeft".into(), "ArrowLeft".into(), 37, None),
        "ArrowUp" => ("ArrowUp".into(), "ArrowUp".into(), 38, None),
        "ArrowRight" => ("ArrowRight".into(), "ArrowRight".into(), 39, None),
        "ArrowDown" => ("ArrowDown".into(), "ArrowDown".into(), 40, None),
        key if key.len() == 1 && key.as_bytes()[0].is_ascii_alphabetic() => {
            let byte = key.as_bytes()[0];
            let character = if modifiers & 8 != 0 {
                byte.to_ascii_uppercase()
            } else if modifiers & 7 != 0 {
                byte.to_ascii_lowercase()
            } else {
                byte
            } as char;
            let key = character.to_string();
            (
                key.clone(),
                format!("Key{}", byte.to_ascii_uppercase() as char),
                u32::from(byte.to_ascii_uppercase()),
                Some(key),
            )
        }
        key if key.len() == 1 && key.as_bytes()[0].is_ascii_digit() && modifiers & 8 == 0 => (
            key.into(),
            format!("Digit{key}"),
            u32::from(key.as_bytes()[0]),
            Some(key.into()),
        ),
        key if key.starts_with('F') => {
            let number = key[1..]
                .parse::<u32>()
                .ok()
                .filter(|n| (1..=24).contains(n) && key == format!("F{n}"))
                .ok_or(UNAVAILABLE)?;
            (key.into(), key.into(), 111 + number, None)
        }
        _ => return Err(UNAVAILABLE),
    };
    // Shortcut flags never also insert their printable character into the page.
    if modifiers & 7 != 0 {
        text = None;
    }
    Ok(DomPressKey {
        key,
        code,
        windows_virtual_key_code: virtual_key,
        modifiers,
        text,
    })
}
