//! Running the daemon in the background: starting and stopping it from the
//! window, and starting it at login, through a systemd user service on Linux
//! and the Run registry key on Windows.

use crate::{config, control};
use anyhow::{Context, Result, bail};
use std::{
    env, fs,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub use platform::{autostart_enabled, can_autostart, set_autostart};

pub fn is_running() -> bool {
    control::is_running()
}

/// Starts the daemon: through systemd when it manages Hydra, otherwise as
/// `hydra-stt --daemon` detached from this process, so it keeps running after
/// the window closes.
pub fn start() -> Result<()> {
    if platform::start_managed()? {
        return Ok(());
    }
    let mut command = Command::new(env::current_exe()?);
    command
        .arg("--daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    #[cfg(windows)]
    {
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        std::os::windows::process::CommandExt::creation_flags(
            &mut command,
            CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW,
        );
    }
    let mut child = command
        .spawn()
        .context("could not start hydra-stt --daemon")?;
    // Reap it if it exits while the window is still open.
    thread::spawn(move || child.wait());
    Ok(())
}

/// Stops the daemon and waits until it has exited. A daemon started outside
/// systemd is asked to quit over the control channel.
pub fn stop() -> Result<()> {
    platform::stop_managed()?;
    if is_running() {
        control::quit()?;
    }
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

#[cfg(target_os = "linux")]
mod platform {
    use anyhow::{Context, Result, anyhow};
    use std::{env, fs, path::PathBuf, thread};
    use zbus_systemd::{
        systemd1::{ManagerProxy, UnitProxy},
        zbus,
    };

    const UNIT: &str = "hydra-stt.service";

    /// Whether the systemd user service runs the daemon. The window then
    /// starts and stops it through systemd too, so there is only ever one.
    fn managed() -> bool {
        autostart_enabled()
    }

    /// Starts the service if systemd manages Hydra. Returns whether it did.
    pub fn start_managed() -> Result<bool> {
        if !managed() {
            return Ok(false);
        }
        systemd(async |manager| {
            // Clears a failed state left by earlier start attempts.
            let _ = manager.reset_failed_unit(UNIT.to_owned()).await;
            manager
                .start_unit(UNIT.to_owned(), "replace".to_owned())
                .await?;
            Ok(())
        })?;
        Ok(true)
    }

    /// Stops the service if systemd manages Hydra and it is running.
    pub fn stop_managed() -> Result<()> {
        if !managed() {
            return Ok(());
        }
        systemd(async |manager| {
            // GetUnit fails when the unit is not loaded, so not running.
            let Ok(path) = manager.get_unit(UNIT.to_owned()).await else {
                return Ok(());
            };
            let unit = UnitProxy::builder(manager.inner().connection())
                .path(path)?
                .build()
                .await?;
            if unit.active_state().await? != "inactive" {
                manager
                    .stop_unit(UNIT.to_owned(), "replace".to_owned())
                    .await?;
            }
            Ok(())
        })
    }

    /// Runs `call` against systemd's user manager over D-Bus. It runs on its own
    /// thread and runtime, since callers may be on a thread that already runs one.
    fn systemd<T, F>(call: F) -> Result<T>
    where
        T: Send + 'static,
        F: AsyncFnOnce(&ManagerProxy<'static>) -> zbus::Result<T> + Send + 'static,
    {
        thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(anyhow::Error::from)?
                .block_on(async move {
                    let connection = zbus::Connection::session().await?;
                    let manager = ManagerProxy::new(&connection).await?;
                    anyhow::Ok(call(&manager).await?)
                })
        })
        .join()
        .map_err(|_| anyhow!("the systemd call panicked"))?
        .context("could not talk to the systemd user manager")
    }

    /// Whether a systemd user manager is available to start Hydra at login.
    pub fn can_autostart() -> bool {
        systemd(async |manager| manager.version().await).is_ok()
    }

    pub fn autostart_enabled() -> bool {
        systemd(async |manager| manager.get_unit_file_state(UNIT.to_owned()).await)
            .is_ok_and(|state| state == "enabled")
    }

    pub fn set_autostart(enabled: bool) -> Result<()> {
        if enabled {
            write_unit()?;
        }
        systemd(async move |manager| {
            if enabled {
                manager.reload().await?;
                manager
                    .enable_unit_files(vec![UNIT.to_owned()], false, true)
                    .await?;
            } else {
                manager
                    .disable_unit_files(vec![UNIT.to_owned()], false)
                    .await?;
            }
            // Like systemctl, reload so systemd sees the changed links.
            manager.reload().await
        })
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
}

#[cfg(windows)]
mod platform {
    use anyhow::{Context, Result};
    use std::env;
    use windows::{
        Win32::{
            Foundation::ERROR_FILE_NOT_FOUND,
            System::Registry::{
                HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW,
                RegSetKeyValueW,
            },
        },
        core::w,
    };

    const RUN_KEY: windows::core::PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
    const VALUE: windows::core::PCWSTR = w!("Hydra STT");

    /// Windows has no service manager to hand the daemon to.
    pub fn start_managed() -> Result<bool> {
        Ok(false)
    }

    pub fn stop_managed() -> Result<()> {
        Ok(())
    }

    pub fn can_autostart() -> bool {
        true
    }

    pub fn autostart_enabled() -> bool {
        let mut size = 0_u32;
        // SAFETY: only asks for the size of the value, if it exists.
        unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                VALUE,
                RRF_RT_REG_SZ,
                None,
                None,
                Some(&mut size),
            )
        }
        .is_ok()
    }

    /// Adds or removes `hydra-stt --daemon` under the user's Run key.
    pub fn set_autostart(enabled: bool) -> Result<()> {
        if !enabled {
            // SAFETY: both names are valid, null-terminated wide strings.
            let result = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, VALUE) };
            if result == ERROR_FILE_NOT_FOUND {
                return Ok(());
            }
            return result.ok().context("could not turn off start at login");
        }
        let command = format!("\"{}\" --daemon", env::current_exe()?.display());
        let data: Vec<u16> = command.encode_utf16().chain([0]).collect();
        // SAFETY: `data` is a null-terminated wide string of the given size.
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                VALUE,
                REG_SZ.0,
                Some(data.as_ptr().cast()),
                (data.len() * 2) as u32,
            )
        }
        .ok()
        .context("could not turn on start at login")
    }
}
