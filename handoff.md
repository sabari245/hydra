# Hydra STT handoff

Updated: 2026-10-02.

## Current state

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
   `$XDG_RUNTIME_DIR/hydra-stt.sock`) on Wayland, or the `[hotkey] binding`
   global hotkey on X11.
2. Pause playing MPRIS players, play the press cue, record with `arecord`
   (mono 16 kHz WAV).
3. On the next toggle, stop recording, resume media, play the release cue.
4. Transcribe with Groq (`whisper-large-v3-turbo` by default).
5. Clean up with IsoQuant `glm-5.3-flash` (optional; falls back to the raw
   transcript on error).
6. Type with `wtype` (Wayland) or paste with `xdotool` (X11), then Enter.

## Files

- `src/main.rs` — CLI, daemon loop, recording, transcription, typing, sounds
- `src/config.rs` — `config.toml` loading, defaults, XDG paths
- `src/cleanup.rs` — IsoQuant transcript cleanup and its system prompt
- `src/control.rs` — compositor toggle socket
- `src/logging.rs` — private diagnostic log
- `src/media.rs` — MPRIS pause/resume
- `config.example.toml` — commented config, embedded as the first-run template
- `assets/` — feedback cues, embedded in the binary; `scripts/generate-feedback.py`
- `install.sh` — `curl | sh` installer for release tarballs
- `.github/workflows/ci.yml`, `release.yml` — CI and tagged releases

## Configuration

`~/.config/hydra-stt/config.toml` is created on first run (dir `700`, file
`600`). It holds API keys, models, cleanup settings, hotkey, ALSA device,
sound volume/files, media pause, Enter, and log dir. `GROQ_API_KEY`,
`GROQ_MODEL`, `ISO_QUANT_API_KEY` env vars override the file;
`HYDRA_STT_CONFIG` selects another file. Unknown keys are rejected.

Logs: release builds use `~/.local/state/hydra-stt/hydra.log`; debug builds use
`logs/` in the checkout. Both contain transcripts, never keys or audio.

## Releases

Bump `Cargo.toml` version, push a `vX.Y.Z` tag. The release workflow builds
x86_64 and aarch64 with `cargo-zigbuild` (glibc 2.31 floor), and publishes
`hydra-stt-<target>.tar.gz`, `hydra-stt_<arch>.deb`, `.sha256` files and
`install.sh`. Asset names have no version so `releases/latest/download/` works.

Anonymous `curl | sh` installs work once a tagged release exists.

## Validation completed

- `cargo fmt --check`, `cargo clippy --all-targets`, `cargo test` — passed
- Config first-run creation, permissions, key loading, unknown-field and range
  errors, permission warning — tested in a sandbox HOME
- `--cleanup` and `--models` against live APIs with keys from the config file
- Daemon startup, single-instance guard, SIGTERM clean shutdown — passed
- Local zigbuild of both targets, `.deb` packaging, installer install /
  checksum mismatch / uninstall — passed
- Not yet run: the GitHub Actions workflows themselves, and a live dictation
  cycle with the renamed binary

## Known limitations / next steps

- No systemd user unit or autostart yet.
- No cancel command; the second toggle always submits.
- Toggles received while transcribing are queued.
- Groq requests have no explicit timeout or retry.
- No `.rpm` or AUR package yet.
