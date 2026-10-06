//! Pausing media players while recording and resuming them afterwards:
//! MPRIS players through playerctl on Linux, and the system media sessions
//! (Spotify, browsers and so on) on Windows.

pub use platform::PausedPlayers;

#[cfg(target_os = "linux")]
mod platform {
    use std::process::Command;

    #[derive(Debug, Default)]
    pub struct PausedPlayers(Vec<String>);

    fn has_status(player: &str, expected: &str) -> bool {
        matches!(
            Command::new("playerctl").args(["--player", player, "status"]).output(),
            Ok(output) if output.status.success()
                && String::from_utf8_lossy(&output.stdout).trim() == expected
        )
    }

    impl PausedPlayers {
        pub fn pause() -> Self {
            let mut paused = Self::default();
            let output = match Command::new("playerctl").arg("--list-all").output() {
                Ok(output) if output.status.success() => output,
                Ok(_) => return paused,
                Err(error) => {
                    log!("WARN", "media_unavailable", "{error}");
                    return paused;
                }
            };
            for player in String::from_utf8_lossy(&output.stdout).lines() {
                if !has_status(player, "Playing") {
                    continue;
                }
                match Command::new("playerctl")
                    .args(["--player", player, "pause"])
                    .output()
                {
                    Ok(output) if output.status.success() => {
                        log!("INFO", "media_paused", "player={player:?}");
                        paused.0.push(player.to_owned());
                    }
                    result => log!(
                        "WARN",
                        "media_pause_failed",
                        "player={player:?} result={result:?}"
                    ),
                }
            }
            paused
        }
    }

    impl Drop for PausedPlayers {
        fn drop(&mut self) {
            for player in &self.0 {
                if !has_status(player, "Paused") {
                    continue;
                }
                match Command::new("playerctl")
                    .args(["--player", player, "play"])
                    .output()
                {
                    Ok(output) if output.status.success() => {
                        log!("INFO", "media_resumed", "player={player:?}")
                    }
                    result => log!(
                        "WARN",
                        "media_resume_failed",
                        "player={player:?} result={result:?}"
                    ),
                }
            }
        }
    }
}

#[cfg(windows)]
mod platform {
    use windows::Media::Control::{
        GlobalSystemMediaTransportControlsSession as Session,
        GlobalSystemMediaTransportControlsSessionManager as Manager,
        GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
    };

    #[derive(Default)]
    pub struct PausedPlayers(Vec<Session>);

    impl std::fmt::Debug for PausedPlayers {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(formatter, "PausedPlayers({})", self.0.len())
        }
    }

    fn name(session: &Session) -> String {
        session
            .SourceAppUserModelId()
            .map(|id| id.to_string())
            .unwrap_or_default()
    }

    fn has_status(session: &Session, expected: Status) -> bool {
        session
            .GetPlaybackInfo()
            .and_then(|info| info.PlaybackStatus())
            .is_ok_and(|status| status == expected)
    }

    impl PausedPlayers {
        pub fn pause() -> Self {
            let mut paused = Self::default();
            let sessions = match Manager::RequestAsync()
                .and_then(|request| request.join())
                .and_then(|manager| manager.GetSessions())
            {
                Ok(sessions) => sessions,
                Err(error) => {
                    log!("WARN", "media_unavailable", "{error}");
                    return paused;
                }
            };
            for session in sessions {
                if !has_status(&session, Status::Playing) {
                    continue;
                }
                let player = name(&session);
                match session.TryPauseAsync().and_then(|pause| pause.join()) {
                    Ok(true) => {
                        log!("INFO", "media_paused", "player={player:?}");
                        paused.0.push(session);
                    }
                    result => log!(
                        "WARN",
                        "media_pause_failed",
                        "player={player:?} result={result:?}"
                    ),
                }
            }
            paused
        }
    }

    impl Drop for PausedPlayers {
        fn drop(&mut self) {
            for session in &self.0 {
                if !has_status(session, Status::Paused) {
                    continue;
                }
                let player = name(session);
                match session.TryPlayAsync().and_then(|play| play.join()) {
                    Ok(true) => log!("INFO", "media_resumed", "player={player:?}"),
                    result => log!(
                        "WARN",
                        "media_resume_failed",
                        "player={player:?} result={result:?}"
                    ),
                }
            }
        }
    }
}
