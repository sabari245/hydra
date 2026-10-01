# Hydra MVP Handoff

## What was built

Hydra is a local Linux dictation daemon written in Rust.

Flow:

1. Receive `Super+Space` through Niri and `hydra --toggle` on Wayland,
   or register a global hotkey on X11.
2. Pause playing MPRIS media players and play the press cue.
3. Start microphone recording with `arecord` at mono 16 kHz WAV.
4. On the next `Super+Space`, stop recording, resume paused media,
   and play the release cue.
5. Upload the WAV to Groq's OpenAI-compatible transcription endpoint.
6. Use `whisper-large-v3-turbo` by default.
7. Type the transcript into the focused application and press Enter.

## Files

- `src/main.rs` — application implementation
- `src/control.rs` — local compositor toggle socket
- `src/logging.rs` — private diagnostic logs, including transcripts
- `src/media.rs` — MPRIS media pause/resume
- `assets/` and `scripts/generate-feedback.py` — feedback cues and generator
- `Cargo.toml` — Rust dependencies
- `.env.example` — environment variable template
- `README.md` — user setup and usage instructions
- `.gitignore` — ignores build output, `.env`, and logs

## Setup

The API key must be supplied through the environment. `.env` is ignored by Git
and is not loaded automatically:

```sh
export GROQ_API_KEY='your-new-groq-key'
```

Run the daemon:

```sh
cargo run --release
```

Or run the compiled binary:

```sh
./target/release/hydra
```

To list speech models available to the configured Groq account:

```sh
cargo run --release -- --models
```

The model can be changed with:

```sh
export GROQ_MODEL=whisper-large-v3
```

## System requirements

- Rust/Cargo
- `arecord` from ALSA utilities
- `wtype` on Wayland, or `xdotool` on X11
- `paplay` for feedback and `playerctl` for media control
- Working microphone and audio output
- Groq API access

The current environment is Niri/Wayland. The Niri shortcut calls `hydra --toggle`
through a private Unix socket. See README.md for the binding. Wayland typing
uses `wtype` with its default zero delay.

## Validation completed

- `cargo fmt -- --check` — passed
- `cargo check` — passed
- `cargo test` — passed; no tests are currently defined
- `cargo build --release` — passed
- Runtime startup smoke test — passed
- Live Niri shortcut, recording, transcription, typing, and Enter — confirmed
- Media pause and resume — confirmed in logs and by the user
- Updated press/release cues — preview played successfully and heard by the user

## Known limitations / next steps

- `global-hotkey` supports X11 only. Wayland requires a compositor binding
  invoking `hydra --toggle`; it does not register an X11 hotkey.
- Recording uses the system ALSA default input device. Device selection is not
  configurable yet.
- Transcription is synchronous after stopping, so another recording cannot be
  started while the Groq request is in progress.
- There is no tray icon, service file, automatic startup, retry handling, or
  configurable audio feedback yet.
- Media pause requires MPRIS support. Terminating the daemon during recording
  does not provide graceful recorder cleanup or media restoration.
