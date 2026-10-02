# Hydra STT

Push-to-toggle speech-to-text dictation for Wayland on x86_64 Linux. Press a key, speak, press
it again: the recording is transcribed with Groq Whisper, routed to a
processing profile by IsoQuant's System One decision model, rewritten by that
profile with `glm-5.3-flash`, and typed into the focused application. Or,
when you ask it to do something ("open the browser and go to GitHub", "fill
out this form"), an agent does it on your desktop.

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
a `.deb` package and a `.tar.gz` archive for x86_64 (glibc 2.31+):

```sh
sudo apt install ./hydra-stt_amd64.deb
```

### Runtime requirements

- `arecord` (alsa-utils) for recording
- A Wayland session, and `wtype` for typing
- `paplay` (pulseaudio-utils, works with PipeWire) for feedback sounds
- `playerctl` for pausing media while recording (optional)
- For the computer agent: a wlroots-style compositor with screencopy and
  virtual pointer support (Niri, Sway, Hyprland), `wl-clipboard`, and
  `notify-send` (optional)
- For the window: EGL and xkbcommon (`libegl1`, `libxkbcommon0`)
- A Groq API key, and an IsoQuant API key unless `[isoquant] enabled = false`

## Set up

Open **Hydra STT** from your applications menu, or run `hydra-stt`. The
window is where you control Hydra:

- **Home** shows whether Hydra is running in the background, with Start,
  Stop and Restart, a switch to start it at login, and the shortcut command
  to bind in your compositor.
- **API keys** holds your Groq and IsoQuant keys, and tests the Groq key.
- The other pages cover every option: speech model, microphone, sounds,
  profiles, the computer agent, and output.

The dictation itself runs as a background process (`hydra-stt --daemon`),
so closing the window does not stop it. Start at login uses a systemd user
service (`~/.config/systemd/user/hydra-stt.service`, started with your
graphical session). Without systemd, add `hydra-stt --daemon` to your
compositor's startup commands.

## Configure

The window edits `~/.config/hydra-stt/config.toml` in place and keeps the
file's comments. The daemon reads the file when it starts, so after saving,
press **Restart Hydra to apply**.

You can also edit the file by hand. It is created on first run with every
option commented (see [`config.example.toml`](config.example.toml)).

| Section       | Options                                                        |
| ------------- | -------------------------------------------------------------- |
| `[groq]`      | `api_key`, `model`                                             |
| `[isoquant]`  | `enabled`, `api_key`, `api_url`, `timeout_secs`                |
| `[router]`    | `model`, `instructions`, `min_confidence`                      |
| `[profiles.NAME]` | `description`, `model`, `prompt`, `tools`                  |
| `[computer]`  | `max_steps`, `command_timeout_secs`, `allow_privileged`, `screenshot_max_size`, `notify` |
| `[history]`   | `enabled`, `entries`                                           |
| `[recording]` | `device` (ALSA device for `arecord -D`; see `arecord -L`)      |
| `[sounds]`    | `enabled`, `volume` (0-100), `press`, `release` (custom WAVs)  |
| `[media]`     | `pause_while_recording`                                        |
| `[output]`    | `press_enter` (default off), `newlines` (`"space"` or `"shift_enter"`) |
| `[logging]`   | `dir`                                                          |

**API keys.** The file is created with permissions `600` in a `700`
directory, and Hydra STT logs a warning if other users can read it. That is
the same protection `gh`, `aws` and similar CLIs use. To keep keys out of the
file, leave `api_key` empty and export `GROQ_API_KEY` and `ISO_QUANT_API_KEY`
instead; environment variables always override the file. `GROQ_MODEL` and
`HYDRA_STT_CONFIG` (alternate config path) are also honored.

## Use

Press your shortcut to start recording and again to stop. A high click means recording started; a low click means it stopped. Media
players that were playing are paused while recording and resumed afterwards.
If processing fails, the raw transcript is typed instead. Line breaks are
joined into one line by default, because a typed newline is a Return key press
that would submit partial text; `newlines = "shift_enter"` keeps them. Empty output is
not typed and does not send Enter.

Commands:

```sh
hydra-stt                 # open the window
hydra-stt --daemon        # run dictation in the foreground (what runs in the background)
hydra-stt --toggle        # start/stop recording in the running daemon
hydra-stt --stop          # stop the running daemon
hydra-stt --models        # list Groq speech models on your account
hydra-stt --config-path   # print the config file location
printf '%s' 'Um, I I need to call, uh, call Sam.' | hydra-stt --process
printf '%s' 'Edit A, actually no, edit B.' | hydra-stt --process prompt
printf '%s' "What's on my second monitor?" | hydra-stt --process computer
```

## Profiles

A profile is a system prompt that rewrites the transcript, or, with tools, an
agent that acts on it. Three are built in:

- **`default`**: cleans up dictation (fillers, false starts, punctuation, code
  paths and identifiers) and keeps your wording.
- **`prompt`**: treats the speech as a prompt for an AI assistant and keeps
  only your final intent. "Edit file A, actually no, change file B" becomes
  "Change file B", and asides like "wait, I got a call" are dropped.
- **`computer`**: an agent that carries out the request on your desktop and
  types nothing. See [Computer agent](#computer-agent).

With more than one profile, each transcript goes to IsoQuant System One
(`/v1/systemone`), which picks a profile from the `description` of each. The
default profile runs in parallel, so routing adds no latency when it wins.
A choice below `[router] min_confidence`, or any routing error, falls back to
`default`. With only `[profiles.default]`, System One is never called.

Add your own by giving it a description (what System One matches against)
and a prompt:

```toml
[profiles.email]
description = "An email or a reply to one."
prompt = "Rewrite the transcript as a concise, friendly email body. Return only the email."
```

The `route_decided` log event records each choice with its probabilities.

### Memory and history

Each profile is shown its last `[history] entries` requests (input, output,
the routing probabilities, and an agent's actions), stored per profile in
`~/.local/share/hydra-stt/history/NAME.jsonl`, so it can resolve "do that
again" or keep spellings consistent. Long-term memory is a directory of small
text files in `~/.local/share/hydra-stt/memories` that the agent maintains
when you say "remember that..." or state a lasting preference. Its contents
are added to every profile's prompt, so dictation cleanup also learns names
and spellings. Both are plain files you can read, edit, or delete.

## Computer agent

The `computer` profile runs a tool-calling loop: look at a screenshot, act,
look again, and repeat until done, then show a summary as a notification. A
press of your toggle key while it works cancels it and kills its commands. Its
tools are ported from Anthropic's MIT-licensed reference code rather than
written from scratch:

| Tool       | Does                                                         | Ported from |
| ---------- | ------------------------------------------------------------ | ----------- |
| `computer` | screenshot any monitor, click, drag, scroll, type, keys, clipboard, zoom | [`computer.py`](https://github.com/anthropics/anthropic-quickstarts/blob/main/computer-use-best-practices/computer_use/tools/computer.py) |
| `bash`     | run a command (merged output, timeout, output cap)           | [`shell.py`](https://github.com/anthropics/anthropic-quickstarts/blob/main/computer-use-best-practices/computer_use/tools/shell.py) |
| `editor`   | view, create, str_replace, insert on text files              | [`editor.py`](https://github.com/anthropics/anthropic-quickstarts/blob/main/computer-use-best-practices/computer_use/tools/editor.py) |
| `memory`   | the `/memories` directory: view, create, str_replace, insert, delete, rename | [`memory_tool.py`](https://github.com/anthropics/claude-cookbooks/blob/main/tool_use/memory_tool.py) |

Screenshots use the wlr-screencopy protocol, the mouse uses the
wlr-virtual-pointer protocol (exact absolute positions on the chosen monitor,
no uinput daemon), and the keyboard uses `wtype`.

**Safety.** The agent runs as you, without a sandbox: it can run any command,
change any file you can, and see everything on your screens, and screenshots
are sent to IsoQuant. Its prompt forbids destructive or irreversible actions
unless you clearly asked for them, `sudo` and similar tools are refused unless
`allow_privileged = true`, and steps and command time are capped, but these
are guard rails rather than a security boundary. Remove `[profiles.computer]`
to turn it off.

### Toggle key (Niri example)

Wayland does not allow global hotkeys, so the compositor runs
`hydra-stt --toggle`, which signals the daemon through
`$XDG_RUNTIME_DIR/hydra-stt.sock`. In Niri's `binds` block:

```kdl
Mod+Space repeat=false hotkey-overlay-title="Hydra STT" {
    spawn "hydra-stt" "--toggle";
}
```

Use the absolute path (e.g. `~/.local/bin/hydra-stt`) if that directory is not
on the compositor's `PATH`.

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
cargo run                 # the window, from a debug build
cargo run -- --daemon     # the daemon in the foreground, logs to ./logs
cargo run -- --process < transcript.txt
```

Only one daemon can run at a time, so stop the installed one first
(`hydra-stt --stop`) to try a debug build.

The built-in profile prompts are in `src/profiles.rs`. The default prompt follows the conservative approach
of [Fluent](https://github.com/inhaq/fluent) and this
[community dictation prompt](https://gist.github.com/travisjhicks/c11d6e85a912c6c436daca3c7afe12b2).
The feedback cues in `assets/` are embedded in the binary; regenerate them with
`python scripts/generate-feedback.py`.

### Releasing

Day-to-day work happens on the `dev` branch; `main` only receives merges for
releases. CI (`.github/workflows/ci.yml`) runs fmt, clippy and tests on pushes
to both. To publish a release, merge `dev` into `main`, bump `version` in
`Cargo.toml`, then:

```sh
git tag v0.1.0 && git push origin v0.1.0
```

`.github/workflows/release.yml` builds the x86_64 binary with
`cargo-zigbuild` against glibc 2.31, packages `.tar.gz` and `.deb` files with
checksums, and attaches them and `install.sh` to a GitHub release.

## License

[MIT](LICENSE)
