# Hydra STT handoff

Updated: 2026-10-02.

## Current state

- Scope: Wayland on x86_64 Linux only (user decision, 2026-10-02). X11 and
  aarch64 support were removed to limit compatibility work.

- Checkout: `/home/sabari/code/hydra`.
- Public repository: https://github.com/sabari245/hydra (`main`), MIT licensed.
- The app, package and binary are named `hydra-stt` (the Debian `hydra`
  package is THC-Hydra, so the old name would conflict).
- No service or autostart is installed.
- The live Niri configuration has the Super+Space binding in
  `/home/sabari/.config/niri/cfg/keybinds.kdl`, outside Git. It must point at
  `hydra-stt --toggle` (the old `hydra` binary and `hydra.sock` are gone).
- The user confirmed dictation, movie pause/resume, and the sound cues with the
  pre-rename build.

## What it does

0. `hydra-stt` opens the window; the daemon is `hydra-stt --daemon`
   (changed 2026-10-02; bare `hydra-stt` used to be the daemon). The window
   starts it detached and stops it with a `quit` datagram on the socket.
1. Toggle arrives from the compositor (`hydra-stt --toggle` over
   `$XDG_RUNTIME_DIR/hydra-stt.sock`).
2. Pause playing MPRIS players, play the press cue, record with `arecord`
   (mono 16 kHz WAV).
3. On the next toggle, stop recording, resume media, play the release cue.
4. Transcribe with Groq (`whisper-large-v3-turbo` by default).
5. Route to a profile with IsoQuant System One (`/v1/systemone`, `choice`
   question over profile descriptions) when more than one profile exists, then
   rewrite with that profile's prompt on `glm-5.3-flash`. The default profile
   runs in parallel with routing. Fallbacks: low confidence or route error →
   default profile; profile error → default output; default error → raw text.
6. Type with `wtype`. Enter only with
   `press_enter = true`.

## Computer agent (removed for now, upcoming)

The computer-control agent (`src/agent.rs`, `src/tools/`, screenshots via
libwayshot, mouse via wlr-virtual-pointer, long-term memory) was removed on
2026-10-02 to ship dictation first. Its last version is the annotated git tag
`computer-agent`. Config files from that time still load: `[computer]` and
`tools` keys are ignored, a `[profiles.computer]` with an empty prompt is
dropped, and saving from the window removes them. The window shows it as
"Computer agent (upcoming)".

## Files

- `src/main.rs` — CLI, daemon loop, recording, transcription, typing, sounds
- `src/config.rs` — `config.toml` loading, defaults, XDG paths, and saving
  through `toml_edit` so comments survive
- `src/settings.rs` — the window (`hydra-stt` with no arguments), egui via
  eframe (glow, Wayland only): Home page with daemon status/start/stop/
  restart/autostart, then pages editing every config option
- `src/service.rs` — starting the daemon detached (`--daemon`), stopping it
  over the control socket, and the systemd user unit for start at login
- `assets/hydra-stt.desktop` — menu entry; installed by the .deb and by
  `install.sh` (with an absolute Exec path)
- `src/profiles.rs` — profiles, built-in prompts, System One routing,
  and history injection
- `src/history.rs` — per-profile JSONL history
- `src/typing.rs` — wtype output and chunking
- `src/control.rs` — compositor toggle socket
- `src/logging.rs` — private diagnostic log
- `src/media.rs` — MPRIS pause/resume
- `config.example.toml` — commented config, embedded as the first-run template
- `assets/` — feedback cues, embedded in the binary; `scripts/generate-feedback.py`
- `install.sh` — `curl | sh` installer for release tarballs
- `.github/workflows/ci.yml`, `release.yml` — CI and tagged releases

## Configuration

`~/.config/hydra-stt/config.toml` is created on first run (dir `700`, file
`600`). It holds API keys, IsoQuant/router/profile/history settings, ALSA device,
sound volume/files, media pause, Enter, and log dir. `GROQ_API_KEY`,
`GROQ_MODEL`, `ISO_QUANT_API_KEY` env vars override the file;
`HYDRA_STT_CONFIG` selects another file. Unknown keys are rejected.

Data: release builds keep history (`history/`) in
`~/.local/share/hydra-stt`; debug builds use `data/` in the checkout.

Logs: release builds use `~/.local/state/hydra-stt/hydra.log`; debug builds use
`logs/` in the checkout. Both contain transcripts, never keys or audio.

## Releases

Bump `Cargo.toml` version, push a `vX.Y.Z` tag. The release workflow builds
x86_64 only with `cargo-zigbuild` (glibc 2.31 floor), and publishes
`hydra-stt-<target>.tar.gz`, `hydra-stt_<arch>.deb`, `.sha256` files and
`install.sh`. Asset names have no version so `releases/latest/download/` works.

Anonymous `curl | sh` installs work once a tagged release exists.

## Validation completed

- `cargo fmt --check`, `cargo clippy --all-targets`, `cargo test` — passed
- Config first-run creation, permissions, key loading, unknown-field and range
  errors, permission warning — tested in a sandbox HOME
- `--process` and `--models` against live APIs with keys from the config file
- Routing: dictation → default, rambling coding requests → prompt; single
  profile skips routing; low confidence falls back to default
- Daemon startup, single-instance guard, SIGTERM clean shutdown — passed
- Local zigbuild of both targets, `.deb` packaging, installer install /
  checksum mismatch / uninstall — passed
- Not yet run: the GitHub Actions workflows themselves, and a live dictation
  cycle with the renamed binary

## wtype keycode bug (fixed)

wtype assigns the Nth distinct character of one invocation to evdev keycode N.
Characters landing on modifier keycodes (29 Left Ctrl, 42 Left Shift, ...) were
swallowed: in a 4.6k-character dictation, capital I and R vanished. Typing now
splits text into wtype calls of at most 28 distinct characters each.

## Known limitations / next steps

- A toggle while processing cancels it; there is no separate cancel command.
- Groq requests have no explicit timeout or retry.
- No `.rpm` or AUR package yet.
