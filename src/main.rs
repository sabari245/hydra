macro_rules! log {
    ($level:expr, $event:expr, $($arg:tt)*) => {
        $crate::logging::event($level, $event, format_args!($($arg)*))
    };
}

mod cleanup;
mod config;
mod control;
mod logging;
mod media;

use anyhow::{Context, Result, anyhow, bail};
use arboard::Clipboard;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState, hotkey::HotKey};
use nix::{sys::signal, unistd::Pid};
use reqwest::multipart::{Form, Part};
use serde::Deserialize;
use std::{
    env, fs,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::Stdio,
    process::{Child, Command},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
use tokio::{
    signal::unix::{self as tokio_signal, SignalKind},
    sync::mpsc as tokio_mpsc,
};

const GROQ_API_URL: &str = "https://api.groq.com/openai/v1";
const PRESS_SOUND: &[u8] = include_bytes!("../assets/press.wav");
const RELEASE_SOUND: &[u8] = include_bytes!("../assets/release.wav");
const USAGE: &str = "\
Usage: hydra-stt [COMMAND]

Commands:
  (none)         Run the dictation daemon
  --toggle       Start or stop recording in the running daemon
  --cleanup      Clean up a transcript read from stdin and print it
  --models       List the speech models on your Groq account
  --config-path  Print the configuration file path
  --help         Show this help
  --version      Show the version";

enum Mode {
    Daemon,
    Cleanup,
    Models,
}

#[derive(Debug)]
struct Recording {
    child: Child,
    path: PathBuf,
    started: Instant,
    _paused_players: media::PausedPlayers,
}

#[derive(Debug, Deserialize)]
struct TranscriptionResponse {
    text: String,
}

#[derive(Debug, Deserialize)]
struct ModelsResponse {
    data: Vec<ModelInfo>,
}

#[derive(Debug, Deserialize)]
struct ModelInfo {
    id: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let mode = match env::args().nth(1).as_deref() {
        None => Mode::Daemon,
        Some("--toggle") => return control::toggle(),
        Some("--cleanup") => Mode::Cleanup,
        Some("--models") => Mode::Models,
        Some("--config-path") => {
            println!("{}", config::path()?.display());
            return Ok(());
        }
        Some("--help" | "-h") => {
            println!("{USAGE}");
            return Ok(());
        }
        Some("--version" | "-V") => {
            println!("hydra-stt {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some(other) => bail!("unknown argument {other:?}\n\n{USAGE}"),
    };

    let config_path = config::path()?;
    if config::ensure_exists(&config_path)? {
        eprintln!(
            "Created {}; add your API keys there.",
            config_path.display()
        );
    }
    let config = config::Config::load(&config_path)?;
    let log_path = logging::init(&config.log_dir()?)?;
    log!(
        "INFO",
        "startup",
        "pid={} version={} session={:?} wayland={:?} display={:?} config={} log={}",
        std::process::id(),
        env!("CARGO_PKG_VERSION"),
        env::var("XDG_SESSION_TYPE").ok(),
        env::var("WAYLAND_DISPLAY").ok(),
        env::var("DISPLAY").ok(),
        config_path.display(),
        log_path.display()
    );
    if config::is_shared(&config_path) {
        log!(
            "WARN",
            "config_permissions",
            "{} is readable by other users; run chmod 600 on it",
            config_path.display()
        );
    }
    let result = run(mode, &config, &config_path).await;
    if let Err(error) = &result {
        log!("ERROR", "fatal", "{error:#}");
    }
    log!("INFO", "shutdown", "success={}", result.is_ok());
    result
}

async fn run(mode: Mode, config: &config::Config, config_path: &Path) -> Result<()> {
    if let Mode::Cleanup = mode {
        let cleaner =
            cleanup::Cleaner::from_config(&config.cleanup, config.output.newlines, config_path)?
                .context("cleanup is disabled in the configuration")?;
        let mut text = String::new();
        std::io::stdin().read_to_string(&mut text)?;
        println!("{}", cleaner.clean(&reqwest::Client::new(), &text).await?);
        return Ok(());
    }
    let api_key = config.groq_api_key(config_path)?;
    let model = config.groq.model.as_str();

    if let Mode::Models = mode {
        list_models(api_key).await?;
        return Ok(());
    }

    let cleaner =
        cleanup::Cleaner::from_config(&config.cleanup, config.output.newlines, config_path)?;

    let (hotkey_tx, mut hotkey_rx) = tokio_mpsc::unbounded_channel();
    let _control_listener = control::start(hotkey_tx.clone())?;
    let wayland = env::var_os("WAYLAND_DISPLAY").is_some();
    if wayland {
        log!(
            "INFO",
            "hotkey_backend",
            "backend=compositor command=hydra-stt --toggle"
        );
    } else {
        let hotkey = config
            .hotkey
            .binding
            .parse::<HotKey>()
            .with_context(|| format!("invalid hotkey.binding {:?}", config.hotkey.binding))?;
        start_hotkey_listener(hotkey, config.hotkey.binding.clone(), hotkey_tx)?;
    }

    println!("Hydra STT is running.");
    if wayland {
        println!("Bind `hydra-stt --toggle` in your compositor to start/stop recording.");
    } else {
        println!("Press {} to start/stop recording.", config.hotkey.binding);
    }
    println!("Groq model: {model}");
    match &cleaner {
        Some(cleaner) => println!("Cleanup model: {}", cleaner.model),
        None => println!("Cleanup: disabled"),
    }

    let client = reqwest::Client::new();
    let mut recording = None;

    let mut cycle = 0_u64;
    let mut terminate = tokio_signal::signal(SignalKind::terminate())?;
    let mut interrupt = tokio_signal::signal(SignalKind::interrupt())?;
    loop {
        tokio::select! {
            toggle = hotkey_rx.recv() => if toggle.is_none() { break },
            _ = terminate.recv() => break,
            _ = interrupt.recv() => break,
        }
        log!(
            "INFO",
            "toggle_received",
            "cycle={cycle} recording={}",
            recording.is_some()
        );
        if let Some(active) = recording.take() {
            let audio_path = stop_recording(active)?;
            play_click(&config.sounds, false);
            process_recording(
                &client,
                config,
                api_key,
                cleaner.as_ref(),
                &audio_path,
                cycle,
            )
            .await;
            let _ = fs::remove_file(audio_path);
        } else {
            cycle += 1;
            log!("INFO", "recording_requested", "cycle={cycle}");
            let paused_players = if config.media.pause_while_recording {
                media::PausedPlayers::pause()
            } else {
                media::PausedPlayers::default()
            };
            play_click(&config.sounds, true);
            recording = Some(start_recording(&config.recording, paused_players)?);
            println!("Recording...");
        }
    }

    log!(
        "INFO",
        "signal_received",
        "recording={}",
        recording.is_some()
    );
    if let Some(active) = recording {
        let audio_path = stop_recording(active)?;
        let _ = fs::remove_file(audio_path);
    }
    Ok(())
}

async fn process_recording(
    client: &reqwest::Client,
    config: &config::Config,
    api_key: &str,
    cleaner: Option<&cleanup::Cleaner>,
    audio_path: &Path,
    cycle: u64,
) {
    if audio_path
        .metadata()
        .map(|metadata| metadata.len() <= 44)
        .unwrap_or(true)
    {
        log!("WARN", "recording_empty", "cycle={cycle}");
        return;
    }

    println!("Transcribing...");
    let text = match transcribe(client, api_key, &config.groq.model, audio_path).await {
        Ok(text) if !text.trim().is_empty() => text,
        Ok(_) => {
            log!("WARN", "transcript_empty", "cycle={cycle}");
            return;
        }
        Err(error) => {
            log!("ERROR", "transcription_failed", "cycle={cycle} {error:#}");
            return;
        }
    };
    log!("INFO", "transcript", "cycle={cycle} text={text:?}");

    let text = match cleaner {
        None => text,
        Some(cleaner) => {
            println!("Cleaning up...");
            clean_or_raw(client, cleaner, text, cycle).await
        }
    };
    if text.is_empty() {
        log!(
            "INFO",
            "cleanup_empty",
            "cycle={cycle} skipping_insertion=true"
        );
        return;
    }
    let text = normalize_lines(&text, config.output.newlines);
    if let Err(error) = paste_and_submit(&text, config.output.press_enter) {
        log!("ERROR", "insertion_failed", "cycle={cycle} {error:#}");
    }
}

/// A typed "\n" is a Return key press that would submit partial text, so line
/// breaks are either removed or kept for typing as Shift+Enter.
fn normalize_lines(text: &str, newlines: config::Newlines) -> String {
    let join_words = |line: &str| line.split_whitespace().collect::<Vec<_>>().join(" ");
    match newlines {
        config::Newlines::Space => join_words(text),
        config::Newlines::ShiftEnter => {
            let mut lines: Vec<String> = Vec::new();
            for line in text.lines().map(join_words) {
                // Keep at most one blank line between paragraphs.
                if !line.is_empty() || lines.last().is_some_and(|last| !last.is_empty()) {
                    lines.push(line);
                }
            }
            lines.join("\n").trim_end().to_owned()
        }
    }
}

async fn clean_or_raw(
    client: &reqwest::Client,
    cleaner: &cleanup::Cleaner,
    text: String,
    cycle: u64,
) -> String {
    match cleaner.clean(client, &text).await {
        Ok(cleaned) => {
            log!(
                "INFO",
                "cleaned_transcript",
                "cycle={cycle} text={cleaned:?}"
            );
            cleaned
        }
        Err(error) => {
            log!(
                "WARN",
                "cleanup_failed",
                "cycle={cycle} using_raw_transcript=true {error:#}"
            );
            text
        }
    }
}

fn start_hotkey_listener(
    hotkey: HotKey,
    binding: String,
    tx: tokio_mpsc::UnboundedSender<()>,
) -> Result<()> {
    let (ready_tx, ready_rx) = mpsc::channel();

    thread::spawn(move || {
        let result = (|| -> Result<()> {
            let manager = GlobalHotKeyManager::new()?;
            let hotkey_id = hotkey.id();
            manager.register(hotkey)?;
            log!(
                "INFO",
                "hotkey_registered",
                "backend=X11 binding={binding} id={hotkey_id}"
            );
            ready_tx
                .send(Ok(()))
                .map_err(|_| anyhow!("hotkey listener was not initialized"))?;

            let receiver = GlobalHotKeyEvent::receiver();
            loop {
                let event = receiver
                    .recv()
                    .map_err(|_| anyhow!("hotkey event channel closed"))?;
                log!(
                    "INFO",
                    "hotkey_event",
                    "id={} state={:?}",
                    event.id,
                    event.state
                );
                if event.id == hotkey_id
                    && event.state == HotKeyState::Pressed
                    && tx.send(()).is_err()
                {
                    break;
                }
            }
            Ok(())
        })();

        if let Err(error) = result {
            log!("ERROR", "hotkey_listener_failed", "{error:#}");
            let _ = ready_tx.send(Err(error));
        }
    });

    ready_rx
        .recv()
        .context("hotkey listener did not respond")??;
    Ok(())
}

fn start_recording(
    config: &config::Recording,
    paused_players: media::PausedPlayers,
) -> Result<Recording> {
    let path = env::temp_dir().join(format!("hydra-stt-{}.wav", std::process::id()));
    let mut command = Command::new("arecord");
    command.args(["--quiet", "--format=S16_LE", "--rate=16000", "--channels=1"]);
    if !config.device.is_empty() {
        command.arg(format!("--device={}", config.device));
    }
    let mut child = command
        .arg(&path)
        .stderr(Stdio::piped())
        .spawn()
        .context("could not start arecord; install ALSA utilities")?;

    if let Some(stderr) = child.stderr.take() {
        thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                match line {
                    Ok(line) => log!("WARN", "arecord_stderr", "{line}"),
                    Err(error) => {
                        log!("ERROR", "arecord_stderr_failed", "{error}");
                        break;
                    }
                }
            }
        });
    }
    log!(
        "INFO",
        "recording_started",
        "pid={} path={}",
        child.id(),
        path.display()
    );
    Ok(Recording {
        child,
        path,
        started: Instant::now(),
        _paused_players: paused_players,
    })
}

fn stop_recording(mut recording: Recording) -> Result<PathBuf> {
    signal::kill(
        Pid::from_raw(recording.child.id() as i32),
        signal::Signal::SIGINT,
    )
    .context("could not stop arecord")?;
    let status = recording
        .child
        .wait()
        .context("could not wait for arecord")?;
    log!(
        "INFO",
        "recording_stopped",
        "status={status} duration_ms={} bytes={:?}",
        recording.started.elapsed().as_millis(),
        fs::metadata(&recording.path).map(|m| m.len()).ok()
    );
    Ok(recording.path)
}

async fn transcribe(
    client: &reqwest::Client,
    api_key: &str,
    model: &str,
    audio_path: &Path,
) -> Result<String> {
    let audio = tokio::fs::read(audio_path)
        .await
        .context("could not read the recording")?;
    let started = Instant::now();
    log!(
        "INFO",
        "transcription_request",
        "model={model} audio_bytes={}",
        audio.len()
    );
    let file = Part::bytes(audio)
        .file_name("recording.wav")
        .mime_str("audio/wav")?;
    let form = Form::new()
        .part("file", file)
        .text("model", model.to_owned())
        .text("response_format", "json")
        .text("temperature", "0");

    let response = client
        .post(format!("{GROQ_API_URL}/audio/transcriptions"))
        .bearer_auth(api_key)
        .multipart(form)
        .send()
        .await
        .context("request to Groq failed")?;
    let text = check_groq_response(response)
        .await?
        .json::<TranscriptionResponse>()
        .await?
        .text;
    log!(
        "INFO",
        "transcription_completed",
        "duration_ms={}",
        started.elapsed().as_millis()
    );
    Ok(text)
}

async fn list_models(api_key: &str) -> Result<()> {
    let response = reqwest::Client::new()
        .get(format!("{GROQ_API_URL}/models"))
        .bearer_auth(api_key)
        .send()
        .await
        .context("request to Groq failed")?;
    let models = check_groq_response(response)
        .await?
        .json::<ModelsResponse>()
        .await?
        .data;
    println!("Speech models available on this account:");
    for model in models.iter().filter(|model| model.id.contains("whisper")) {
        println!("- {}", model.id);
    }
    Ok(())
}

async fn check_groq_response(response: reqwest::Response) -> Result<reqwest::Response> {
    let status = response.status();
    log!("INFO", "groq_response", "status={status}");
    if !status.is_success() {
        bail!("Groq returned {status}: {}", response.text().await?);
    }
    Ok(response)
}

fn play_click(config: &config::Sounds, start: bool) {
    if !config.enabled {
        return;
    }
    let (name, custom, builtin) = if start {
        ("press", &config.press, PRESS_SOUND)
    } else {
        ("release", &config.release, RELEASE_SOUND)
    };
    let volume = u32::from(config.volume) * 65536 / 100;
    let mut command = Command::new("paplay");
    command
        .args(["--stream-name=Hydra STT feedback"])
        .arg(format!("--volume={volume}"))
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if let Some(path) = custom {
        command.arg(path);
    }
    let result = if custom.is_some() {
        command.output()
    } else {
        command.stdin(Stdio::piped()).spawn().and_then(|mut child| {
            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(builtin)?;
            }
            child.wait_with_output()
        })
    };
    match result {
        Ok(output) if output.status.success() => {
            log!("INFO", "audio_feedback_played", "cue={name} backend=paplay");
        }
        Ok(output) => log!(
            "ERROR",
            "audio_feedback_failed",
            "cue={name} status={} stderr={:?}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ),
        Err(error) => log!(
            "ERROR",
            "audio_feedback_failed",
            "cue={name} {error}; install pulseaudio-utils or your distribution's paplay package"
        ),
    }
}

fn paste_and_submit(text: &str, press_enter: bool) -> Result<()> {
    let wayland = env::var_os("WAYLAND_DISPLAY").is_some();
    log!(
        "INFO",
        "insertion_started",
        "backend={} characters={}",
        if wayland { "wtype" } else { "xdotool" },
        text.chars().count()
    );
    if wayland {
        return type_on_wayland(text, press_enter);
    }

    let mut clipboard = Clipboard::new().context("could not access the clipboard")?;
    clipboard
        .set_text(text)
        .context("could not put the transcript on the clipboard")?;

    let paste_status = Command::new("xdotool")
        .args(["key", "--clearmodifiers", "ctrl+v"])
        .status()
        .context("could not run xdotool; install xdotool for keyboard output")?;
    if !paste_status.success() {
        bail!("xdotool failed while pasting the transcript");
    }

    if !press_enter {
        log!(
            "INFO",
            "insertion_completed",
            "backend=xdotool enter_sent=false"
        );
        return Ok(());
    }
    thread::sleep(Duration::from_millis(40));
    let enter_status = Command::new("xdotool")
        .args(["key", "--clearmodifiers", "Return"])
        .status()
        .context("could not send Enter through xdotool")?;
    log!("INFO", "enter_result", "status={enter_status}");
    if !enter_status.success() {
        bail!("xdotool failed while pressing Enter");
    }
    log!(
        "INFO",
        "insertion_completed",
        "backend=xdotool enter_sent=true"
    );
    Ok(())
}

fn type_on_wayland(text: &str, press_enter: bool) -> Result<()> {
    // Everything after `--` is text to wtype, so each line needs its own call.
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            wtype(
                &["-M", "shift", "-k", "Return", "-m", "shift"],
                "Shift+Enter",
            )?;
        }
        if !line.is_empty() {
            wtype(&["--", line], "the transcript")?;
        }
    }
    log!("INFO", "typing_result", "lines={}", text.lines().count());
    if !press_enter {
        return Ok(());
    }

    wtype(&["-k", "Return"], "Enter")?;
    log!("INFO", "enter_result", "sent=true");
    Ok(())
}

fn wtype(args: &[&str], what: &str) -> Result<()> {
    let status = Command::new("wtype")
        .args(args)
        .status()
        .context("could not run wtype; install wtype for Wayland keyboard output")?;
    if !status.success() {
        bail!("wtype failed while typing {what}: {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use config::Newlines::{ShiftEnter, Space};

    #[test]
    fn space_joins_everything_into_one_line() {
        assert_eq!(
            normalize_lines("First  line.\n\nSecond\tline.\n", Space),
            "First line. Second line."
        );
    }

    #[test]
    fn shift_enter_keeps_single_blank_lines() {
        assert_eq!(
            normalize_lines("One  two.\n\n\n\nThree.\n- four\n\n", ShiftEnter),
            "One two.\n\nThree.\n- four"
        );
    }
}
