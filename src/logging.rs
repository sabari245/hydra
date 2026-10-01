use anyhow::{Context, Result};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::PathBuf,
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

static LOG: OnceLock<Mutex<File>> = OnceLock::new();

pub fn init() -> Result<PathBuf> {
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("logs");
    fs::create_dir_all(&directory).context("could not create log directory")?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
    let path = directory.join("hydra.log");
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    LOG.set(Mutex::new(file))
        .map_err(|_| anyhow::anyhow!("logger already initialized"))?;
    Ok(path)
}

pub fn event(level: &str, name: &str, detail: std::fmt::Arguments<'_>) {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let line = format!(
        "timestamp_ms={timestamp} level={level} event={name} detail={:?}\n",
        detail.to_string()
    );
    eprint!("{line}");
    if let Some(log) = LOG.get() {
        match log.lock() {
            Ok(mut file) => {
                if let Err(error) = file.write_all(line.as_bytes()).and_then(|_| file.flush()) {
                    eprintln!("could not write Hydra log: {error}");
                }
            }
            Err(error) => eprintln!("could not lock Hydra log: {error}"),
        }
    }
}
