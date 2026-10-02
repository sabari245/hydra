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
