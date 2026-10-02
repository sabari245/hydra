mod cleanup;
mod control;
mod logging;
mod media;

macro_rules! log {
    ($level:expr, $event:expr, $($arg:tt)*) => {
        logging::event($level, $event, format_args!($($arg)*))
    };
}

use anyhow::{Context, Result, anyhow, bail};
use arboard::Clipboard;
use global_hotkey::{
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
    hotkey::{Code, HotKey, Modifiers},
};
use nix::{sys::signal, unistd::Pid};
use reqwest::multipart::{Form, Part};
use serde::Deserialize;
use std::{
    env, fs,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    process::Stdio,
    process::{Child, Command},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
use tokio::sync::mpsc as tokio_mpsc;

const GROQ_API_URL: &str = "https://api.groq.com/openai/v1";
const DEFAULT_MODEL: &str = "whisper-large-v3-turbo";

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
    if env::args().any(|arg| arg == "--toggle") {
        return control::toggle();
    }
    let log_path = logging::init()?;
    log!(
        "INFO",
        "startup",
        "pid={} session={:?} wayland={:?} display={:?} log={}",
        std::process::id(),
        env::var("XDG_SESSION_TYPE").ok(),
        env::var("WAYLAND_DISPLAY").ok(),
        env::var("DISPLAY").ok(),
        log_path.display()
    );
    let result = run().await;
    if let Err(error) = &result {
        log!("ERROR", "fatal", "{error:#}");
    }
    log!("INFO", "shutdown", "success={}", result.is_ok());
    result
}

async fn run() -> Result<()> {
    if env::args().any(|arg| arg == "--cleanup") {
        let cleaner = cleanup::Cleaner::from_env()?;
        let mut text = String::new();
        std::io::stdin().read_to_string(&mut text)?;
        println!("{}", cleaner.clean(&reqwest::Client::new(), &text).await?);
        return Ok(());
    }
    let api_key = env::var("GROQ_API_KEY")
        .context("GROQ_API_KEY is not set; export it before starting hydra")?;
    let model = env::var("GROQ_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_owned());

    if env::args().any(|arg| arg == "--models") {
        list_models(&api_key).await?;
        return Ok(());
    }

    let cleaner = cleanup::Cleaner::from_env()?;

    let (hotkey_tx, mut hotkey_rx) = tokio_mpsc::unbounded_channel();
    let _control_listener = control::start(hotkey_tx.clone())?;
    if env::var_os("WAYLAND_DISPLAY").is_some() {
        log!(
            "INFO",
            "hotkey_backend",
            "backend=compositor command=hydra --toggle"
        );
    } else {
        start_hotkey_listener(hotkey_tx)?;
    }

    println!("Hydra is running.");
    println!("Press Super+Space to start/stop recording.");
    println!("Groq model: {model}");
    println!("IsoQuant cleanup model: {}", cleanup::MODEL);

    let client = reqwest::Client::new();
    let mut recording = None;

    let mut cycle = 0_u64;
    while hotkey_rx.recv().await.is_some() {
        log!(
            "INFO",
            "toggle_received",
            "cycle={cycle} recording={}",
            recording.is_some()
        );
        if let Some(active) = recording.take() {
            let audio_path = stop_recording(active)?;
            play_click(false);

            if audio_path
                .metadata()
                .map(|metadata| metadata.len() <= 44)
                .unwrap_or(true)
            {
                log!("WARN", "recording_empty", "cycle={cycle}");
                let _ = fs::remove_file(audio_path);
                continue;
            }

            println!("Transcribing...");
            match transcribe(&client, &api_key, &model, &audio_path).await {
                Ok(text) if !text.trim().is_empty() => {
                    log!("INFO", "transcript", "cycle={cycle} text={text:?}");
                    println!("Cleaning up...");
                    let text = match cleaner.clean(&client, &text).await {
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
                    };
                    if text.is_empty() {
                        log!(
                            "INFO",
                            "cleanup_empty",
                            "cycle={cycle} skipping_insertion=true"
                        );
                        let _ = fs::remove_file(audio_path);
                        continue;
                    }
                    if let Err(error) = paste_and_submit(&text) {
                        log!("ERROR", "insertion_failed", "cycle={cycle} {error:#}");
                    }
                }
                Ok(_) => log!("WARN", "transcript_empty", "cycle={cycle}"),
                Err(error) => log!("ERROR", "transcription_failed", "cycle={cycle} {error:#}"),
            }
            let _ = fs::remove_file(audio_path);
        } else {
            cycle += 1;
            log!("INFO", "recording_requested", "cycle={cycle}");
            let paused_players = media::PausedPlayers::pause();
            play_click(true);
            recording = Some(start_recording(paused_players)?);
            println!("Recording...");
        }
    }

    Ok(())
}

fn start_hotkey_listener(tx: tokio_mpsc::UnboundedSender<()>) -> Result<()> {
    let (ready_tx, ready_rx) = mpsc::channel();

    thread::spawn(move || {
        let result = (|| -> Result<()> {
            let manager = GlobalHotKeyManager::new()?;
            let hotkey = HotKey::new(Some(Modifiers::SUPER), Code::Space);
            let hotkey_id = hotkey.id();
            manager.register(hotkey)?;
            log!(
                "INFO",
                "hotkey_registered",
                "backend=X11 binding=Super+Space id={hotkey_id}"
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
                if event.id == hotkey_id && event.state == HotKeyState::Pressed {
                    if tx.send(()).is_err() {
                        break;
                    }
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

fn start_recording(paused_players: media::PausedPlayers) -> Result<Recording> {
    let path = env::temp_dir().join(format!("hydra-{}.wav", std::process::id()));
    let mut child = Command::new("arecord")
        .args(["--quiet", "--format=S16_LE", "--rate=16000", "--channels=1"])
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
    let status = response.status();
    log!("INFO", "groq_response", "status={status}");
    if !status.is_success() {
        bail!("Groq returned {status}: {}", response.text().await?);
    }

    let text = response.json::<TranscriptionResponse>().await?.text;
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
    let status = response.status();
    log!("INFO", "groq_response", "status={status}");
    if !status.is_success() {
        bail!("Groq returned {status}: {}", response.text().await?);
    }

    let models = response.json::<ModelsResponse>().await?.data;
    println!("Speech models available on this account:");
    for model in models
        .iter()
        .filter(|model| model.id.contains("whisper") || model.id.contains("distil-whisper"))
    {
        println!("- {}", model.id);
    }
    Ok(())
}

fn play_click(start: bool) {
    let name = if start { "press" } else { "release" };
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("assets")
        .join(format!("{name}.wav"));
    let result = Command::new("paplay")
        .args(["--stream-name=Hydra feedback", "--volume=60000"])
        .arg(path)
        .output();
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

fn paste_and_submit(text: &str) -> Result<()> {
    log!(
        "INFO",
        "insertion_started",
        "backend={} characters={}",
        if env::var_os("WAYLAND_DISPLAY").is_some() {
            "wtype"
        } else {
            "xdotool"
        },
        text.chars().count()
    );
    if env::var_os("WAYLAND_DISPLAY").is_some() {
        return type_on_wayland(text);
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

fn type_on_wayland(text: &str) -> Result<()> {
    let type_status = Command::new("wtype")
        .arg(text)
        .status()
        .context("could not run wtype; install wtype for Wayland keyboard output")?;
    log!("INFO", "typing_result", "status={type_status}");
    if !type_status.success() {
        bail!("wtype failed while typing the transcript");
    }

    let enter_status = Command::new("wtype")
        .args(["-k", "Return"])
        .status()
        .context("could not send Enter through wtype")?;
    log!("INFO", "enter_result", "status={enter_status}");
    if !enter_status.success() {
        bail!("wtype failed while pressing Enter");
    }
    Ok(())
}
