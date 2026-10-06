//! Talking to the running daemon: `--toggle` and `--stop` send it commands
//! over a Unix socket on Linux and a named pipe on Windows. On Windows the
//! daemon also registers the global shortcut itself.

pub use platform::{is_running, start};

/// A message to the running daemon.
pub enum Command {
    Toggle,
    Quit,
}

pub fn toggle() -> anyhow::Result<()> {
    platform::send(b"toggle")
}

pub fn quit() -> anyhow::Result<()> {
    platform::send(b"quit")
}

/// The command a control message asks for.
fn parse(message: &[u8], source: &str) -> Option<Command> {
    match message {
        b"toggle" => {
            log!("INFO", "compositor_toggle", "source={source}");
            Some(Command::Toggle)
        }
        b"quit" => {
            log!("INFO", "quit_requested", "source={source}");
            Some(Command::Quit)
        }
        _ => None,
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::Command;
    use crate::config;
    use anyhow::{Context, Result, bail};
    use std::{
        env, fs, io,
        os::unix::{fs::PermissionsExt, net::UnixDatagram},
        path::PathBuf,
        thread,
    };
    use tokio::sync::mpsc::UnboundedSender;

    fn socket_path() -> Result<PathBuf> {
        let directory = env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?;
        Ok(PathBuf::from(directory).join(format!("{}.sock", config::APP_NAME)))
    }

    pub(super) fn send(message: &[u8]) -> Result<()> {
        let socket = UnixDatagram::unbound()?;
        socket
            .connect(socket_path()?)
            .context("Hydra STT is not running in the background")?;
        socket
            .send(message)
            .context("could not send a command to Hydra STT")?;
        Ok(())
    }

    /// Whether a daemon is listening on the control socket.
    pub fn is_running() -> bool {
        let Ok(path) = socket_path() else {
            return false;
        };
        UnixDatagram::unbound().is_ok_and(|socket| socket.connect(path).is_ok())
    }

    pub struct Listener {
        path: PathBuf,
    }
    impl Drop for Listener {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    pub fn start(tx: UnboundedSender<Command>, _shortcut: &config::Shortcut) -> Result<Listener> {
        let path = socket_path()?;
        let socket = match UnixDatagram::bind(&path) {
            Ok(socket) => socket,
            Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
                let probe = UnixDatagram::unbound()?;
                match probe.connect(&path) {
                    Ok(()) => bail!(
                        "Hydra STT is already running in the background; \
                         open the Hydra STT window or run `hydra-stt --stop` to stop it"
                    ),
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
                        ) =>
                    {
                        if path.exists() {
                            fs::remove_file(&path)?;
                        }
                        UnixDatagram::bind(&path)?
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            Err(error) => return Err(error.into()),
        };
        let listener = Listener { path };
        fs::set_permissions(&listener.path, fs::Permissions::from_mode(0o600))?;
        thread::spawn(move || {
            let mut buffer = [0_u8; 64];
            loop {
                let command = match socket.recv(&mut buffer) {
                    Ok(size) => match super::parse(&buffer[..size], "unix_socket") {
                        Some(command) => command,
                        None => continue,
                    },
                    Err(error) => {
                        log!("ERROR", "control_listener_failed", "{error}");
                        break;
                    }
                };
                if tx.send(command).is_err() {
                    break;
                }
            }
        });
        Ok(listener)
    }
}

#[cfg(windows)]
mod platform {
    use super::Command;
    use crate::{config, shortcut::Shortcut};
    use anyhow::{Context, Result, anyhow, bail};
    use std::{
        fs::OpenOptions,
        io::{self, Write},
        sync::mpsc,
        thread,
        time::Duration,
    };
    use tokio::{
        io::AsyncReadExt,
        net::windows::named_pipe::{NamedPipeServer, ServerOptions},
        sync::mpsc::UnboundedSender,
    };
    use windows::Win32::UI::{
        Input::KeyboardAndMouse::{HOT_KEY_MODIFIERS, MOD_NOREPEAT, RegisterHotKey},
        WindowsAndMessaging::{GetMessageW, MSG, WM_HOTKEY},
    };

    const ERROR_PIPE_BUSY: i32 = 231;

    /// One pipe per user, so each user's daemon is separate.
    fn pipe_name() -> String {
        let user = std::env::var("USERNAME").unwrap_or_default();
        format!(r"\\.\pipe\{}-{user}", config::APP_NAME)
    }

    fn connect() -> io::Result<std::fs::File> {
        let mut attempts = 0;
        loop {
            match OpenOptions::new().write(true).open(pipe_name()) {
                Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY) && attempts < 50 => {
                    attempts += 1;
                    thread::sleep(Duration::from_millis(20));
                }
                result => return result,
            }
        }
    }

    pub(super) fn send(message: &[u8]) -> Result<()> {
        connect()
            .context("Hydra STT is not running in the background")?
            .write_all(message)
            .context("could not send a command to Hydra STT")
    }

    /// Whether a daemon is listening on the control pipe.
    pub fn is_running() -> bool {
        match connect() {
            Ok(_) => true,
            Err(error) => error.raw_os_error() == Some(ERROR_PIPE_BUSY),
        }
    }

    pub struct Listener;

    pub fn start(tx: UnboundedSender<Command>, shortcut: &config::Shortcut) -> Result<Listener> {
        let name = pipe_name();
        // Only one daemon may create the pipe; another gets access denied.
        let server = match ServerOptions::new().first_pipe_instance(true).create(&name) {
            Ok(server) => server,
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => bail!(
                "Hydra STT is already running in the background; \
                 open the Hydra STT window or run `hydra-stt --stop` to stop it"
            ),
            Err(error) => return Err(error).context("could not create the control pipe"),
        };
        register_shortcut(&shortcut.keys, tx.clone())?;
        tokio::spawn(listen(server, name, tx));
        Ok(Listener)
    }

    async fn listen(mut server: NamedPipeServer, name: String, tx: UnboundedSender<Command>) {
        loop {
            if let Err(error) = server.connect().await {
                log!("ERROR", "control_listener_failed", "{error}");
                break;
            }
            // A fresh instance takes the next client while this one is read.
            let mut client = match ServerOptions::new().create(&name) {
                Ok(next) => std::mem::replace(&mut server, next),
                Err(error) => {
                    log!("ERROR", "control_listener_failed", "{error}");
                    break;
                }
            };
            let mut buffer = [0_u8; 64];
            let size = client.read(&mut buffer).await.unwrap_or(0);
            let Some(command) = super::parse(&buffer[..size], "named_pipe") else {
                continue;
            };
            if tx.send(command).is_err() {
                break;
            }
        }
    }

    /// Registers the global shortcut on a thread that waits for it.
    fn register_shortcut(keys: &str, tx: UnboundedSender<Command>) -> Result<()> {
        let shortcut = Shortcut::parse(keys)?;
        let (ready_tx, ready_rx) = mpsc::channel();
        thread::spawn(move || {
            let modifiers = HOT_KEY_MODIFIERS(shortcut.modifiers) | MOD_NOREPEAT;
            // SAFETY: with no window, WM_HOTKEY is posted to this thread.
            let registered = unsafe { RegisterHotKey(None, 1, modifiers, shortcut.key) };
            let failed = registered.is_err();
            let _ = ready_tx.send(registered);
            if failed {
                return;
            }
            let mut message = MSG::default();
            // SAFETY: `message` is a valid MSG for GetMessageW to fill.
            while unsafe { GetMessageW(&mut message, None, 0, 0) }.as_bool() {
                if message.message == WM_HOTKEY {
                    log!("INFO", "compositor_toggle", "source=shortcut");
                    if tx.send(Command::Toggle).is_err() {
                        break;
                    }
                }
            }
        });
        ready_rx
            .recv()
            .map_err(|_| anyhow!("the shortcut thread exited"))?
            .with_context(|| {
                format!(
                    "could not register the shortcut {keys}; another app may use it. \
                     Choose another one in the Hydra STT window"
                )
            })
    }
}
