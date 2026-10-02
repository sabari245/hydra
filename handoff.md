# Hydra handoff

Updated: 2026-10-02.

## Current state

- Checkout: `/home/sabari/code/hydra`.
- Private repository: https://github.com/sabari245/hydra.
- Branch: `main`, tracking `origin/main`.
- Initial implementation commit: `4cb0e7c`.
- Hydra is stopped at the user's request. No service or autostart is installed.
- The live Niri configuration includes the Super+Space binding in
  `/home/sabari/.config/niri/cfg/keybinds.kdl`. That host file is outside Git.
- The user confirmed dictation, movie pause/resume, and the updated sound preview.


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

To use the local `.env`, export its values before starting:

```sh
cd /home/sabari/code/hydra
set -a
source .env
set +a
cargo run --release
```

The daemon stays in the foreground. Ctrl+C stops it; finish recording first
because signal shutdown does not currently clean up the recorder or media.

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

## Niri input and troubleshooting

The original global hotkey registered successfully under XWayland but did not
receive shortcut events from native Wayland applications. The fix uses a Niri
binding and a local Unix datagram socket instead:

```kdl
Mod+Space repeat=false hotkey-overlay-title="Hydra Dictation" {
    spawn "/home/sabari/code/hydra/target/release/hydra" "--toggle";
}
```

The socket is `$XDG_RUNTIME_DIR/hydra.sock`, with permissions `600`. Toggle
commands do not require the Groq key. A second daemon is rejected, and stale
sockets are recovered when the daemon starts. Use `niri validate` after editing
the compositor configuration.

Diagnostic logs append to `logs/hydra.log`. The directory has permissions `700`
and the file `600`. The log contains recognized speech, recording metadata,
Groq response status, media events, feedback playback, typing, and errors.
Credentials and audio contents are not written to the log. Logs, `.env`, and
`target/` are excluded from Git. Logs currently have no rotation.

```sh
tail -f logs/hydra.log
```

On Wayland, look for `compositor_toggle`, `toggle_received`,
`recording_started`, `recording_stopped`, `transcription_request`,
`groq_response`, `transcript`, `typing_result`, and `enter_result`.
`media_paused` and `media_resumed` identify affected players.
`audio_feedback_played` reports successful `paplay` completion; it does not
prove the sound was audible to the user.

## Feedback and media behavior

The first synthesized cues were not audible to the user. Feedback now uses
140 ms WAV files through `paplay`, which routes to the desktop default output
and waits for playback completion. The user heard both updated cues in a
standalone preview. Their audibility during dictation still needs confirmation.

Regenerate the assets with:

```sh
python scripts/generate-feedback.py
```

Media is paused before the press cue and recording. Only players reported as
Playing and successfully paused by Hydra are tracked. At recording stop,
tracked players still reported as Paused are resumed before transcription.
Already paused players are left alone. This requires MPRIS controls exposed
through `playerctl`; it cannot pause every arbitrary audio stream.

## Validation completed

- `cargo fmt -- --check` — passed
- `cargo check` — passed
- `cargo test` — passed; no tests are currently defined
- `cargo build --release` — passed
- Runtime startup smoke test — passed
- Live Niri shortcut, recording, transcription, typing, and Enter — confirmed
- Media pause and resume — confirmed in logs and by the user
- Updated `paplay` press/release cues — preview played successfully and heard by the user
- Private GitHub repository and matching local/remote commit — confirmed at publication

## Known limitations / next steps

- `global-hotkey` supports X11 only. Wayland requires a compositor binding
  invoking `hydra --toggle`; it does not register an X11 hotkey.
- Recording uses the system ALSA default input device. Device selection is not
  configurable yet.
- Transcription and insertion are processed before the next toggle. Shortcut
  events received during that time are queued and may start recording afterward.
- There is no cancel command. The second toggle stops recording and submits it.
- Enter is sent automatically after typing; this can submit text in the focused app.
- Groq requests have no explicit timeout or retry policy.
- There is no tray icon, service file, automatic startup, retry handling, or
  configurable audio feedback yet.
- Media pause requires MPRIS support. Terminating the daemon during recording
  does not provide graceful recorder cleanup or media restoration.
