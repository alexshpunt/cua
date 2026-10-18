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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn enter_tab_and_space_have_explicit_key_code_and_text_semantics() {
        let enter = parse_press_key("Enter").unwrap();
        assert_eq!(
            (
                enter.key.as_str(),
                enter.code.as_str(),
                enter.windows_virtual_key_code,
                enter.text.as_deref()
            ),
            ("Enter", "Enter", 13, Some("\r"))
        );
        let tab = parse_press_key("Tab").unwrap();
        assert_eq!(
            (tab.code.as_str(), tab.windows_virtual_key_code, tab.text),
            ("Tab", 9, None)
        );
        let space = parse_press_key("Space").unwrap();
        assert_eq!(
            (
                space.key.as_str(),
                space.code.as_str(),
                space.text.as_deref()
            ),
            (" ", "Space", Some(" "))
        );
    }
    #[test]
    fn shortcuts_keep_explicit_modifier_flags_without_inserting_shortcut_text() {
        let key = parse_press_key("Control+Shift+A").unwrap();
        assert_eq!(
            (
                key.key.as_str(),
                key.code.as_str(),
                key.windows_virtual_key_code,
                key.modifiers,
                key.text
            ),
            ("A", "KeyA", 65, 10, None)
        );
        assert_eq!(parse_press_key("Meta+ArrowLeft").unwrap().modifiers, 4);
        assert_eq!(
            parse_press_key("Alt+Control+Shift+Meta+Home")
                .unwrap()
                .modifiers,
            15
        );
    }
    #[test]
    fn printable_ascii_and_shifted_letters_preserve_requested_text_without_dpr_or_layout_conversion(
    ) {
        assert_eq!(parse_press_key("a").unwrap().text.as_deref(), Some("a"));
        assert_eq!(parse_press_key("A").unwrap().text.as_deref(), Some("A"));
        assert_eq!(
            parse_press_key("Shift+a").unwrap().text.as_deref(),
            Some("A")
        );
        assert_eq!(parse_press_key("7").unwrap().code, "Digit7");
        assert_eq!(parse_press_key("Control+a").unwrap().text, None);
    }
    #[test]
    fn malformed_unknown_and_layout_dependent_keys_refuse_without_coercion_or_fallback() {
        for key in [
            "",
            "Control",
            "Control+",
            "+Enter",
            "Control+Control+A",
            "Ctrl+A",
            "Control+a+b",
            "Enter ",
            "\n",
            "é",
            "😀",
            "Shift+1",
            "!",
            "F25",
            "Control+é",
        ] {
            assert_eq!(
                parse_press_key(key),
                Err("dom_action_key_unavailable"),
                "{key:?}"
            );
        }
    }
    #[test]
    fn navigation_and_function_keys_have_bounded_explicit_virtual_key_codes() {
        for (key, code) in [
            ("Backspace", 8),
            ("Delete", 46),
            ("Escape", 27),
            ("ArrowLeft", 37),
            ("ArrowUp", 38),
            ("ArrowRight", 39),
            ("ArrowDown", 40),
            ("Home", 36),
            ("End", 35),
            ("PageUp", 33),
            ("PageDown", 34),
            ("F1", 112),
            ("F24", 135),
        ] {
            let mapped = parse_press_key(key).unwrap();
            assert_eq!(mapped.windows_virtual_key_code, code);
            assert_eq!(mapped.modifiers, 0);
            assert_eq!(mapped.text, None);
        }
    }
}
