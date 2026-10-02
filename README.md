# Hydra STT

Push-to-toggle speech-to-text dictation for Linux. Press a key, speak, press
it again: the recording is transcribed with Groq Whisper, optionally cleaned
up with IsoQuant's `glm-5.3-flash`, and typed into the focused application,
followed by Enter.

## Install

```sh
curl -fsSL https://github.com/sabari245/hydra/releases/latest/download/install.sh | sh
```

The script installs `hydra-stt` to `~/.local/bin`, verifies its SHA-256
checksum, and reports missing runtime tools. Options:

```sh
sh install.sh --version v0.1.0     # a specific release
sh install.sh --prefix /usr/local  # install to /usr/local/bin (run with sudo)
sh install.sh --uninstall
```

`HYDRA_STT_BASE_URL` downloads the release files from another location, such
as your own website, instead of GitHub.

Each [release](https://github.com/sabari245/hydra/releases) also provides
`.deb` packages and `.tar.gz` archives for x86_64 and aarch64 (glibc 2.31+):

```sh
sudo apt install ./hydra-stt_amd64.deb
```

### Runtime requirements

- `arecord` (alsa-utils) for recording
- `wtype` on Wayland, or `xdotool` on X11, for typing
- `paplay` (pulseaudio-utils, works with PipeWire) for feedback sounds
- `playerctl` for pausing media while recording (optional)
- A Groq API key; an IsoQuant API key if cleanup is enabled

## Configure

Run `hydra-stt` once. It creates `~/.config/hydra-stt/config.toml` with
every option commented (see [`config.example.toml`](config.example.toml)),
then exits asking for your API keys.

| Section       | Options                                                        |
| ------------- | -------------------------------------------------------------- |
| `[groq]`      | `api_key`, `model`                                             |
| `[cleanup]`   | `enabled`, `api_key`, `api_url`, `model`, `timeout_secs`, `prompt` |
| `[hotkey]`    | `binding` (X11 only, e.g. `"Super+Space"`, `"Ctrl+Alt+KeyD"`)  |
| `[recording]` | `device` (ALSA device for `arecord -D`; see `arecord -L`)      |
| `[sounds]`    | `enabled`, `volume` (0-100), `press`, `release` (custom WAVs)  |
| `[media]`     | `pause_while_recording`                                        |
| `[output]`    | `press_enter`                                                  |
| `[logging]`   | `dir`                                                          |

**API keys.** The file is created with permissions `600` in a `700`
directory, and Hydra STT logs a warning if other users can read it. That is
the same protection `gh`, `aws` and similar CLIs use. To keep keys out of the
file, leave `api_key` empty and export `GROQ_API_KEY` and `ISO_QUANT_API_KEY`
instead; environment variables always override the file. `GROQ_MODEL` and
`HYDRA_STT_CONFIG` (alternate config path) are also honored.

## Run

```sh
hydra-stt
```

A high click means recording started; a low click means it stopped. Media
players that were playing are paused while recording and resumed afterwards.
If cleanup fails, the raw transcript is typed instead. Empty cleaned text is
not typed and does not send Enter. SIGINT or SIGTERM stop the daemon cleanly.

Other commands:

```sh
hydra-stt --toggle        # start/stop recording in the running daemon
hydra-stt --models        # list Groq speech models on your account
hydra-stt --config-path   # print the config file location
printf '%s' 'Um, I I need to call, uh, call Sam.' | hydra-stt --cleanup
```

### Wayland (Niri example)

Wayland does not allow global hotkeys, so the compositor runs
`hydra-stt --toggle`, which signals the daemon through
`$XDG_RUNTIME_DIR/hydra-stt.sock`. In Niri's `binds` block:

```kdl
Mod+Space repeat=false hotkey-overlay-title="Hydra STT" {
    spawn "hydra-stt" "--toggle";
}
```

Use the absolute path (e.g. `~/.local/bin/hydra-stt`) if that directory is not
on the compositor's `PATH`. On X11, `[hotkey] binding` is registered directly.

## Logs

Installed builds append to `~/.local/state/hydra-stt/hydra.log`
(`$XDG_STATE_HOME` is respected, or set `[logging] dir`); debug builds from a
checkout write to `logs/` in the repository. Events are also printed to
stderr. Logs contain your dictated text but never API keys or audio.

```sh
tail -f ~/.local/state/hydra-stt/hydra.log
```

## Development

```sh
cargo run                 # debug build, logs to ./logs
cargo run -- --cleanup < transcript.txt
```

The cleanup prompt is in `src/cleanup.rs`, and it follows the conservative approach
of [Fluent](https://github.com/inhaq/fluent) and this
[community dictation prompt](https://gist.github.com/travisjhicks/c11d6e85a912c6c436daca3c7afe12b2).
The feedback cues in `assets/` are embedded in the binary; regenerate them with
`python scripts/generate-feedback.py`.

### Releasing

CI (`.github/workflows/ci.yml`) runs fmt, clippy and tests on every push.
To publish a release, bump `version` in `Cargo.toml`, then:

```sh
git tag v0.1.0 && git push origin v0.1.0
```

`.github/workflows/release.yml` cross-builds x86_64 and aarch64 binaries with
`cargo-zigbuild` against glibc 2.31, packages `.tar.gz` and `.deb` files with
checksums, and attaches them and `install.sh` to a GitHub release.

## License

[MIT](LICENSE)
