//! Click sounds when recording starts and stops: played with paplay on Linux,
//! and with PlaySound on Windows.

use crate::config;

const PRESS_SOUND: &[u8] = include_bytes!("../assets/press.wav");
const RELEASE_SOUND: &[u8] = include_bytes!("../assets/release.wav");

pub fn play_click(config: &config::Sounds, start: bool) {
    if !config.enabled {
        return;
    }
    let (name, custom, builtin) = if start {
        ("press", &config.press, PRESS_SOUND)
    } else {
        ("release", &config.release, RELEASE_SOUND)
    };
    match platform::play(custom.as_deref(), builtin, config.volume) {
        Ok(backend) => log!(
            "INFO",
            "audio_feedback_played",
            "cue={name} backend={backend}"
        ),
        Err(error) => log!("ERROR", "audio_feedback_failed", "cue={name} {error:#}"),
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use anyhow::{Context, Result, bail};
    use std::{
        io::Write,
        path::Path,
        process::{Command, Stdio},
    };

    pub fn play(custom: Option<&Path>, builtin: &[u8], volume: u8) -> Result<&'static str> {
        let volume = u32::from(volume) * 65536 / 100;
        let mut command = Command::new("paplay");
        command
            .args(["--stream-name=Hydra STT feedback"])
            .arg(format!("--volume={volume}"))
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let output = match custom {
            Some(path) => command.arg(path).output(),
            None => command.stdin(Stdio::piped()).spawn().and_then(|mut child| {
                if let Some(mut stdin) = child.stdin.take() {
                    stdin.write_all(builtin)?;
                }
                child.wait_with_output()
            }),
        }
        .context(
            "could not run paplay; install pulseaudio-utils or your distribution's paplay package",
        )?;
        if !output.status.success() {
            bail!(
                "paplay exited with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok("paplay")
    }
}

#[cfg(windows)]
mod platform {
    use crate::wav;
    use anyhow::{Context, Result, bail};
    use std::{fs, path::Path};
    use windows::{
        Win32::Media::Audio::{PlaySoundW, SND_MEMORY, SND_NODEFAULT, SND_SYNC},
        core::PCWSTR,
    };

    pub fn play(custom: Option<&Path>, builtin: &[u8], volume: u8) -> Result<&'static str> {
        let mut sound = match custom {
            Some(path) => {
                fs::read(path).with_context(|| format!("could not read {}", path.display()))?
            }
            None => builtin.to_vec(),
        };
        // PlaySound has no volume, so 16-bit sounds are scaled instead.
        wav::scale_volume(&mut sound, volume);
        // SAFETY: SND_MEMORY reads a WAV image from the pointer, which stays
        // alive for the whole synchronous call.
        let played = unsafe {
            PlaySoundW(
                PCWSTR(sound.as_ptr().cast()),
                None,
                SND_MEMORY | SND_SYNC | SND_NODEFAULT,
            )
        };
        if !played.as_bool() {
            bail!("Windows could not play the sound; it must be a PCM WAV file");
        }
        Ok("playsound")
    }
}
