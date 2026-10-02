//! Running the daemon in the background: starting and stopping it from the
//! window, and starting it at login through a systemd user service.

use crate::{config, control};
use anyhow::{Context, Result, bail};
use std::{
    env, fs,
    os::unix::process::CommandExt,
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const UNIT: &str = "hydra-stt.service";

pub fn is_running() -> bool {
    control::is_running()
}

/// Starts `hydra-stt --daemon` detached from this process, so it keeps
/// running after the window closes.
pub fn start() -> Result<()> {
    let mut child = Command::new(env::current_exe()?)
        .arg("--daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .context("could not start hydra-stt --daemon")?;
    // Reap it if it exits while the window is still open.
    thread::spawn(move || child.wait());
    Ok(())
}

/// Asks the daemon to exit and waits until it has.
pub fn stop() -> Result<()> {
    control::quit()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while is_running() {
        if Instant::now() > deadline {
            bail!("Hydra STT did not stop within 5 seconds");
        }
        thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

pub fn restart() -> Result<()> {
    if is_running() {
        stop()?;
    }
    start()
}

/// The command that toggles recording, for the compositor key binding.
pub fn toggle_command() -> String {
    let program = env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| config::APP_NAME.to_owned());
    format!("{program} --toggle")
}

fn systemctl(arguments: &[&str]) -> Result<String> {
    let output = Command::new("systemctl")
        .arg("--user")
        .args(arguments)
        .stdin(Stdio::null())
        .output()
        .context("could not run systemctl")?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "systemctl --user {}: {}",
            arguments.join(" "),
            stderr.trim()
        );
    }
    Ok(stdout)
}

/// Whether a systemd user manager is available to start Hydra at login.
pub fn can_autostart() -> bool {
    systemctl(&["show-environment"]).is_ok()
}

pub fn autostart_enabled() -> bool {
    // is-enabled exits non-zero for "disabled", so read stdout either way.
    Command::new("systemctl")
        .args(["--user", "is-enabled", UNIT])
        .stderr(Stdio::null())
        .output()
        .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).trim() == "enabled")
}

pub fn set_autostart(enabled: bool) -> Result<()> {
    if enabled {
        write_unit()?;
        systemctl(&["daemon-reload"])?;
        systemctl(&["enable", UNIT])?;
    } else {
        systemctl(&["disable", UNIT])?;
    }
    Ok(())
}

fn unit_path() -> Result<PathBuf> {
    let base = match env::var_os("XDG_CONFIG_HOME") {
        Some(directory) if !directory.is_empty() => PathBuf::from(directory),
        _ => PathBuf::from(env::var_os("HOME").context("HOME is not set")?).join(".config"),
    };
    Ok(base.join("systemd/user").join(UNIT))
}

/// Writes the user unit, pointing at this executable. Rewritten each time
/// autostart is turned on, so it follows the binary if it moves.
fn write_unit() -> Result<()> {
    let path = unit_path()?;
    if let Some(directory) = path.parent() {
        fs::create_dir_all(directory)?;
    }
    let unit = unit_text(&env::current_exe()?.display().to_string());
    fs::write(&path, unit).with_context(|| format!("could not write {}", path.display()))
}

fn unit_text(program: &str) -> String {
    let program = program.replace('%', "%%");
    format!(
        "# Written by hydra-stt. Turn it on or off in the Hydra STT window.\n\
         [Unit]\n\
         Description=Hydra STT dictation\n\
         PartOf=graphical-session.target\n\
         After=graphical-session.target\n\
         StartLimitIntervalSec=60\n\
         StartLimitBurst=3\n\
         \n\
         [Service]\n\
         ExecStart=\"{program}\" --daemon\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         \n\
         [Install]\n\
         WantedBy=graphical-session.target\n"
    )
}

/// The last error the daemon logged at or after `since_ms` (Unix time).
pub fn last_error(log_dir: &std::path::Path, since_ms: u128) -> Option<String> {
    let log = fs::read_to_string(log_dir.join("hydra.log")).ok()?;
    log.lines().rev().find_map(|line| {
        let timestamp: u128 = line
            .strip_prefix("timestamp_ms=")?
            .split(' ')
            .next()?
            .parse()
            .ok()?;
        if timestamp < since_ms || !line.contains(" level=ERROR ") {
            return None;
        }
        let detail = line.split_once(" detail=")?.1;
        Some(detail.trim_matches('"').replace("\\\"", "\""))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_quotes_and_escapes_the_program() {
        let unit = unit_text("/opt/my apps/100%/hydra-stt");
        assert!(unit.contains("ExecStart=\"/opt/my apps/100%%/hydra-stt\" --daemon\n"));
        assert!(unit.contains("WantedBy=graphical-session.target"));
    }
}
