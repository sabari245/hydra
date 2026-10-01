use std::process::Command;

#[derive(Debug, Default)]
pub struct PausedPlayers(Vec<String>);

impl PausedPlayers {
    pub fn pause() -> Self {
        let mut paused = Self::default();
        let output = match Command::new("playerctl").arg("--list-all").output() {
            Ok(output) if output.status.success() => output,
            Ok(_) => return paused,
            Err(error) => {
                crate::logging::event("WARN", "media_unavailable", format_args!("{error}"));
                return paused;
            }
        };
        for player in String::from_utf8_lossy(&output.stdout).lines() {
            let status = Command::new("playerctl")
                .args(["--player", player, "status"])
                .output();
            if !matches!(status, Ok(output) if output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "Playing")
            {
                continue;
            }
            match Command::new("playerctl")
                .args(["--player", player, "pause"])
                .output()
            {
                Ok(output) if output.status.success() => {
                    crate::logging::event(
                        "INFO",
                        "media_paused",
                        format_args!("player={player:?}"),
                    );
                    paused.0.push(player.to_owned());
                }
                result => crate::logging::event(
                    "WARN",
                    "media_pause_failed",
                    format_args!("player={player:?} result={result:?}"),
                ),
            }
        }
        paused
    }
}

impl Drop for PausedPlayers {
    fn drop(&mut self) {
        for player in &self.0 {
            let status = Command::new("playerctl")
                .args(["--player", player, "status"])
                .output();
            if !matches!(status, Ok(output) if output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "Paused")
            {
                continue;
            }
            match Command::new("playerctl")
                .args(["--player", player, "play"])
                .output()
            {
                Ok(output) if output.status.success() => crate::logging::event(
                    "INFO",
                    "media_resumed",
                    format_args!("player={player:?}"),
                ),
                result => crate::logging::event(
                    "WARN",
                    "media_resume_failed",
                    format_args!("player={player:?} result={result:?}"),
                ),
            }
        }
    }
}
