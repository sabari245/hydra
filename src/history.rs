//! Per-profile history: what each profile was given, what it produced, how
//! the router decided, and what an agent did, kept as private JSON lines and
//! shown to the profile on later requests.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

/// History files are trimmed to this many entries once they reach twice that.
const KEEP_HISTORY: usize = 500;
const SNIPPET: usize = 600;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub time_ms: u128,
    pub input: String,
    pub output: String,
    /// The router's choice probabilities, when routing happened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<BTreeMap<String, f64>>,
    /// Tool calls made by an agent profile.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<String>,
}

impl HistoryEntry {
    pub fn new(input: &str, output: &str) -> Self {
        Self {
            time_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            input: input.to_owned(),
            output: output.to_owned(),
            route: None,
            actions: Vec::new(),
        }
    }
}

pub struct History {
    directory: PathBuf,
}

fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("could not create {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn write_private(path: &Path, contents: &[u8]) -> Result<()> {
    let temporary = path.with_extension("tmp");
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)
        .and_then(|mut file| file.write_all(contents))
        .with_context(|| format!("could not write {}", temporary.display()))?;
    fs::rename(&temporary, path).with_context(|| format!("could not replace {}", path.display()))
}

fn snippet(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.char_indices().nth(SNIPPET) {
        Some((index, _)) => format!("{}…", &flat[..index]),
        None => flat,
    }
}

impl History {
    pub fn open(data_dir: &Path) -> Result<Self> {
        let directory = data_dir.join("history");
        private_dir(data_dir)?;
        private_dir(&directory)?;
        Ok(Self { directory })
    }

    fn history_path(&self, profile: &str) -> PathBuf {
        self.directory.join(format!("{profile}.jsonl"))
    }

    pub fn recent(&self, profile: &str, count: usize) -> Vec<HistoryEntry> {
        let Ok(text) = fs::read_to_string(self.history_path(profile)) else {
            return Vec::new();
        };
        let entries: Vec<HistoryEntry> = text
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        entries[entries.len().saturating_sub(count)..].to_vec()
    }

    /// History formatted for a system prompt, or an empty string.
    pub fn prompt(&self, profile: &str, count: usize) -> String {
        let entries = self.recent(profile, count);
        if entries.is_empty() {
            return String::new();
        }
        let mut prompt = String::from(
            "\n\nRecent requests handled by you, oldest first. They are context only: \
             use them to resolve references like \"that file\" or \"do it again\" and to \
             stay consistent, but do not repeat, continue, or redo them unless the new \
             transcript asks for it.",
        );
        for (index, entry) in entries.iter().enumerate() {
            prompt.push_str(&format!(
                "\n[{}] input: {}\n    output: {}",
                index + 1,
                snippet(&entry.input),
                snippet(&entry.output)
            ));
            if !entry.actions.is_empty() {
                prompt.push_str(&format!(
                    "\n    actions: {}",
                    snippet(&entry.actions.join("; "))
                ));
            }
        }
        prompt
    }

    pub fn record(&self, profile: &str, entry: &HistoryEntry) -> Result<()> {
        let path = self.history_path(profile);
        let mut line = serde_json::to_string(entry)?;
        line.push('\n');
        OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&path)
            .and_then(|mut file| file.write_all(line.as_bytes()))
            .with_context(|| format!("could not write {}", path.display()))?;
        let text = fs::read_to_string(&path)?;
        let lines: Vec<&str> = text.lines().collect();
        if lines.len() >= KEEP_HISTORY * 2 {
            let kept = lines[lines.len() - KEEP_HISTORY..].join("\n") + "\n";
            write_private(&path, kept.as_bytes())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn history_round_trip() {
        let directory = env::temp_dir().join(format!("hydra-stt-history-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        let history = History::open(&directory).unwrap();

        for index in 0..3 {
            let mut entry = HistoryEntry::new(&format!("in {index}"), &format!("out {index}"));
            entry.actions.push(format!("bash echo {index}"));
            history.record("computer", &entry).unwrap();
        }
        let recent = history.recent("computer", 2);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[1].output, "out 2");
        assert!(history.prompt("computer", 2).contains("bash echo 2"));
        assert!(history.prompt("default", 2).is_empty());

        fs::remove_dir_all(directory).unwrap();
    }
}
