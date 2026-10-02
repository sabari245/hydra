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
    Ok(PathBuf::from(directory).join(format!("{}.sock", crate::config::APP_NAME)))
}

/// A message to the running daemon.
pub enum Command {
    Toggle,
    Quit,
}

pub fn toggle() -> Result<()> {
    send(b"toggle")
}

pub fn quit() -> Result<()> {
    send(b"quit")
}

fn send(message: &[u8]) -> Result<()> {
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

pub fn start(tx: UnboundedSender<Command>) -> Result<Listener> {
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
                Ok(size) if &buffer[..size] == b"toggle" => {
                    log!("INFO", "compositor_toggle", "source=unix_socket");
                    Command::Toggle
                }
                Ok(size) if &buffer[..size] == b"quit" => {
                    log!("INFO", "quit_requested", "source=unix_socket");
                    Command::Quit
                }
                Ok(_) => continue,
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
