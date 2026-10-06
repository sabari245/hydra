// No console window on Windows; commands run from a terminal attach to it.
#![cfg_attr(windows, windows_subsystem = "windows")]

macro_rules! log {
    ($level:expr, $event:expr, $($arg:tt)*) => {
        $crate::logging::event($level, $event, format_args!($($arg)*))
    };
}

mod config;
mod control;
mod history;
mod logging;
mod media;
mod private;
mod profiles;
mod recording;
mod sarvam;
mod service;
mod settings;
mod shortcut;
mod sounds;
mod typing;
mod wav;

use anyhow::{Context, Result, bail};
use reqwest::multipart::{Form, Part};
use serde::Deserialize;
use std::{env, fs, io::Read, path::Path, sync::Arc, time::Instant};
use tokio::{sync::mpsc as tokio_mpsc, task::JoinHandle};

const GROQ_API_URL: &str = "https://api.groq.com/openai/v1";
const USAGE: &str = "\
Usage: hydra-stt [COMMAND]

Commands:
  (none)         Open the Hydra STT window: settings, and starting or
                 stopping Hydra in the background
  --daemon       Run the dictation daemon (what runs in the background)
  --toggle       Start or stop recording in the running daemon (bind it to a
                 key in your compositor; on Windows, Hydra registers the
                 shortcut set in the window itself)
  --stop         Stop the running daemon
  --process [P]  Process a transcript from stdin, routed to a profile or
                 always through profile P, and print the text to type
  --models       List the speech models on your Groq account
  --config-path  Print the configuration file path
  --help         Show this help
  --version      Show the version";

enum Mode {
    Daemon,
    Process(Option<String>),
    Models,
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
    #[cfg(windows)]
    // SAFETY: attaching to the parent's console, if there is one, has no
    // other effect.
    unsafe {
        let _ = windows::Win32::System::Console::AttachConsole(
            windows::Win32::System::Console::ATTACH_PARENT_PROCESS,
        );
    }
    let mode = match env::args().nth(1).as_deref() {
        None | Some("--settings") => return settings::run(),
        Some("--daemon") => Mode::Daemon,
        Some("--toggle") => return control::toggle(),
        Some("--stop") => return control::quit(),
        Some("--process" | "--cleanup") => Mode::Process(env::args().nth(2)),
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
            "Created {}; add your API keys there or in the Hydra STT window.",
            config_path.display()
        );
    }
    let config = Arc::new(config::Config::load(&config_path)?);
    let log_path = logging::init(&config.log_dir()?)?;
    log!(
        "INFO",
        "startup",
        "pid={} version={} session={:?} wayland={:?} config={} log={}",
        std::process::id(),
        env!("CARGO_PKG_VERSION"),
        env::var("XDG_SESSION_TYPE").ok(),
        env::var("WAYLAND_DISPLAY").ok(),
        config_path.display(),
        log_path.display()
    );
    if private::is_shared(&config_path) {
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

async fn run(mode: Mode, config: &Arc<config::Config>, config_path: &Path) -> Result<()> {
    if let Mode::Process(profile) = &mode {
        let pipeline = profiles::Pipeline::from_config(config, config_path)?
            .context("Cleanup is disabled in the configuration")?;
        let mut text = String::new();
        std::io::stdin().read_to_string(&mut text)?;
        let output = pipeline
            .process(&reqwest::Client::new(), &text, profile.as_deref())
            .await;
        println!("{output}");
        return Ok(());
    }
    if let Mode::Models = mode {
        list_models(config.groq_api_key(config_path)?).await?;
        return Ok(());
    }
    let api_key: Arc<str> = if config.sarvam.enabled {
        config.sarvam_api_key(config_path)?
    } else {
        config.groq_api_key(config_path)?
    }
    .into();

    let pipeline = profiles::Pipeline::from_config(config, config_path)?.map(Arc::new);

    let (toggle_tx, mut toggle_rx) = tokio_mpsc::unbounded_channel();
    let _control_listener = control::start(toggle_tx, &config.shortcut)?;
    #[cfg(target_os = "linux")]
    if env::var_os("WAYLAND_DISPLAY").is_none() {
        bail!("Hydra STT needs a Wayland session (WAYLAND_DISPLAY is not set)");
    }

    println!("Hydra STT is running.");
    println!("Bind `hydra-stt --toggle` in your compositor to start/stop recording.");
    if config.sarvam.enabled {
        let sarvam = &config.sarvam;
        println!("Sarvam model: {} ({})", sarvam.model, sarvam.mode.as_str());
    } else {
        println!("Groq model: {}", config.groq.model);
    }
    match &pipeline {
        Some(pipeline) => println!("Profiles: {}", pipeline.profile_names().join(", ")),
        None => println!("Profiles: disabled, typing raw transcripts"),
    }

    let client = reqwest::Client::new();
    let mut recording: Option<recording::Recording> = None;
    let mut processing: Option<JoinHandle<()>> = None;

    let mut cycle = 0_u64;
    let mut shutdown = std::pin::pin!(shutdown_signal()?);
    loop {
        tokio::select! {
            command = toggle_rx.recv() => match command {
                Some(control::Command::Toggle) => {}
                Some(control::Command::Quit) | None => break,
            },
            _ = &mut shutdown => break,
        }
        let busy = processing.as_ref().is_some_and(|task| !task.is_finished());
        log!(
            "INFO",
            "toggle_received",
            "cycle={cycle} recording={} processing={busy}",
            recording.is_some()
        );
        if let Some(active) = recording.take() {
            let audio_path = active.stop()?;
            sounds::play_click(&config.sounds, false);
            let (client, config, api_key, pipeline) = (
                client.clone(),
                Arc::clone(config),
                Arc::clone(&api_key),
                pipeline.clone(),
            );
            processing = Some(tokio::spawn(async move {
                process_recording(
                    &client,
                    &config,
                    &api_key,
                    pipeline.as_deref(),
                    &audio_path,
                    cycle,
                )
                .await;
                let _ = fs::remove_file(audio_path);
            }));
        } else if busy {
            // A toggle while a transcript is being processed cancels it.
            if let Some(task) = processing.take() {
                task.abort();
            }
            log!("INFO", "processing_cancelled", "cycle={cycle}");
            sounds::play_click(&config.sounds, false);
            println!("Cancelled.");
        } else {
            cycle += 1;
            log!("INFO", "recording_requested", "cycle={cycle}");
            let paused_players = if config.media.pause_while_recording {
                media::PausedPlayers::pause()
            } else {
                media::PausedPlayers::default()
            };
            sounds::play_click(&config.sounds, true);
            recording = Some(recording::Recording::start(
                &config.recording,
                paused_players,
            )?);
            println!("Recording...");
        }
    }

    log!(
        "INFO",
        "signal_received",
        "recording={}",
        recording.is_some()
    );
    if let Some(task) = processing {
        task.abort();
    }
    if let Some(active) = recording {
        let audio_path = active.stop()?;
        let _ = fs::remove_file(audio_path);
    }
    Ok(())
}

/// Resolves when the daemon is asked to exit: SIGTERM or SIGINT on Linux,
/// Ctrl+C on Windows.
#[cfg(unix)]
fn shutdown_signal() -> Result<impl Future<Output = ()>> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    Ok(async move {
        tokio::select! {
            _ = terminate.recv() => {}
            _ = interrupt.recv() => {}
        }
    })
}

#[cfg(windows)]
fn shutdown_signal() -> Result<impl Future<Output = ()>> {
    Ok(async {
        let _ = tokio::signal::ctrl_c().await;
    })
}

async fn process_recording(
    client: &reqwest::Client,
    config: &config::Config,
    api_key: &str,
    pipeline: Option<&profiles::Pipeline>,
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
    let transcript = if config.sarvam.enabled {
        match tokio::fs::read(audio_path).await {
            Ok(wav) => sarvam::transcribe(client, api_key, &config.sarvam, &wav).await,
            Err(error) => Err(anyhow::Error::new(error).context("could not read the recording")),
        }
    } else {
        transcribe(client, api_key, &config.groq.model, audio_path).await
    };
    let text = match transcript {
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

    let text = match pipeline {
        None => text,
        Some(pipeline) => {
            println!("Processing...");
            pipeline.process(client, &text, None).await
        }
    };
    if text.is_empty() {
        log!(
            "INFO",
            "output_empty",
            "cycle={cycle} skipping_insertion=true"
        );
        return;
    }
    let text = typing::normalize_lines(&text, config.output.newlines);
    if let Err(error) = typing::type_text(&text, config.output.press_enter) {
        log!("ERROR", "insertion_failed", "cycle={cycle} {error:#}");
    }
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
    println!("Speech models available on this account:");
    for model in speech_models(api_key).await? {
        println!("- {model}");
    }
    Ok(())
}

/// The Whisper models available to this Groq API key.
async fn speech_models(api_key: &str) -> Result<Vec<String>> {
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
    Ok(models
        .into_iter()
        .map(|model| model.id)
        .filter(|id| id.contains("whisper"))
        .collect())
}

async fn check_groq_response(response: reqwest::Response) -> Result<reqwest::Response> {
    let status = response.status();
    log!("INFO", "groq_response", "status={status}");
    if !status.is_success() {
        bail!("Groq returned {status}: {}", response.text().await?);
    }
    Ok(response)
}
