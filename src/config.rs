use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::{
    env, fs,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

pub const APP_NAME: &str = "hydra-stt";
const TEMPLATE: &str = include_str!("../config.example.toml");

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub groq: Groq,
    pub cleanup: Cleanup,
    pub hotkey: Hotkey,
    pub recording: Recording,
    pub sounds: Sounds,
    pub media: Media,
    pub output: Output,
    pub logging: Logging,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Groq {
    pub api_key: String,
    pub model: String,
}

impl Default for Groq {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            model: "whisper-large-v3-turbo".to_owned(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Cleanup {
    pub enabled: bool,
    pub api_key: String,
    pub api_url: String,
    pub model: String,
    pub timeout_secs: u64,
    pub prompt: String,
}

impl Default for Cleanup {
    fn default() -> Self {
        Self {
            enabled: true,
            api_key: String::new(),
            api_url: "https://api.isoquant.ai/v1/chat/completions".to_owned(),
            model: "glm-5.3-flash".to_owned(),
            timeout_secs: 30,
            prompt: String::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Hotkey {
    pub binding: String,
}

impl Default for Hotkey {
    fn default() -> Self {
        Self {
            binding: "Super+Space".to_owned(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Recording {
    pub device: String,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Sounds {
    pub enabled: bool,
    pub volume: u8,
    pub press: Option<PathBuf>,
    pub release: Option<PathBuf>,
}

impl Default for Sounds {
    fn default() -> Self {
        Self {
            enabled: true,
            volume: 90,
            press: None,
            release: None,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Media {
    pub pause_while_recording: bool,
}

impl Default for Media {
    fn default() -> Self {
        Self {
            pause_while_recording: true,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Output {
    pub press_enter: bool,
    pub newlines: Newlines,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Newlines {
    /// Join everything into one line; nothing can trigger a submit.
    #[default]
    Space,
    /// Keep line breaks and type each one as Shift+Enter.
    ShiftEnter,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Logging {
    pub dir: Option<PathBuf>,
}

fn home() -> Result<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")
}

fn xdg_dir(variable: &str, fallback: &str) -> Result<PathBuf> {
    match env::var_os(variable) {
        Some(directory) if Path::new(&directory).is_absolute() => Ok(PathBuf::from(directory)),
        _ => Ok(home()?.join(fallback)),
    }
}

pub fn path() -> Result<PathBuf> {
    if let Some(path) = env::var_os("HYDRA_STT_CONFIG") {
        return Ok(PathBuf::from(path));
    }
    Ok(xdg_dir("XDG_CONFIG_HOME", ".config")?
        .join(APP_NAME)
        .join("config.toml"))
}

/// Writes the commented default configuration if none exists yet.
pub fn ensure_exists(path: &Path) -> Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    if let Some(directory) = path.parent() {
        fs::create_dir_all(directory)
            .with_context(|| format!("could not create {}", directory.display()))?;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
    }
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .and_then(|mut file| file.write_all(TEMPLATE.as_bytes()))
        .with_context(|| format!("could not write {}", path.display()))?;
    Ok(true)
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("could not read {}", path.display()))?;
        let mut config: Self =
            toml::from_str(&text).with_context(|| format!("invalid config {}", path.display()))?;
        if config.sounds.volume > 100 {
            bail!("sounds.volume must be between 0 and 100");
        }
        if let Ok(key) = env::var("GROQ_API_KEY") {
            config.groq.api_key = key;
        }
        if let Ok(model) = env::var("GROQ_MODEL") {
            config.groq.model = model;
        }
        if let Ok(key) = env::var("ISO_QUANT_API_KEY") {
            config.cleanup.api_key = key;
        }
        Ok(config)
    }

    pub fn groq_api_key(&self, path: &Path) -> Result<&str> {
        let key = self.groq.api_key.trim();
        if key.is_empty() {
            bail!(
                "no Groq API key; set groq.api_key in {} or export GROQ_API_KEY",
                path.display()
            );
        }
        Ok(key)
    }

    pub fn log_dir(&self) -> Result<PathBuf> {
        if let Some(directory) = &self.logging.dir {
            return Ok(directory.clone());
        }
        if cfg!(debug_assertions) {
            return Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("logs"));
        }
        Ok(xdg_dir("XDG_STATE_HOME", ".local/state")?.join(APP_NAME))
    }
}

/// Returns true when the file can be read by users other than its owner.
pub fn is_shared(path: &Path) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.permissions().mode() & 0o077 != 0)
        .unwrap_or(false)
}
