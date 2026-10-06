use anyhow::{Context, Result, bail};
use serde::{Deserialize, de::IgnoredAny};
use std::{
    collections::BTreeMap,
    env, fs,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

pub const APP_NAME: &str = "hydra-stt";
const TEMPLATE: &str = include_str!("../config.example.toml");

/// Groq's Whisper models.
pub const WHISPER_STANDARD: &str = "whisper-large-v3";
pub const WHISPER_TURBO: &str = "whisper-large-v3-turbo";

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub groq: Groq,
    pub cleanup: Cleanup,
    pub isoquant: IsoQuant,
    pub router: Router,
    pub profiles: Profiles,
    /// `[computer]` configured the computer agent, which was removed for now.
    /// Accepted and ignored so older files still load.
    #[serde(rename = "computer")]
    removed_computer: IgnoredAny,
    pub history: History,
    pub recording: Recording,
    pub sounds: Sounds,
    pub media: Media,
    pub output: Output,
    pub logging: Logging,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Groq {
    pub api_key: String,
    pub model: String,
}

impl Default for Groq {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            model: WHISPER_TURBO.to_owned(),
        }
    }
}

/// Master switch for every cleanup profile.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Cleanup {
    pub enabled: bool,
}

impl Default for Cleanup {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IsoQuant {
    pub enabled: bool,
    pub api_key: String,
    pub api_url: String,
    pub timeout_secs: u64,
}

impl Default for IsoQuant {
    fn default() -> Self {
        Self {
            enabled: true,
            api_key: String::new(),
            api_url: "https://api.isoquant.ai/v1".to_owned(),
            timeout_secs: 30,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Router {
    pub model: String,
    pub instructions: String,
    pub min_confidence: f64,
}

impl Default for Router {
    fn default() -> Self {
        Self {
            model: "isoquant/system-one".to_owned(),
            instructions: "Choose which profile should process this dictated speech.".to_owned(),
            min_confidence: 0.5,
        }
    }
}

/// Profiles by name. Defining any `[profiles.*]` table replaces the built-in set.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(transparent)]
pub struct Profiles(pub BTreeMap<String, Profile>);

impl Default for Profiles {
    fn default() -> Self {
        Self(
            ["default", "prompt"]
                .into_iter()
                .map(|name| (name.to_owned(), Profile::default()))
                .collect(),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Profile {
    pub description: String,
    pub model: String,
    pub prompt: String,
    /// Gave the profile the computer agent's tools; ignored for now.
    #[serde(rename = "tools")]
    removed_tools: IgnoredAny,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            description: String::new(),
            model: "glm-5.3-flash".to_owned(),
            prompt: String::new(),
            removed_tools: IgnoredAny,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct History {
    pub enabled: bool,
    pub entries: usize,
}

impl Default for History {
    fn default() -> Self {
        Self {
            enabled: true,
            entries: 10,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Recording {
    pub device: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Deserialize)]
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

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
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

impl Newlines {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Space => "space",
            Self::ShiftEnter => "shift_enter",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
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
    /// Reads the file and applies environment overrides.
    pub fn load(path: &Path) -> Result<Self> {
        let mut config = Self::read(path)?;
        if let Ok(key) = env::var("GROQ_API_KEY") {
            config.groq.api_key = key;
        }
        if let Ok(model) = env::var("GROQ_MODEL") {
            config.groq.model = model;
        }
        if let Ok(key) = env::var("ISO_QUANT_API_KEY") {
            config.isoquant.api_key = key;
        }
        Ok(config)
    }

    /// Reads the file as written, without environment overrides.
    pub fn read(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("could not read {}", path.display()))?;
        let mut config: Self =
            toml::from_str(&text).with_context(|| format!("invalid config {}", path.display()))?;
        // The built-in computer agent profile, removed for now.
        config
            .profiles
            .0
            .retain(|name, profile| name != "computer" || !profile.prompt.trim().is_empty());
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        if self.sounds.volume > 100 {
            bail!("sounds.volume must be between 0 and 100");
        }
        if !(0.0..=1.0).contains(&self.router.min_confidence) {
            bail!("router.min_confidence must be between 0 and 1");
        }
        Ok(())
    }

    /// Writes these values into the file, keeping its comments and layout.
    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let text = fs::read_to_string(path).unwrap_or_default();
        let mut document: toml_edit::DocumentMut = text
            .parse()
            .with_context(|| format!("invalid config {}", path.display()))?;
        self.write_to(&mut document);
        let temporary = path.with_extension("toml.tmp");
        fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temporary)
            .and_then(|mut file| file.write_all(document.to_string().as_bytes()))
            .with_context(|| format!("could not write {}", temporary.display()))?;
        fs::rename(&temporary, path)
            .with_context(|| format!("could not replace {}", path.display()))?;
        Ok(())
    }

    fn write_to(&self, document: &mut toml_edit::DocumentMut) {
        let root = document.as_table_mut();
        let groq = section(root, "groq");
        set(groq, "api_key", &self.groq.api_key);
        set(groq, "model", &self.groq.model);
        let cleanup = section(root, "cleanup");
        set(cleanup, "enabled", self.cleanup.enabled);
        let isoquant = section(root, "isoquant");
        set(isoquant, "enabled", self.isoquant.enabled);
        set(isoquant, "api_key", &self.isoquant.api_key);
        set(isoquant, "api_url", &self.isoquant.api_url);
        set(isoquant, "timeout_secs", self.isoquant.timeout_secs as i64);
        let router = section(root, "router");
        set(router, "model", &self.router.model);
        set(router, "instructions", &self.router.instructions);
        set(router, "min_confidence", self.router.min_confidence);

        let profiles = section(root, "profiles");
        profiles.set_implicit(true);
        profiles.retain(|name, _| self.profiles.0.contains_key(name));
        for (name, profile) in &self.profiles.0 {
            let table = section(profiles, name);
            set(table, "model", &profile.model);
            set(table, "description", &profile.description);
            set(table, "prompt", &profile.prompt);
            table.remove("tools");
        }
        root.remove("computer");

        let history = section(root, "history");
        set(history, "enabled", self.history.enabled);
        set(history, "entries", self.history.entries as i64);
        set(section(root, "recording"), "device", &self.recording.device);
        let sounds = section(root, "sounds");
        set(sounds, "enabled", self.sounds.enabled);
        set(sounds, "volume", i64::from(self.sounds.volume));
        set_path(sounds, "press", self.sounds.press.as_deref());
        set_path(sounds, "release", self.sounds.release.as_deref());
        let pause = self.media.pause_while_recording;
        set(section(root, "media"), "pause_while_recording", pause);
        let output = section(root, "output");
        set(output, "press_enter", self.output.press_enter);
        set(output, "newlines", self.output.newlines.as_str());
        set_path(section(root, "logging"), "dir", self.logging.dir.as_deref());
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

    /// History lives here: ~/.local/share/hydra-stt by default.
    pub fn data_dir(&self) -> Result<PathBuf> {
        if cfg!(debug_assertions) {
            return Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data"));
        }
        Ok(xdg_dir("XDG_DATA_HOME", ".local/share")?.join(APP_NAME))
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

/// Returns the table at `key`, creating it (or replacing a non-table value).
fn section<'a>(parent: &'a mut toml_edit::Table, key: &str) -> &'a mut toml_edit::Table {
    let item = parent
        .entry(key)
        .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
    if !item.is_table() {
        *item = toml_edit::Item::Table(toml_edit::Table::new());
    }
    item.as_table_mut().expect("just made a table")
}

/// Sets a value, keeping the comment beside the old one. Unchanged values
/// are left alone so their original formatting survives.
fn set(table: &mut toml_edit::Table, key: &str, value: impl Into<toml_edit::Value>) {
    let mut value = value.into();
    if let toml_edit::Value::String(text) = &value
        && text.value().contains('\n')
    {
        value = multiline(text.value());
    }
    match table.get_mut(key).and_then(toml_edit::Item::as_value_mut) {
        Some(old) if same(old, &value) => {}
        Some(old) => {
            *value.decor_mut() = old.decor().clone();
            *old = value;
        }
        None => drop(table.insert(key, toml_edit::Item::Value(value))),
    }
}

fn same(a: &toml_edit::Value, b: &toml_edit::Value) -> bool {
    use toml_edit::Value::{Boolean, Float, Integer, String};
    match (a, b) {
        (String(a), String(b)) => a.value() == b.value(),
        (Integer(a), Integer(b)) => a.value() == b.value(),
        (Float(a), Float(b)) => a.value() == b.value(),
        (Boolean(a), Boolean(b)) => a.value() == b.value(),
        _ => false,
    }
}

/// A `'''literal'''` string when possible, so prompts stay readable in the file.
fn multiline(text: &str) -> toml_edit::Value {
    let literal_safe = !text.contains("'''")
        && !text.ends_with('\'')
        && text
            .chars()
            .all(|c| c == '\n' || c == '\t' || !c.is_control());
    literal_safe
        .then(|| format!("'''\n{text}'''").parse().ok())
        .flatten()
        .unwrap_or_else(|| text.into())
}

fn set_path(table: &mut toml_edit::Table, key: &str, path: Option<&Path>) {
    match path {
        Some(path) => set(table, key, path.to_string_lossy().as_ref()),
        None => drop(table.remove(key)),
    }
}

/// Returns true when the file can be read by users other than its owner.
pub fn is_shared(path: &Path) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.permissions().mode() & 0o077 != 0)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved(config: &Config) -> (String, Config) {
        let directory = env::temp_dir().join(format!("hydra-stt-config-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!("{:p}.toml", config));
        fs::write(&path, TEMPLATE).unwrap();
        config.save(&path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        let read = Config::read(&path).unwrap();
        fs::remove_file(&path).unwrap();
        (text, read)
    }

    #[test]
    fn saving_unchanged_values_keeps_the_file() {
        let config: Config = toml::from_str(TEMPLATE).unwrap();
        assert_eq!(saved(&config).0, TEMPLATE);
    }

    #[test]
    fn files_with_the_removed_agent_still_load() {
        let directory = env::temp_dir().join(format!("hydra-stt-config-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("agent.toml");
        let old = format!(
            "{TEMPLATE}\n[profiles.computer]\nprompt = \"\"\ntools = true\n\n\
             [computer]\nmax_steps = 30\n"
        );
        fs::write(&path, old).unwrap();
        let config = Config::read(&path).unwrap();
        assert!(!config.profiles.0.contains_key("computer"));
        config.save(&path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        fs::remove_file(&path).unwrap();
        assert!(!text.contains("computer") && !text.contains("tools"));
    }

    #[test]
    fn saving_round_trips_and_keeps_comments() {
        let mut config: Config = toml::from_str(TEMPLATE).unwrap();
        config.groq.api_key = "gsk_test".to_owned();
        config.sounds.volume = 40;
        config.sounds.press = Some(PathBuf::from("/tmp/press.wav"));
        config.output.newlines = Newlines::ShiftEnter;
        config.profiles.0.remove("prompt");
        config.profiles.0.insert(
            "email".to_owned(),
            Profile {
                description: "An email.".to_owned(),
                prompt: "Line one.\nLine 'two'.\n".to_owned(),
                ..Profile::default()
            },
        );
        let (text, read) = saved(&config);
        assert_eq!(read, config);
        assert!(text.contains("# Speech-to-text. Required.\napi_key = \"gsk_test\""));
        assert!(text.contains("prompt = '''\nLine one.\nLine 'two'.\n'''"));
        assert!(!text.contains("[profiles.prompt]"));
    }
}
