//! Long-term memory as a directory of small text files under /memories.
//!
//! Ported from Anthropic's client-side handler for the `memory` tool
//! (anthropics/claude-cookbooks, tool_use/memory_tool.py, MIT): the same
//! commands, parameters, path rules, and messages. The files live in the data
//! directory, and their contents are added to every profile's system prompt
//! so dictation cleanup also benefits from them.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Component, Path, PathBuf},
};

const ROOT: &str = "/memories";
const TEXT_EXTENSIONS: &[&str] = &[".txt", ".md", ".json", ".py", ".yaml", ".yml"];
/// Memory beyond this is listed by name only in system prompts.
const MAX_PROMPT_CHARS: usize = 8000;

pub struct Memory {
    root: PathBuf,
}

pub fn definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "memory",
            "description": "Long-term memory that persists across requests: a /memories directory of small text files. Use it to keep facts, names, spellings, preferences, and things the user often says or asks for. Its current contents are shown in the system prompt.",
            "parameters": {
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "enum": ["view", "create", "str_replace", "insert", "delete", "rename"],
                        "description": "* view: show a directory listing or a file with line numbers (optional `view_range`).\n* create: create or overwrite a file with `file_text` (.txt, .md, .json, .py, .yaml, .yml).\n* str_replace: replace the unique occurrence of `old_str` with `new_str`.\n* insert: insert `insert_text` at line `insert_line`.\n* delete: delete a file or directory.\n* rename: move `old_path` to `new_path`."
                    },
                    "path": { "type": "string", "description": "A path starting with /memories, such as /memories/people.md." },
                    "view_range": { "type": "array", "items": { "type": "integer" }, "minItems": 2, "maxItems": 2 },
                    "file_text": { "type": "string" },
                    "old_str": { "type": "string" },
                    "new_str": { "type": "string" },
                    "insert_line": { "type": "integer", "minimum": 0 },
                    "insert_text": { "type": "string" },
                    "old_path": { "type": "string" },
                    "new_path": { "type": "string" }
                },
                "required": ["command"]
            }
        }
    })
}

fn get<'a>(arguments: &'a Value, name: &str) -> Option<&'a str> {
    arguments.get(name).and_then(Value::as_str)
}

impl Memory {
    pub fn open(data_dir: &Path) -> Result<Self> {
        let root = data_dir.join("memories");
        fs::create_dir_all(&root)
            .with_context(|| format!("could not create {}", root.display()))?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        Ok(Self { root })
    }

    /// Maps a /memories path inside the memory directory, rejecting traversal.
    fn validate(&self, path: &str) -> Result<PathBuf> {
        let Some(relative) = path.strip_prefix(ROOT) else {
            bail!(
                "Path must start with /memories, got: {path}. All memory operations must be \
                 confined to the /memories directory."
            );
        };
        if !relative.is_empty() && !relative.starts_with('/') {
            bail!("Path must start with /memories, got: {path}.");
        }
        let relative = Path::new(relative.trim_start_matches('/'));
        if relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
        {
            bail!(
                "Path '{path}' would escape /memories directory. Directory traversal attempts \
                 are not allowed."
            );
        }
        Ok(self.root.join(relative))
    }

    pub fn execute(&self, arguments: &Value) -> Result<String> {
        let command = get(arguments, "command").unwrap_or_default();
        match command {
            "view" => self.view(arguments),
            "create" => self.create(arguments),
            "str_replace" => self.str_replace(arguments),
            "insert" => self.insert(arguments),
            "delete" => self.delete(arguments),
            "rename" => self.rename(arguments),
            _ => bail!(
                "Unknown command: '{command}'. Valid commands are: view, create, str_replace, \
                 insert, delete, rename"
            ),
        }
    }

    fn path<'a>(&self, arguments: &'a Value) -> Result<(&'a str, PathBuf)> {
        let path = get(arguments, "path").context("Missing required parameter: path")?;
        Ok((path, self.validate(path)?))
    }

    fn view(&self, arguments: &Value) -> Result<String> {
        let (path, full) = self.path(arguments)?;
        if full.is_dir() {
            let mut items: Vec<String> = fs::read_dir(&full)
                .with_context(|| format!("Cannot read directory {path}"))?
                .filter_map(Result::ok)
                .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
                .map(|entry| {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if entry.path().is_dir() {
                        format!("{name}/")
                    } else {
                        name
                    }
                })
                .collect();
            items.sort();
            if items.is_empty() {
                return Ok(format!("Directory: {path}\n(empty)"));
            }
            let listing: Vec<String> = items.iter().map(|item| format!("- {item}")).collect();
            return Ok(format!("Directory: {path}\n{}", listing.join("\n")));
        }
        if !full.is_file() {
            bail!("Path not found: {path}");
        }
        let content = fs::read_to_string(&full)
            .with_context(|| format!("Cannot read {path}: File is not valid UTF-8 text"))?;
        let lines: Vec<&str> = content.lines().collect();
        let (mut start, mut end) = (0, lines.len());
        if let Some(range) = arguments.get("view_range").and_then(Value::as_array)
            && let [first, last] = range.as_slice()
        {
            start = (first.as_i64().unwrap_or(1).max(1) - 1) as usize;
            end = match last.as_i64().unwrap_or(-1) {
                -1 => lines.len(),
                last => (last.max(0) as usize).min(lines.len()),
            };
        }
        Ok(lines
            .iter()
            .enumerate()
            .take(end)
            .skip(start)
            .map(|(index, line)| format!("{:4}: {line}", index + 1))
            .collect::<Vec<_>>()
            .join("\n"))
    }

    fn create(&self, arguments: &Value) -> Result<String> {
        let (path, full) = self.path(arguments)?;
        if !TEXT_EXTENSIONS
            .iter()
            .any(|extension| path.ends_with(extension))
        {
            bail!(
                "Cannot create {path}: Only text files are supported. Use file extensions: .txt, \
                 .md, .json, .py, .yaml, .yml"
            );
        }
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&full, get(arguments, "file_text").unwrap_or_default())
            .with_context(|| format!("Cannot create file {path}"))?;
        Ok(format!("File created successfully at {path}"))
    }

    fn str_replace(&self, arguments: &Value) -> Result<String> {
        let (path, full) = self.path(arguments)?;
        let old =
            get(arguments, "old_str").context("Missing required parameters: path, old_str")?;
        let new = get(arguments, "new_str").unwrap_or_default();
        if !full.is_file() {
            bail!("File not found: {path}");
        }
        let content = fs::read_to_string(&full)?;
        match content.matches(old).count() {
            0 => bail!("String not found in {path}. The exact text must exist in the file."),
            1 => {}
            count => bail!(
                "String appears {count} times in {path}. The string must be unique. Use more \
                 specific context."
            ),
        }
        fs::write(&full, content.replacen(old, new, 1))
            .with_context(|| format!("Cannot edit file {path}"))?;
        Ok(format!("File {path} has been edited successfully"))
    }

    fn insert(&self, arguments: &Value) -> Result<String> {
        let (path, full) = self.path(arguments)?;
        let line = arguments
            .get("insert_line")
            .and_then(Value::as_u64)
            .context("Missing required parameters: path, insert_line")? as usize;
        let text = get(arguments, "insert_text").unwrap_or_default();
        if !full.is_file() {
            bail!("File not found: {path}");
        }
        let content = fs::read_to_string(&full)?;
        let mut lines: Vec<&str> = content.lines().collect();
        if line > lines.len() {
            bail!(
                "Invalid insert_line {line}. Must be between 0 and {}",
                lines.len()
            );
        }
        lines.insert(line, text.trim_end_matches('\n'));
        fs::write(&full, lines.join("\n") + "\n")
            .with_context(|| format!("Cannot insert into {path}"))?;
        Ok(format!("Text inserted at line {line} in {path}"))
    }

    fn delete(&self, arguments: &Value) -> Result<String> {
        let (path, full) = self.path(arguments)?;
        if full == self.root {
            bail!("Cannot delete the /memories directory itself");
        }
        if full.is_file() {
            fs::remove_file(&full).with_context(|| format!("Cannot delete {path}"))?;
            Ok(format!("File deleted: {path}"))
        } else if full.is_dir() {
            fs::remove_dir_all(&full).with_context(|| format!("Cannot delete {path}"))?;
            Ok(format!("Directory deleted: {path}"))
        } else {
            bail!("Path not found: {path}")
        }
    }

    fn rename(&self, arguments: &Value) -> Result<String> {
        let (Some(old_path), Some(new_path)) =
            (get(arguments, "old_path"), get(arguments, "new_path"))
        else {
            bail!("Missing required parameters: old_path, new_path");
        };
        let (old, new) = (self.validate(old_path)?, self.validate(new_path)?);
        if !old.exists() {
            bail!("Source path not found: {old_path}");
        }
        if new.exists() {
            bail!(
                "Destination already exists: {new_path}. Cannot overwrite existing \
                 files/directories."
            );
        }
        if let Some(parent) = new.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(&old, &new)
            .with_context(|| format!("Cannot rename {old_path} to {new_path}"))?;
        Ok(format!("Renamed {old_path} to {new_path}"))
    }

    /// Every memory file and its contents, for a system prompt, or an empty
    /// string when memory is empty.
    pub fn prompt(&self) -> String {
        let mut files = Vec::new();
        collect(&self.root, &mut files);
        files.sort();
        if files.is_empty() {
            return String::new();
        }
        let mut prompt = String::from(
            "\n\nThe user's saved memory (facts, names, spellings, and preferences; use them \
             where relevant):",
        );
        for file in files {
            let name = format!(
                "{ROOT}/{}",
                file.strip_prefix(&self.root).unwrap_or(&file).display()
            );
            let content = fs::read_to_string(&file).unwrap_or_default();
            if prompt.len() + content.len() > MAX_PROMPT_CHARS {
                prompt.push_str(&format!("\n--- {name} (not shown, too long) ---"));
            } else {
                prompt.push_str(&format!("\n--- {name} ---\n{}", content.trim_end()));
            }
        }
        prompt
    }
}

fn collect(directory: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        if path.is_dir() {
            collect(&path, files);
        } else {
            files.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn behaves_like_the_reference_handler() {
        let directory = env::temp_dir().join(format!("hydra-stt-memory-{}", std::process::id()));
        let memory = Memory::open(&directory).unwrap();
        let run = |arguments: Value| memory.execute(&arguments);

        assert!(run(json!({ "command": "view", "path": "/etc/passwd" })).is_err());
        assert!(run(json!({ "command": "view", "path": "/memories/../secret" })).is_err());
        assert!(run(json!({ "command": "create", "path": "/memories/a.sh" })).is_err());
        run(json!({ "command": "create", "path": "/memories/people.md", "file_text": "Manager: Priya\n" }))
            .unwrap();
        run(json!({ "command": "insert", "path": "/memories/people.md", "insert_line": 1, "insert_text": "Team: Hydra" }))
            .unwrap();
        run(json!({ "command": "str_replace", "path": "/memories/people.md", "old_str": "Priya", "new_str": "Priya R" }))
            .unwrap();
        assert_eq!(
            run(json!({ "command": "view", "path": "/memories/people.md" })).unwrap(),
            "   1: Manager: Priya R\n   2: Team: Hydra"
        );
        assert!(
            run(json!({ "command": "view", "path": "/memories" }))
                .unwrap()
                .contains("- people.md")
        );
        assert!(
            memory
                .prompt()
                .contains("--- /memories/people.md ---\nManager: Priya R")
        );
        run(json!({ "command": "rename", "old_path": "/memories/people.md", "new_path": "/memories/work/people.md" }))
            .unwrap();
        assert!(run(json!({ "command": "delete", "path": "/memories" })).is_err());
        run(json!({ "command": "delete", "path": "/memories/work" })).unwrap();
        assert!(memory.prompt().is_empty());

        fs::remove_dir_all(directory).unwrap();
    }
}
