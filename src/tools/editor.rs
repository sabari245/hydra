//! File viewing and editing: view / create / str_replace / insert.
//!
//! Ported from the `editor` tool in Anthropic's computer-use reference
//! implementation (anthropics/anthropic-quickstarts,
//! computer-use-best-practices/computer_use/tools/editor.py, MIT), which
//! mirrors the hosted `text_editor` tool. Unlike the reference, paths are not
//! confined to a scratch directory: the agent works on the user's files.

use super::home;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{fs, path::PathBuf};

/// Longest file view returned at once; larger files need `view_range`.
const MAX_VIEW_CHARS: usize = 20_000;

pub fn definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "editor",
            "description": "View, create, and edit text files on the user's computer. Paths may be absolute, start with ~, or be relative to the home directory.",
            "parameters": {
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "enum": ["view", "create", "str_replace", "insert"],
                        "description": "* view: show a text file with line numbers, or list a directory.\n* create: create or overwrite a file with `file_text`.\n* str_replace: replace the single occurrence of `old_str` with `new_str` in the file. Fails if `old_str` is missing or not unique.\n* insert: insert `new_str` after line `insert_line` (0 inserts at the top)."
                    },
                    "path": { "type": "string" },
                    "view_range": {
                        "type": "array",
                        "items": { "type": "integer" },
                        "minItems": 2,
                        "maxItems": 2,
                        "description": "[start, end] 1-indexed line range; -1 for end means EOF."
                    },
                    "file_text": { "type": "string" },
                    "old_str": { "type": "string" },
                    "new_str": { "type": "string" },
                    "insert_line": { "type": "integer", "minimum": 0 }
                },
                "required": ["command", "path"]
            }
        }
    })
}

pub fn resolve(path: &str) -> PathBuf {
    let path = path.trim();
    if path == "~" {
        return home();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return home().join(rest);
    }
    home().join(path)
}

pub fn execute(arguments: &Value) -> Result<String> {
    let command = super::string(arguments, "command")?;
    let path = super::string(arguments, "path")?;
    let target = resolve(path);
    let shown = target.display().to_string();
    match command {
        "view" => view(&target, &shown, arguments.get("view_range")),
        "create" => {
            let text = arguments
                .get("file_text")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("could not create {}", parent.display()))?;
            }
            fs::write(&target, text).with_context(|| format!("could not write {shown}"))?;
            Ok(format!("wrote {} chars to {shown}", text.chars().count()))
        }
        "str_replace" => {
            let (Some(old), Some(new)) = (
                arguments.get("old_str").and_then(Value::as_str),
                arguments.get("new_str").and_then(Value::as_str),
            ) else {
                bail!("str_replace requires `old_str` and `new_str`");
            };
            let text = read_text(&target, &shown)?;
            match text.matches(old).count() {
                0 => bail!("`old_str` not found in {shown}"),
                1 => {}
                count => bail!("`old_str` matched {count} times in {shown}; must be unique"),
            }
            fs::write(&target, text.replacen(old, new, 1))
                .with_context(|| format!("could not write {shown}"))?;
            Ok(format!("replaced 1 occurrence in {shown}"))
        }
        "insert" => {
            let (Some(line), Some(new)) = (
                arguments.get("insert_line").and_then(Value::as_u64),
                arguments.get("new_str").and_then(Value::as_str),
            ) else {
                bail!("insert requires `insert_line` and `new_str`");
            };
            let text = read_text(&target, &shown)?;
            let mut lines: Vec<String> = text.split_inclusive('\n').map(str::to_owned).collect();
            // A last line without a newline would otherwise run into the insert.
            if let Some(last) = lines.last_mut()
                && !last.ends_with('\n')
            {
                last.push('\n');
            }
            let line = (line as usize).min(lines.len());
            let mut new = new.to_owned();
            if !new.is_empty() && !new.ends_with('\n') {
                new.push('\n');
            }
            lines.insert(line, new);
            fs::write(&target, lines.concat())
                .with_context(|| format!("could not write {shown}"))?;
            Ok(format!("inserted after line {line} in {shown}"))
        }
        other => bail!("unknown command {other:?}"),
    }
}

fn read_text(target: &PathBuf, shown: &str) -> Result<String> {
    if !target.is_file() {
        bail!("{shown} is not a file");
    }
    let bytes = fs::read(target).with_context(|| format!("could not read {shown}"))?;
    String::from_utf8(bytes).with_context(|| format!("{shown} is not a UTF-8 text file"))
}

fn view(target: &PathBuf, shown: &str, view_range: Option<&Value>) -> Result<String> {
    if !target.exists() {
        bail!("{shown} does not exist");
    }
    if target.is_dir() {
        let mut entries: Vec<String> = fs::read_dir(target)
            .with_context(|| format!("could not list {shown}"))?
            .filter_map(Result::ok)
            .map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                if entry.path().is_dir() {
                    format!("{name}/")
                } else {
                    name
                }
            })
            .collect();
        entries.sort();
        let body = if entries.is_empty() {
            "(empty)".to_owned()
        } else {
            entries.join("\n")
        };
        return Ok(format!("{shown}/\n{body}"));
    }
    let bytes = fs::read(target).with_context(|| format!("could not read {shown}"))?;
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().collect();
    let (mut start, mut end) = (1, lines.len());
    if let Some(range) = view_range.and_then(Value::as_array)
        && let [first, last] = range.as_slice()
    {
        start = first.as_i64().unwrap_or(1).max(1) as usize;
        end = match last.as_i64().unwrap_or(-1) {
            -1 => lines.len(),
            last => (last.max(0) as usize).min(lines.len()),
        };
    }
    let mut body = String::new();
    for (number, line) in lines
        .iter()
        .enumerate()
        .take(end)
        .skip(start.saturating_sub(1))
    {
        if body.len() > MAX_VIEW_CHARS {
            body.push_str(&format!(
                "[... stopped at line {number}; use view_range to see more ...]\n"
            ));
            break;
        }
        body.push_str(&format!("{:6}\t{line}\n", number + 1));
    }
    Ok(format!(
        "{shown} (lines {start}-{end} of {})\n{body}",
        lines.len()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn edits_files_like_the_reference_tool() {
        let directory = env::temp_dir().join(format!("hydra-stt-editor-{}", std::process::id()));
        let file = directory.join("notes.txt");
        let path = file.to_str().unwrap();
        let run = |arguments: Value| execute(&arguments);

        run(json!({ "command": "create", "path": path, "file_text": "one\ntwo\ntwo" })).unwrap();
        assert!(
            run(
                json!({ "command": "str_replace", "path": path, "old_str": "two", "new_str": "2" })
            )
            .is_err()
        );
        run(json!({ "command": "str_replace", "path": path, "old_str": "one", "new_str": "1" }))
            .unwrap();
        run(json!({ "command": "insert", "path": path, "insert_line": 0, "new_str": "top" }))
            .unwrap();
        run(json!({ "command": "insert", "path": path, "insert_line": 99, "new_str": "end" }))
            .unwrap();
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "top\n1\ntwo\ntwo\nend\n"
        );

        let view = run(json!({ "command": "view", "path": path, "view_range": [2, 3] })).unwrap();
        assert!(view.contains("(lines 2-3 of 5)") && view.contains("     2\t1\n     3\ttwo\n"));
        assert!(
            run(json!({ "command": "view", "path": directory.to_str().unwrap() }))
                .unwrap()
                .contains("notes.txt")
        );

        fs::remove_dir_all(directory).unwrap();
    }
}
