//! Tools for the computer-control agent. Each one is ported from, or shaped
//! after, a published reference implementation; see the module docs.

mod bash;
mod computer;
mod editor;
pub mod memory;

use crate::{config, screenshot::Shot};
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::{env, path::PathBuf, time::Duration};

pub enum ToolResult {
    Text(String),
    /// A description of the image, and the image.
    Image(String, Shot),
}

pub fn home() -> PathBuf {
    env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

fn string<'a>(arguments: &'a Value, name: &str) -> Result<&'a str> {
    arguments
        .get(name)
        .and_then(Value::as_str)
        .with_context(|| format!("missing string argument {name:?}"))
}

/// The tools of one agent run, with the state they keep between calls.
pub struct Toolbox<'a> {
    computer: computer::Computer,
    memory: Option<&'a memory::Memory>,
    settings: &'a config::Computer,
}

impl<'a> Toolbox<'a> {
    pub fn new(settings: &'a config::Computer, memory: Option<&'a memory::Memory>) -> Self {
        Self {
            computer: computer::Computer::new(settings.screenshot_max_size),
            memory,
            settings,
        }
    }

    pub fn definitions(&self) -> Vec<Value> {
        let mut definitions = vec![
            computer::definition(),
            bash::definition(self.settings.command_timeout_secs),
            editor::definition(),
        ];
        if self.memory.is_some() {
            definitions.push(memory::definition());
        }
        definitions
    }

    pub async fn execute(&mut self, name: &str, arguments: &Value) -> Result<ToolResult> {
        match name {
            "computer" => self.computer.execute(arguments).await,
            "bash" => bash::execute(
                string(arguments, "command")?,
                Duration::from_secs(self.settings.command_timeout_secs.max(1)),
                self.settings.allow_privileged,
            )
            .await
            .map(ToolResult::Text),
            "editor" => editor::execute(arguments).map(ToolResult::Text),
            "memory" => self
                .memory
                .context("memory is unavailable")?
                .execute(arguments)
                .map(ToolResult::Text),
            _ => bail!("unknown tool {name:?}"),
        }
    }
}
