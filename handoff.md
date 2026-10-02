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
7. A profile with tools (built-in `computer`) runs the agent in `src/agent.rs`
   instead and types nothing: a loop of IsoQuant chat calls with tools, the
   two newest screenshots kept as images, a summary notification at the end.
   Processing runs as a task; a toggle while it runs aborts it, and each
   bash command's process group is killed on drop.

## Files

- `src/main.rs` — CLI, daemon loop, recording, transcription, typing, sounds
- `src/config.rs` — `config.toml` loading, defaults, XDG paths
- `src/profiles.rs` — profiles, built-in prompts, System One routing, memory
  and history injection
- `src/agent.rs` — computer agent loop (shape of Anthropic's `loop.py`)
- `src/tools/` — agent tools, each ported from Anthropic MIT reference code:
  `computer.rs` (computer.py), `bash.rs` (shell.py), `editor.rs` (editor.py),
  `memory.rs` (claude-cookbooks memory_tool.py)
- `src/screenshot.rs` — libwayshot capture, JPEG encode
- `src/pointer.rs` — wlr-virtual-pointer mouse (absolute, per monitor)
- `src/history.rs` — per-profile JSONL history
- `src/typing.rs` — wtype output, chunking, agent typing
- `src/control.rs` — compositor toggle socket
- `src/logging.rs` — private diagnostic log
- `src/media.rs` — MPRIS pause/resume
- `config.example.toml` — commented config, embedded as the first-run template
- `assets/` — feedback cues, embedded in the binary; `scripts/generate-feedback.py`
- `install.sh` — `curl | sh` installer for release tarballs
- `.github/workflows/ci.yml`, `release.yml` — CI and tagged releases

## Configuration

`~/.config/hydra-stt/config.toml` is created on first run (dir `700`, file
`600`). It holds API keys, IsoQuant/router/profile/agent/history settings, ALSA device,
sound volume/files, media pause, Enter, and log dir. `GROQ_API_KEY`,
`GROQ_MODEL`, `ISO_QUANT_API_KEY` env vars override the file;
`HYDRA_STT_CONFIG` selects another file. Unknown keys are rejected.

Data: release builds keep memory (`memories/`) and history (`history/`) in
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

## Computer agent notes

- Niri's screencopy does not draw the cursor, so the agent cannot see the
  pointer; it works from coordinates in the latest screenshot.
- ydotool was rejected for the mouse: its absolute moves go through pointer
  acceleration. Niri exposes `zwlr_virtual_pointer_manager_v1` v2, which takes
  absolute positions per output.
- System One routes computer requests reliably with the current description;
  below `min_confidence` they fall back to default (text is typed instead).
- Live-tested: describe a monitor, save memory, count files with bash, move
  the pointer. Not yet tested live: clicking, typing into forms, multi-step
  browser tasks.

## Known limitations / next steps

- No systemd user unit or autostart yet.
- A toggle while processing cancels it; there is no separate cancel command.
- Groq requests have no explicit timeout or retry.
- No `.rpm` or AUR package yet.
