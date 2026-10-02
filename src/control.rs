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

pub fn toggle() -> Result<()> {
    let socket = UnixDatagram::unbound()?;
    socket
        .connect(socket_path()?)
        .context("Hydra STT is not running; start the daemon first")?;
    socket
        .send(b"toggle")
        .context("could not send toggle to Hydra STT")?;
    Ok(())
}

pub struct Listener {
    path: PathBuf,
}
impl Drop for Listener {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub fn start(tx: UnboundedSender<()>) -> Result<Listener> {
    let path = socket_path()?;
    let socket = match UnixDatagram::bind(&path) {
        Ok(socket) => socket,
        Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
            let probe = UnixDatagram::unbound()?;
            match probe.connect(&path) {
                Ok(()) => bail!("another Hydra STT daemon is already running"),
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
            match socket.recv(&mut buffer) {
                Ok(size) if &buffer[..size] == b"toggle" => {
                    log!("INFO", "compositor_toggle", "source=unix_socket");
                    if tx.send(()).is_err() {
                        break;
                    }
                }
                Ok(_) => {}
                Err(error) => {
                    log!("ERROR", "control_listener_failed", "{error}");
                    break;
                }
            }
        }
    });
    Ok(listener)
}
