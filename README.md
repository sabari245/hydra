# Hydra

A small Linux push-to-toggle dictation MVP.

## Requirements

- Rust and Cargo
- `arecord` (ALSA utilities)
- `xdotool` for X11, or `wtype` for Wayland
- `paplay` for feedback sounds, and `playerctl` for media pause/resume
- A working ALSA/PulseAudio recording and playback device
- A Groq API key with access to speech-to-text
- An IsoQuant API key for GLM-5.3-Flash transcript cleanup

## Run

```sh
export GROQ_API_KEY='your-new-key'
export ISO_QUANT_API_KEY='your-isoquant-key'
cargo run --release
```

The application reads exported environment variables; it does not automatically
load `.env`. On Wayland, configure the compositor shortcut below first.

Press **Super+Space** to start recording. A high click indicates recording has
started; a low click indicates it has stopped. The WAV is sent to
`whisper-large-v3-turbo`, then cleaned with IsoQuant's `glm-5.3-flash` and pasted into the focused
application and Enter is pressed.

Cleanup removes filler sounds, accidental repeated words and phrases, and false
starts, while preserving meaning, language, and intentional emphasis. The short
system prompt is in `src/cleanup.rs`. Cleanup has a 30-second timeout; on an API
error, Hydra inserts the original transcript. Empty cleaned text is not inserted
and does not send Enter. Both original and cleaned transcripts appear in logs.

Test cleanup without recording or typing into another application:

```sh
printf '%s' 'Um, I I need to call, uh, call Sam tomorrow.' | ./target/release/hydra --cleanup
```

The prompt follows the conservative cleanup approach used in
[Fluent](https://github.com/inhaq/fluent) and this
[community dictation prompt](https://gist.github.com/travisjhicks/c11d6e85a912c6c436daca3c7afe12b2).

To inspect the speech models available to the configured Groq account:

```sh
cargo run --release -- --models
```

Set `GROQ_MODEL=whisper-large-v3` for the higher-accuracy model.

On Wayland, text is injected with `wtype`. On X11, the transcript is placed
on the clipboard and pasted with `xdotool`.

## Diagnostic logs

Hydra appends timestamped events to `logs/hydra.log` and prints them to stderr.
The log records startup, hotkey delivery, recording, API status, recognized
transcripts, typing, and failures. Transcripts contain your dictated text.
The directory is private to your user and ignored by Git. API keys and raw
audio are not logged.

```sh
tail -f logs/hydra.log
```

A `hotkey_registered` event followed by no `hotkey_event` when you press the
shortcut means the recording flow never received the shortcut. The current
hotkey backend supports X11 only, including when running under XWayland.

## Niri / Wayland shortcut

On Wayland, the compositor invokes Hydra's local toggle command. Add this
inside Niri's `binds` block, using your checkout's absolute binary path:

```kdl
Mod+Space repeat=false hotkey-overlay-title="Hydra Dictation" {
    spawn "/home/sabari/code/hydra/target/release/hydra" "--toggle";
}
```

Start the daemon with your Groq environment exported. `hydra --toggle` sends
a command to the running daemon through `$XDG_RUNTIME_DIR/hydra.sock`; it
does not need API credentials. Wayland uses this input path, while X11 uses
the global hotkey backend. Niri reloads saved configuration automatically.

## Sounds and media

Wayland output uses `wtype` with its default zero typing delay, followed
by Enter. Start and stop play short mechanical press and release sounds.
`playerctl` pauses players
that are playing before recording and resumes those still paused when
recording stops. Already paused players stay paused. Players must expose
MPRIS controls; arbitrary audio streams cannot be paused this way.

Feedback sounds require `paplay`, using your desktop default output. The
140 ms WAV cues are in `assets/`; regenerate them with
`python scripts/generate-feedback.py`. Playback completion and errors are
recorded in the diagnostic log.
