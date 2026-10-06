use anyhow::{Context, Result};
use std::{
    fs::File,
    io::Write,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

static LOG: OnceLock<Mutex<File>> = OnceLock::new();

pub fn init(directory: &Path) -> Result<PathBuf> {
    crate::private::create_dir(directory)
        .with_context(|| format!("could not create log directory {}", directory.display()))?;
    let path = directory.join("hydra.log");
    let file = crate::private::options()
        .create(true)
        .append(true)
        .open(&path)?;
    crate::private::restrict(&path)?;
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
                    eprintln!("could not write Hydra STT log: {error}");
                }
            }
            Err(error) => eprintln!("could not lock Hydra STT log: {error}"),
        }
    }
}
