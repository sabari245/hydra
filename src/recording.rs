//! Recording the microphone to a 16 kHz, 16-bit mono WAV file: with arecord
//! on Linux, and with cpal (WASAPI) on Windows.

use crate::{config, media};
use anyhow::Result;
use std::{env, path::PathBuf, time::Instant};

pub use platform::Recording;

fn audio_path() -> PathBuf {
    env::temp_dir().join(format!("hydra-stt-{}.wav", std::process::id()))
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use anyhow::Context;
    use nix::{sys::signal, unistd::Pid};
    use std::{
        fs,
        io::{BufRead, BufReader},
        process::{Child, Command, Stdio},
        thread,
    };

    #[derive(Debug)]
    pub struct Recording {
        child: Child,
        path: PathBuf,
        started: Instant,
        _paused_players: media::PausedPlayers,
    }

    impl Recording {
        pub fn start(
            config: &config::Recording,
            paused_players: media::PausedPlayers,
        ) -> Result<Self> {
            let path = audio_path();
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
            Ok(Self {
                child,
                path,
                started: Instant::now(),
                _paused_players: paused_players,
            })
        }

        /// Stops recording and returns the WAV file.
        pub fn stop(mut self) -> Result<PathBuf> {
            signal::kill(
                Pid::from_raw(self.child.id() as i32),
                signal::Signal::SIGINT,
            )
            .context("could not stop arecord")?;
            let status = self.child.wait().context("could not wait for arecord")?;
            log!(
                "INFO",
                "recording_stopped",
                "status={status} duration_ms={} bytes={:?}",
                self.started.elapsed().as_millis(),
                fs::metadata(&self.path).map(|m| m.len()).ok()
            );
            Ok(self.path)
        }
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use crate::wav;
    use anyhow::{Context, anyhow, bail};
    use cpal::{
        FromSample, Sample, SampleFormat, SizedSample,
        traits::{DeviceTrait, HostTrait, StreamTrait},
    };
    use std::{
        fs,
        sync::{Arc, Mutex, mpsc},
        thread,
    };

    /// Captured audio, downmixed to mono, at the device's own sample rate.
    struct Capture {
        samples: Vec<f32>,
        rate: u32,
    }

    pub struct Recording {
        stop: mpsc::Sender<()>,
        thread: thread::JoinHandle<Result<Capture>>,
        path: PathBuf,
        started: Instant,
        _paused_players: media::PausedPlayers,
    }

    impl Recording {
        pub fn start(
            config: &config::Recording,
            paused_players: media::PausedPlayers,
        ) -> Result<Self> {
            // cpal streams may not move between threads, so one thread owns
            // the stream for the whole recording.
            let device_name = config.device.clone();
            let (stop, stopped) = mpsc::channel();
            let (ready, started) = mpsc::channel();
            let thread = thread::spawn(move || capture(&device_name, &ready, &stopped));
            started
                .recv()
                .map_err(|_| anyhow!("the recording thread exited"))??;
            let path = audio_path();
            log!("INFO", "recording_started", "path={}", path.display());
            Ok(Self {
                stop,
                thread,
                path,
                started: Instant::now(),
                _paused_players: paused_players,
            })
        }

        /// Stops recording and returns the WAV file.
        pub fn stop(self) -> Result<PathBuf> {
            let _ = self.stop.send(());
            let capture = self
                .thread
                .join()
                .map_err(|_| anyhow!("the recording thread panicked"))??;
            let pcm = wav::resample(&capture.samples, capture.rate);
            fs::write(&self.path, wav::encode(&pcm))
                .with_context(|| format!("could not write {}", self.path.display()))?;
            log!(
                "INFO",
                "recording_stopped",
                "duration_ms={} device_rate={} bytes={}",
                self.started.elapsed().as_millis(),
                capture.rate,
                pcm.len() + 44
            );
            Ok(self.path)
        }
    }

    impl std::fmt::Debug for Recording {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("Recording")
                .field("path", &self.path)
                .finish()
        }
    }

    /// Records until `stopped` receives (or its sender is dropped). Reports on
    /// `ready` once the stream is running, or with the error that stopped it.
    fn capture(
        device_name: &str,
        ready: &mpsc::Sender<Result<()>>,
        stopped: &mpsc::Receiver<()>,
    ) -> Result<Capture> {
        let samples = Arc::new(Mutex::new(Vec::new()));
        let opened = open(device_name, Arc::clone(&samples));
        let (stream, rate) = match opened {
            Ok(opened) => opened,
            Err(error) => {
                let _ = ready.send(Err(anyhow!("{error:#}")));
                return Err(error);
            }
        };
        let _ = ready.send(Ok(()));
        let _ = stopped.recv();
        drop(stream);
        let samples = std::mem::take(&mut *samples.lock().map_err(|_| anyhow!("lock poisoned"))?);
        Ok(Capture { samples, rate })
    }

    fn open(device_name: &str, samples: Arc<Mutex<Vec<f32>>>) -> Result<(cpal::Stream, u32)> {
        let host = cpal::default_host();
        let device = if device_name.is_empty() {
            host.default_input_device()
                .context("no microphone found; check Windows sound settings")?
        } else {
            host.input_devices()?
                .find(|device| device.to_string() == device_name)
                .with_context(|| format!("no microphone named {device_name:?}"))?
        };
        let supported = device
            .default_input_config()
            .context("the microphone has no usable format")?;
        let rate = supported.sample_rate();
        let channels = usize::from(supported.channels());
        let format = supported.sample_format();
        let config = supported.config();
        let stream = match format {
            SampleFormat::I16 => build::<i16>(&device, config, channels, samples),
            SampleFormat::I32 => build::<i32>(&device, config, channels, samples),
            SampleFormat::F32 => build::<f32>(&device, config, channels, samples),
            other => bail!("unsupported microphone sample format {other}"),
        }?;
        stream.play().context("could not start the microphone")?;
        Ok((stream, rate))
    }

    fn build<T>(
        device: &cpal::Device,
        config: cpal::StreamConfig,
        channels: usize,
        samples: Arc<Mutex<Vec<f32>>>,
    ) -> Result<cpal::Stream>
    where
        T: SizedSample,
        f32: FromSample<T>,
    {
        let stream = device.build_input_stream(
            config,
            move |data: &[T], _: &_| {
                if let Ok(mut samples) = samples.lock() {
                    // Downmix each frame to mono.
                    samples.extend(data.chunks(channels).map(|frame| {
                        frame.iter().map(|&s| f32::from_sample(s)).sum::<f32>() / frame.len() as f32
                    }));
                }
            },
            |error| log!("WARN", "microphone_error", "{error}"),
            None,
        )?;
        Ok(stream)
    }
}
