//! Shell commands, shaped after the `bash` tool in Anthropic's computer-use
//! reference implementation (anthropics/anthropic-quickstarts,
//! computer-use-best-practices/computer_use/tools/shell.py, MIT): one
//! `command` string, stdout and stderr merged, a wall-clock timeout, and
//! capped output. Unlike the reference there is no sandbox, since the agent
//! exists to operate the user's desktop; privilege escalation is refused.

use super::home;
use anyhow::{Context, Result, bail};
use nix::{
    sys::signal::{self, Signal},
    unistd::Pid,
};
use serde_json::{Value, json};
use std::{
    env,
    fs::{self, OpenOptions},
    os::unix::fs::OpenOptionsExt,
    process::Stdio,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
use tokio::process::Command;

const PRIVILEGED: &[&str] = &["sudo", "su", "pkexec", "doas", "run0"];
const MAX_OUTPUT_BYTES: usize = 10_000;

pub fn definition(timeout_secs: u64) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "bash",
            "description": format!("Run a bash script as the user, in the home directory, with no terminal and no stdin. stdout and stderr are merged. {timeout_secs}s timeout; the whole process group is killed on timeout. Programs it starts in the background keep running when the script exits."),
            "parameters": {
                "type": "object",
                "properties": { "command": { "type": "string" } },
                "required": ["command"]
            }
        }
    })
}

/// Rejects commands that invoke a privilege escalation tool. A guard rail
/// against accidents, not a security boundary.
fn privileged(command: &str) -> Option<&'static str> {
    command
        .split(|c: char| c.is_whitespace() || ";|&()`$'\"".contains(c))
        .filter_map(|word| word.rsplit('/').next())
        .find_map(|word| PRIVILEGED.iter().copied().find(|tool| *tool == word))
}

/// Kills a command's whole process group when dropped, so timeouts and
/// cancellation leave nothing behind.
struct ProcessGroup(Option<i32>);

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        if let Some(group) = self.0 {
            let _ = signal::killpg(Pid::from_raw(group), Signal::SIGKILL);
        }
    }
}

pub async fn execute(command: &str, timeout: Duration, allow_privileged: bool) -> Result<String> {
    if !allow_privileged && let Some(tool) = privileged(command) {
        bail!("refused: {tool} is not allowed (computer.allow_privileged is false)");
    }
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let log_path = env::temp_dir().join(format!(
        "hydra-stt-command-{}-{}.log",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    // Output goes to a file instead of a pipe, so programs started in the
    // background cannot hold the command open by keeping the pipe.
    let output = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&log_path)
        .context("could not create the command output file")?;
    let started = Instant::now();
    let mut child = Command::new("bash")
        .arg("-c")
        .arg(command)
        .current_dir(home())
        .stdin(Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(output)
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .context("could not start bash")?;
    let mut group = ProcessGroup(child.id().map(|id| id as i32));
    let status = tokio::time::timeout(timeout, child.wait()).await;
    let bytes = fs::read(&log_path).unwrap_or_default();
    let _ = fs::remove_file(&log_path);
    let mut text = String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_OUTPUT_BYTES)])
        .trim_end()
        .to_owned();
    if bytes.len() > MAX_OUTPUT_BYTES {
        text.push_str(&format!("\n[output truncated at {MAX_OUTPUT_BYTES} bytes]"));
    }
    log!(
        "INFO",
        "agent_command",
        "finished={} duration_ms={} command={command:?}",
        status.is_ok(),
        started.elapsed().as_millis()
    );
    let status = match status {
        Ok(status) => {
            // Finished: leave anything it started in the background running.
            group.0 = None;
            status.context("could not wait for the command")?
        }
        Err(_) => bail!("timed out after {}s\n{text}", timeout.as_secs()),
    };
    if !status.success() {
        bail!(
            "{}",
            if text.is_empty() {
                status.to_string()
            } else {
                format!("{status}\n{text}")
            }
        );
    }
    Ok(if text.is_empty() {
        "(no output)".to_owned()
    } else {
        text
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn privileged_commands_are_detected() {
        assert_eq!(privileged("sudo pacman -Syu"), Some("sudo"));
        assert_eq!(privileged("ls; /usr/bin/pkexec true"), Some("pkexec"));
        assert_eq!(privileged("echo hi && (doas ls)"), Some("doas"));
        assert_eq!(privileged("echo sudoku | grep su-"), None);
        assert_eq!(privileged("ls ~/subdir"), None);
    }

    #[tokio::test]
    async fn commands_report_output_and_time_out() {
        let second = Duration::from_secs(1);
        assert_eq!(execute("echo hello", second, false).await.unwrap(), "hello");
        assert_eq!(execute("true", second, false).await.unwrap(), "(no output)");
        let error = execute("echo bad; exit 3", second, false)
            .await
            .unwrap_err();
        assert!(format!("{error}").contains("exit status: 3\nbad"));
        let error = execute("sleep 10", second, false).await.unwrap_err();
        assert!(format!("{error}").contains("timed out"));
        assert!(execute("sudo true", second, false).await.is_err());
    }
}
