//! Keyboard shortcuts such as "Ctrl+Alt+Space", for the global shortcut the
//! daemon registers on Windows. On Linux the compositor binds the shortcut.

use anyhow::{Result, bail};

/// Modifier flags, with the values RegisterHotKey uses.
pub const ALT: u32 = 0x1;
pub const CONTROL: u32 = 0x2;
pub const SHIFT: u32 = 0x4;
pub const WIN: u32 = 0x8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shortcut {
    pub modifiers: u32,
    /// A Windows virtual-key code.
    pub key: u32,
}

impl Shortcut {
    pub fn parse(text: &str) -> Result<Self> {
        let mut modifiers = 0;
        let mut key = None;
        for part in text.split('+').map(str::trim) {
            let lower = part.to_ascii_lowercase();
            let modifier = match lower.as_str() {
                "ctrl" | "control" => CONTROL,
                "alt" => ALT,
                "shift" => SHIFT,
                "win" | "super" | "meta" => WIN,
                _ => 0,
            };
            if modifier != 0 {
                modifiers |= modifier;
            } else if key.is_some() {
                bail!("the shortcut {text:?} has more than one key");
            } else {
                key = Some(key_code(&lower).ok_or_else(|| {
                    anyhow::anyhow!("unknown key {part:?} in the shortcut {text:?}")
                })?);
            }
        }
        let Some(key) = key else {
            bail!("the shortcut {text:?} needs a key, such as Ctrl+Alt+Space");
        };
        if modifiers == 0 && !(0x70..=0x87).contains(&key) {
            bail!("the shortcut {text:?} needs Ctrl, Alt, Shift or Win, unless it is F1-F24");
        }
        Ok(Self { modifiers, key })
    }
}

fn key_code(name: &str) -> Option<u32> {
    let code = match name {
        "space" => 0x20,
        "enter" | "return" => 0x0D,
        "tab" => 0x09,
        "esc" | "escape" => 0x1B,
        "backspace" => 0x08,
        "insert" => 0x2D,
        "delete" | "del" => 0x2E,
        "home" => 0x24,
        "end" => 0x23,
        "pageup" => 0x21,
        "pagedown" => 0x22,
        "up" => 0x26,
        "down" => 0x28,
        "left" => 0x25,
        "right" => 0x27,
        "pause" => 0x13,
        "`" | "backquote" => 0xC0,
        _ => {
            if let Some(number) = name.strip_prefix('f')
                && let Ok(number @ 1..=24) = number.parse::<u32>()
            {
                return Some(0x6F + number);
            }
            let mut chars = name.chars();
            return match (chars.next(), chars.next()) {
                (Some(c @ ('a'..='z' | '0'..='9')), None) => {
                    Some(u32::from(c.to_ascii_uppercase()))
                }
                _ => None,
            };
        }
    };
    Some(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modifiers_and_keys() {
        assert_eq!(
            Shortcut::parse("Ctrl+Alt+Space").unwrap(),
            Shortcut {
                modifiers: CONTROL | ALT,
                key: 0x20
            }
        );
        assert_eq!(
            Shortcut::parse("win + shift + d").unwrap().key,
            u32::from(b'D')
        );
        assert_eq!(Shortcut::parse("F9").unwrap().key, 0x78);
    }

    #[test]
    fn rejects_unusable_shortcuts() {
        assert!(Shortcut::parse("Space").is_err());
        assert!(Shortcut::parse("Ctrl+Alt").is_err());
        assert!(Shortcut::parse("Ctrl+A+B").is_err());
        assert!(Shortcut::parse("Ctrl+Banana").is_err());
    }
}
