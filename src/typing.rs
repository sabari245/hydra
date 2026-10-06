//! Inserting text into the focused window: with wtype on Wayland, and with
//! SendInput on Windows.

use crate::config;
use anyhow::Result;

/// A typed "\n" is a Return key press that would submit partial text, so line
/// breaks are either removed or kept for typing as Shift+Enter.
pub fn normalize_lines(text: &str, newlines: config::Newlines) -> String {
    let join_words = |line: &str| line.split_whitespace().collect::<Vec<_>>().join(" ");
    match newlines {
        config::Newlines::Space => join_words(text),
        config::Newlines::ShiftEnter => {
            let mut lines: Vec<String> = Vec::new();
            for line in text.lines().map(join_words) {
                // Keep at most one blank line between paragraphs.
                if !line.is_empty() || lines.last().is_some_and(|last| !last.is_empty()) {
                    lines.push(line);
                }
            }
            lines.join("\n").trim_end().to_owned()
        }
    }
}

/// Types a transcript. Line breaks are typed as Shift+Enter.
pub fn type_text(text: &str, press_enter: bool) -> Result<()> {
    log!(
        "INFO",
        "insertion_started",
        "backend={} characters={}",
        platform::BACKEND,
        text.chars().count()
    );
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            platform::shift_enter()?;
        }
        platform::type_line(line)?;
    }
    log!("INFO", "typing_result", "lines={}", text.lines().count());
    if !press_enter {
        return Ok(());
    }
    platform::enter()?;
    log!("INFO", "enter_result", "sent=true");
    Ok(())
}

#[cfg(target_os = "linux")]
mod platform {
    use anyhow::{Context, Result, bail};
    use std::process::Command;

    pub const BACKEND: &str = "wtype";

    pub fn shift_enter() -> Result<()> {
        wtype(
            &["-M", "shift", "-k", "Return", "-m", "shift"],
            "Shift+Enter",
        )
    }

    pub fn enter() -> Result<()> {
        wtype(&["-k", "Return"], "Enter")
    }

    /// Everything after `--` is text to wtype, so each line needs its own call.
    pub fn type_line(line: &str) -> Result<()> {
        for chunk in wtype_chunks(line) {
            wtype(&["--", chunk], "the transcript")?;
        }
        Ok(())
    }

    /// wtype sends the Nth distinct character of a call as evdev keycode N, so the
    /// 29th lands on Left Ctrl (29) and the 42nd on Left Shift (42), and those
    /// characters are swallowed as modifiers. Each call gets a fresh keymap, so
    /// splitting text into chunks of at most 28 distinct characters avoids them.
    pub(super) const WTYPE_MAX_DISTINCT: usize = 28;

    pub(super) fn wtype_chunks(text: &str) -> Vec<&str> {
        let mut chunks = Vec::new();
        let mut seen = Vec::with_capacity(WTYPE_MAX_DISTINCT);
        let mut start = 0;
        for (index, ch) in text.char_indices() {
            if !seen.contains(&ch) {
                if seen.len() == WTYPE_MAX_DISTINCT {
                    chunks.push(&text[start..index]);
                    start = index;
                    seen.clear();
                }
                seen.push(ch);
            }
        }
        if start < text.len() {
            chunks.push(&text[start..]);
        }
        chunks
    }

    fn wtype(args: &[&str], what: &str) -> Result<()> {
        let status = Command::new("wtype")
            .args(args)
            .status()
            .context("could not run wtype; install wtype for Wayland keyboard output")?;
        if !status.success() {
            bail!("wtype failed while typing {what}: {status}");
        }
        Ok(())
    }
}

#[cfg(windows)]
mod platform {
    use anyhow::{Result, bail};
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP,
        KEYEVENTF_UNICODE, SendInput, VIRTUAL_KEY, VK_RETURN, VK_SHIFT,
    };

    pub const BACKEND: &str = "sendinput";

    pub fn shift_enter() -> Result<()> {
        send(&[
            key(VK_SHIFT, false),
            key(VK_RETURN, false),
            key(VK_RETURN, true),
            key(VK_SHIFT, true),
        ])
    }

    pub fn enter() -> Result<()> {
        send(&[key(VK_RETURN, false), key(VK_RETURN, true)])
    }

    /// Types each UTF-16 unit as a Unicode key event, so any character comes
    /// out right whatever the keyboard layout.
    pub fn type_line(line: &str) -> Result<()> {
        let inputs: Vec<INPUT> = line
            .encode_utf16()
            .flat_map(|unit| [unicode(unit, false), unicode(unit, true)])
            .collect();
        // Small batches, so a long transcript does not overrun the input queue.
        for batch in inputs.chunks(64) {
            send(batch)?;
        }
        Ok(())
    }

    fn keyboard(input: KEYBDINPUT) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 { ki: input },
        }
    }

    fn key(key: VIRTUAL_KEY, up: bool) -> INPUT {
        keyboard(KEYBDINPUT {
            wVk: key,
            dwFlags: if up {
                KEYEVENTF_KEYUP
            } else {
                KEYBD_EVENT_FLAGS(0)
            },
            ..Default::default()
        })
    }

    fn unicode(unit: u16, up: bool) -> INPUT {
        let flags = if up {
            KEYEVENTF_UNICODE | KEYEVENTF_KEYUP
        } else {
            KEYEVENTF_UNICODE
        };
        keyboard(KEYBDINPUT {
            wScan: unit,
            dwFlags: flags,
            ..Default::default()
        })
    }

    fn send(inputs: &[INPUT]) -> Result<()> {
        // SAFETY: the inputs are fully initialized and the size is INPUT's.
        let sent = unsafe { SendInput(inputs, size_of::<INPUT>() as i32) };
        if sent as usize != inputs.len() {
            bail!(
                "Windows blocked typing into the focused window; it may belong to an \
                 app running as administrator"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Newlines::{ShiftEnter, Space};

    #[cfg(target_os = "linux")]
    #[test]
    fn wtype_chunks_stay_below_modifier_keycodes() {
        use super::platform::{WTYPE_MAX_DISTINCT, wtype_chunks};
        let text = "Alright man, so we are going to add something. And Isoquant, I'll paste it. Right now? Yes: 42% (ok) [x] {y} 1234567890 QWERTZUIOP";
        let chunks = wtype_chunks(text);
        assert!(chunks.len() > 1);
        assert_eq!(chunks.concat(), text);
        for chunk in chunks {
            let mut distinct: Vec<char> = chunk.chars().collect();
            distinct.sort_unstable();
            distinct.dedup();
            assert!(distinct.len() <= WTYPE_MAX_DISTINCT, "{chunk:?}");
        }
        assert!(wtype_chunks("").is_empty());
    }

    #[test]
    fn space_joins_everything_into_one_line() {
        assert_eq!(
            normalize_lines("First  line.\n\nSecond\tline.\n", Space),
            "First line. Second line."
        );
    }

    #[test]
    fn shift_enter_keeps_single_blank_lines() {
        assert_eq!(
            normalize_lines("One  two.\n\n\n\nThree.\n- four\n\n", ShiftEnter),
            "One two.\n\nThree.\n- four"
        );
    }
}
