//! Inserting text into the focused window with wtype.

use crate::config;
use anyhow::{Context, Result, bail};
use std::process::Command;

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

/// Types a transcript with wtype. Line breaks are typed as Shift+Enter.
pub fn type_text(text: &str, press_enter: bool) -> Result<()> {
    log!(
        "INFO",
        "insertion_started",
        "backend=wtype characters={}",
        text.chars().count()
    );
    // Everything after `--` is text to wtype, so each line needs its own call.
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            wtype(
                &["-M", "shift", "-k", "Return", "-m", "shift"],
                "Shift+Enter",
            )?;
        }
        for chunk in wtype_chunks(line) {
            wtype(&["--", chunk], "the transcript")?;
        }
    }
    log!("INFO", "typing_result", "lines={}", text.lines().count());
    if !press_enter {
        return Ok(());
    }

    wtype(&["-k", "Return"], "Enter")?;
    log!("INFO", "enter_result", "sent=true");
    Ok(())
}

/// Types text for the computer agent. As in Anthropic's reference computer-use
/// tool, line breaks press Return and tabs press Tab, so forms can be filled
/// field by field.
pub fn type_keys(text: &str) -> Result<()> {
    for piece in text.split_inclusive(['\n', '\t']) {
        let (line, key) = match piece.chars().last() {
            Some('\n') => (&piece[..piece.len() - 1], Some("Return")),
            Some('\t') => (&piece[..piece.len() - 1], Some("Tab")),
            _ => (piece, None),
        };
        for chunk in wtype_chunks(line.trim_end_matches('\r')) {
            wtype(&["--", chunk], "text")?;
        }
        if let Some(key) = key {
            wtype(&["-k", key], key)?;
        }
    }
    Ok(())
}

/// wtype sends the Nth distinct character of a call as evdev keycode N, so the
/// 29th lands on Left Ctrl (29) and the 42nd on Left Shift (42), and those
/// characters are swallowed as modifiers. Each call gets a fresh keymap, so
/// splitting text into chunks of at most 28 distinct characters avoids them.
const WTYPE_MAX_DISTINCT: usize = 28;

fn wtype_chunks(text: &str) -> Vec<&str> {
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

pub fn wtype(args: &[&str], what: &str) -> Result<()> {
    let status = Command::new("wtype")
        .args(args)
        .status()
        .context("could not run wtype; install wtype for Wayland keyboard output")?;
    if !status.success() {
        bail!("wtype failed while typing {what}: {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Newlines::{ShiftEnter, Space};

    #[test]
    fn wtype_chunks_stay_below_modifier_keycodes() {
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
