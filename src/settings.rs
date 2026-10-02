//! The Hydra STT window, opened by running `hydra-stt`: starts and stops the
//! background daemon and edits `config.toml`. Saving rewrites only the
//! values, so comments in the file survive. The daemon reads the file at
//! startup, so changes apply after a restart.

use crate::{
    config::{self, Config, Newlines, Profile},
    profiles, service,
};
use anyhow::Result;
use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontFamily, FontId, Layout, Margin, RichText,
    Sense, Stroke, StrokeKind, TextStyle, Theme, vec2,
};
use std::{
    env,
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// How long a freshly started daemon gets to open its control socket.
const START_GRACE: Duration = Duration::from_secs(4);
/// Settings pages stay readable on wide windows.
const CONTENT_WIDTH: f32 = 760.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Home,
    Keys,
    Speech,
    Processing,
    Profiles,
    Output,
    Agent,
}

const PAGES: [(Page, &str); 6] = [
    (Page::Home, "Home"),
    (Page::Keys, "API keys"),
    (Page::Speech, "Speech & sound"),
    (Page::Processing, "Processing"),
    (Page::Profiles, "Profiles"),
    (Page::Output, "Typing & logs"),
];

enum Status {
    Info(String),
    Error(String),
}

struct Settings {
    path: PathBuf,
    /// What is in the file, to tell whether there are unsaved changes.
    saved: Config,
    config: Config,
    load_error: Option<String>,
    page: Page,
    show_groq_key: bool,
    show_isoquant_key: bool,
    status: Option<Status>,
    groq_test: Option<mpsc::Receiver<Result<Vec<String>, String>>>,
    groq_result: Option<Result<String, String>>,
    groq_models: Vec<String>,
    new_profile: String,
    confirm_close: bool,
    runtime: tokio::runtime::Handle,
    /// Whether the daemon was running at the last check.
    running: bool,
    checked: Instant,
    /// A start, stop or restart in progress on another thread.
    action: Option<mpsc::Receiver<Result<(), String>>>,
    /// When the daemon was last started, to report it if it exits at once.
    started: Option<(Instant, u128)>,
    /// None when there is no systemd user manager.
    autostart: Option<bool>,
    /// Saved since the running daemon started.
    restart_needed: bool,
}

pub fn run() -> Result<()> {
    let path = config::path()?;
    config::ensure_exists(&path)?;
    let runtime = tokio::runtime::Handle::current();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Hydra STT")
            .with_app_id(config::APP_NAME)
            .with_inner_size([1000.0, 700.0])
            .with_min_inner_size([720.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Hydra STT",
        options,
        Box::new(move |creation| {
            theme::install(&creation.egui_ctx);
            Ok(Box::new(Settings::new(path, runtime)))
        }),
    )
    .map_err(|error| {
        anyhow::anyhow!(
            "could not open the Hydra STT window: {error}\n\
             Run `hydra-stt --daemon` to run dictation without the window."
        )
    })
}

impl Settings {
    fn new(path: PathBuf, runtime: tokio::runtime::Handle) -> Self {
        let mut settings = Self {
            path,
            saved: Config::default(),
            config: Config::default(),
            load_error: None,
            page: Page::Home,
            show_groq_key: false,
            show_isoquant_key: false,
            status: None,
            groq_test: None,
            groq_result: None,
            groq_models: Vec::new(),
            new_profile: String::new(),
            confirm_close: false,
            runtime,
            running: service::is_running(),
            checked: Instant::now(),
            action: None,
            started: None,
            autostart: service::can_autostart().then(service::autostart_enabled),
            restart_needed: false,
        };
        settings.reload();
        settings
    }

    fn reload(&mut self) {
        match Config::read(&self.path) {
            Ok(config) => {
                self.saved = config.clone();
                self.config = config;
                self.load_error = None;
            }
            Err(error) => self.load_error = Some(format!("{error:#}")),
        }
    }

    fn dirty(&self) -> bool {
        self.load_error.is_none() && self.config != self.saved
    }

    fn save(&mut self) {
        if let Err(error) = self.check() {
            self.status = Some(Status::Error(error));
            return;
        }
        match self.config.save(&self.path) {
            Ok(()) => {
                self.saved = self.config.clone();
                self.restart_needed = self.running;
                self.status = (!self.running).then(|| Status::Info("Changes saved.".to_owned()));
            }
            Err(error) => self.status = Some(Status::Error(format!("{error:#}"))),
        }
    }

    /// Errors that would stop the daemon from starting.
    fn check(&self) -> Result<(), String> {
        self.config.validate().map_err(|error| error.to_string())?;
        if self.config.isoquant.enabled {
            profiles::check(&self.config, &self.path).map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    /// Missing keys. Not errors, since the daemon may get them from its
    /// environment.
    fn warnings(&self) -> Vec<&'static str> {
        let mut warnings = Vec::new();
        if self.config.groq.api_key.trim().is_empty() && env::var_os("GROQ_API_KEY").is_none() {
            warnings.push("Add a Groq API key. Hydra needs it to transcribe your speech.");
        }
        if self.config.isoquant.enabled
            && self.config.isoquant.api_key.trim().is_empty()
            && env::var_os("ISO_QUANT_API_KEY").is_none()
        {
            warnings.push("Add an IsoQuant API key, or turn off processing with IsoQuant.");
        }
        warnings
    }

    fn test_groq_key(&mut self, ctx: &egui::Context) {
        let key = match self.config.groq.api_key.trim() {
            "" => env::var("GROQ_API_KEY").unwrap_or_default(),
            key => key.to_owned(),
        };
        if key.is_empty() {
            self.groq_result = Some(Err("Enter a key to test it.".to_owned()));
            return;
        }
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        self.runtime.spawn(async move {
            let result = crate::speech_models(&key)
                .await
                .map_err(|error| format!("{error:#}"));
            let _ = sender.send(result);
            ctx.request_repaint();
        });
        self.groq_test = Some(receiver);
        self.groq_result = None;
    }

    fn poll_groq_test(&mut self) {
        let Some(result) = self
            .groq_test
            .as_ref()
            .and_then(|test| test.try_recv().ok())
        else {
            return;
        };
        self.groq_test = None;
        self.groq_result = Some(match result {
            Ok(models) => {
                let message = format!("The key works. Speech models: {}.", models.join(", "));
                self.groq_models = models;
                Ok(message)
            }
            Err(error) => Err(error),
        });
    }

    /// Runs a start, stop or restart off the UI thread.
    fn run_action(
        &mut self,
        ctx: &egui::Context,
        starts: bool,
        action: fn() -> anyhow::Result<()>,
    ) {
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        thread::spawn(move || {
            let _ = sender.send(action().map_err(|error| format!("{error:#}")));
            ctx.request_repaint();
        });
        self.action = Some(receiver);
        self.status = None;
        if starts {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            self.started = Some((Instant::now(), now));
            self.restart_needed = false;
        } else {
            self.started = None;
        }
    }

    /// Polls the daemon state about once a second.
    fn poll_daemon(&mut self, ctx: &egui::Context) {
        if let Some(result) = self
            .action
            .as_ref()
            .and_then(|action| action.try_recv().ok())
        {
            self.action = None;
            if let Err(error) = result {
                self.status = Some(Status::Error(error));
                self.started = None;
            }
            self.checked = Instant::now() - Duration::from_secs(1);
        }
        if self.checked.elapsed() >= Duration::from_secs(1) {
            self.running = service::is_running();
            self.checked = Instant::now();
            if let Some((at, since_ms)) = self.started {
                if self.running {
                    self.started = None;
                } else if at.elapsed() > START_GRACE && self.action.is_none() {
                    self.started = None;
                    let reason = self
                        .saved
                        .log_dir()
                        .ok()
                        .and_then(|directory| service::last_error(&directory, since_ms))
                        .unwrap_or_else(|| {
                            "run hydra-stt --daemon in a terminal to see why".to_owned()
                        });
                    self.status = Some(Status::Error(format!(
                        "Hydra stopped right after starting: {reason}"
                    )));
                }
            }
        }
        ctx.request_repaint_after(Duration::from_secs(1));
    }

    fn state(&self) -> State {
        if self.action.is_some() || (self.started.is_some() && !self.running) {
            State::Working
        } else if self.running {
            State::Running
        } else {
            State::Stopped
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Running,
    Working,
    Stopped,
}

impl State {
    fn label(self) -> &'static str {
        match self {
            Self::Running => "Running",
            Self::Working => "Working…",
            Self::Stopped => "Off",
        }
    }

    fn color(self, p: &theme::Palette) -> Color32 {
        match self {
            Self::Running => p.accent,
            Self::Working => p.warn,
            Self::Stopped => p.mist,
        }
    }
}

impl eframe::App for Settings {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_groq_test();
        let ctx = ui.ctx().clone();
        self.poll_daemon(&ctx);
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::S))
            && self.dirty()
        {
            self.save();
        }
        if ctx.input(|input| input.viewport().close_requested()) && self.dirty() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.confirm_close = true;
        }
        let p = theme::Palette::of(ui);

        egui::Panel::left("pages")
            .resizable(false)
            .exact_size(220.0)
            .show_separator_line(false)
            .frame(
                egui::Frame::new()
                    .fill(p.sidebar)
                    .inner_margin(Margin::symmetric(14, 18)),
            )
            .show(ui, |ui| self.sidebar(ui));

        let mut bar_visible = self.dirty()
            || self.status.is_some()
            || (self.restart_needed && self.running && self.action.is_none());
        egui::Panel::bottom("actions")
            .show_separator_line(false)
            .frame(
                egui::Frame::new()
                    .fill(p.sidebar)
                    .stroke(Stroke::new(1.0, p.line))
                    .inner_margin(Margin::symmetric(28, 12)),
            )
            .show_collapsible(ui, &mut bar_visible, |ui| self.action_bar(ui));

        egui::CentralPanel::default_margins()
            .frame(egui::Frame::new().fill(p.bg))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        let mut column = ui.available_rect_before_wrap().shrink2(vec2(36.0, 0.0));
                        let extra = column.width() - CONTENT_WIDTH;
                        if extra > 0.0 {
                            column = column.shrink2(vec2(extra / 2.0, 0.0));
                        }
                        ui.scope_builder(egui::UiBuilder::new().max_rect(column), |ui| {
                            egui::Frame::new()
                                .inner_margin(Margin::symmetric(0, 30))
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    if let Some(error) = self.load_error.clone() {
                                        self.load_error_page(ui, &error);
                                        return;
                                    }
                                    match self.page {
                                        Page::Home => self.home_page(ui),
                                        Page::Keys => self.keys_page(ui),
                                        Page::Speech => self.speech_page(ui),
                                        Page::Processing => self.processing_page(ui),
                                        Page::Profiles => self.profiles_page(ui),
                                        Page::Output => self.output_page(ui),
                                        Page::Agent => agent_page(ui),
                                    }
                                });
                        });
                    });
            });

        if self.confirm_close {
            self.close_dialog(&ctx);
        }
    }
}

// Window chrome: sidebar, action bar, dialogs.
impl Settings {
    fn sidebar(&mut self, ui: &mut egui::Ui) {
        let p = theme::Palette::of(ui);
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            ui.label(RichText::new("Hydra").font(theme::bold(22.0)).color(p.ink));
        });
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            let state = self.state();
            dot(ui, state.color(&p), 7.0);
            ui.label(RichText::new(state.label()).small().color(p.mist));
        });
        ui.add_space(18.0);
        for (page, label) in PAGES {
            if nav_item(ui, label, self.page == page, None).clicked() {
                self.page = page;
            }
        }
        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            ui.horizontal(|ui| {
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION")))
                        .small()
                        .color(p.mist),
                );
            });
            ui.add_space(10.0);
            let agent = nav_item(ui, "Computer agent", self.page == Page::Agent, Some("Soon"));
            if agent.clicked() {
                self.page = Page::Agent;
            }
            ui.horizontal(|ui| {
                ui.add_space(6.0);
                ui.label(RichText::new("Upcoming").small().color(p.mist));
            });
        });
    }

    fn action_bar(&mut self, ui: &mut egui::Ui) {
        let p = theme::Palette::of(ui);
        ui.horizontal(|ui| {
            let dirty = self.dirty();
            let restart = !dirty && self.restart_needed && self.running && self.action.is_none();
            let message = match (&self.status, dirty, restart) {
                (Some(Status::Error(message)), ..) => Some((message.clone(), p.error)),
                (_, true, _) => Some(("You have unsaved changes.".to_owned(), p.ink)),
                (_, _, true) => Some((
                    "Saved. Hydra uses the old settings until it restarts.".to_owned(),
                    p.ink,
                )),
                (Some(Status::Info(message)), ..) => Some((message.clone(), p.ink)),
                (None, ..) => None,
            };
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if dirty {
                    if primary_button(ui, "Save changes")
                        .on_hover_text("Ctrl+S")
                        .clicked()
                    {
                        self.save();
                    }
                    if ui.button("Discard").clicked() {
                        self.config = self.saved.clone();
                        self.status = None;
                    }
                } else if restart {
                    if primary_button(ui, "Restart Hydra").clicked() {
                        self.run_action(ui.ctx(), true, service::restart);
                    }
                    if ui.button("Later").clicked() {
                        self.restart_needed = false;
                    }
                } else if ui.button("Dismiss").clicked() {
                    self.status = None;
                }
                if let Some((message, color)) = message {
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        ui.add(egui::Label::new(RichText::new(message).color(color)).wrap());
                    });
                }
            });
        });
    }

    fn close_dialog(&mut self, ctx: &egui::Context) {
        egui::Modal::new(egui::Id::new("confirm_close")).show(ctx, |ui| {
            ui.set_width(340.0);
            ui.label(RichText::new("Save your changes?").font(theme::bold(18.0)));
            ui.add_space(4.0);
            ui.label("Closing now discards them. Hydra keeps running either way.");
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                if primary_button(ui, "Save and close").clicked() {
                    self.save();
                    if !self.dirty() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    self.confirm_close = false;
                }
                if ui.button("Discard and close").clicked() {
                    self.config = self.saved.clone();
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                if ui.button("Cancel").clicked() {
                    self.confirm_close = false;
                }
            });
        });
    }

    fn load_error_page(&mut self, ui: &mut egui::Ui, error: &str) {
        let p = theme::Palette::of(ui);
        page_title(ui, "The settings file has an error", None);
        card(ui, |ui| {
            ui.label(RichText::new(error).color(p.error));
            ui.add_space(8.0);
            ui.label(format!(
                "Fix {} in a text editor, then reload it.",
                self.path.display()
            ));
            ui.add_space(10.0);
            if primary_button(ui, "Reload").clicked() {
                self.reload();
            }
        });
    }
}

// Pages.
impl Settings {
    fn home_page(&mut self, ui: &mut egui::Ui) {
        let p = theme::Palette::of(ui);
        let state = self.state();
        let (title, body) = match state {
            State::Running => (
                "Listening for your shortcut",
                "Press it to start recording, and again to stop. The text is typed \
                 wherever your cursor is. Closing this window leaves Hydra running.",
            ),
            State::Working => ("Just a moment", "Hydra is starting or stopping."),
            State::Stopped => (
                "Hydra is off",
                "Start it to dictate into any app. It runs in the background, so you \
                 can close this window afterwards.",
            ),
        };

        egui::Frame::new()
            .fill(p.card)
            .stroke(Stroke::new(1.0, p.line))
            .corner_radius(16)
            .inner_margin(Margin::symmetric(28, 26))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let time = ui.input(|input| input.time);
                voice_wave(ui, state, time);
                if state != State::Stopped {
                    ui.ctx().request_repaint_after(Duration::from_millis(33));
                }
                ui.add_space(18.0);
                ui.label(RichText::new(title).font(theme::bold(26.0)).color(p.ink));
                ui.add_space(4.0);
                ui.label(RichText::new(body).color(p.mist));
                ui.add_space(18.0);
                ui.horizontal(|ui| {
                    let idle = self.action.is_none() && self.started.is_none();
                    let ctx = ui.ctx().clone();
                    match state {
                        State::Running => {
                            if ui.add_enabled(idle, egui::Button::new("Stop")).clicked() {
                                self.run_action(&ctx, false, service::stop);
                            }
                            if ui.add_enabled(idle, egui::Button::new("Restart")).clicked() {
                                self.run_action(&ctx, true, service::restart);
                            }
                        }
                        State::Working => {
                            ui.spinner();
                        }
                        State::Stopped => {
                            let ready = idle && !self.dirty();
                            let start =
                                ui.add_enabled_ui(ready, |ui| primary_button(ui, "Start Hydra"));
                            if self.dirty() {
                                start
                                    .response
                                    .on_disabled_hover_text("Save your changes first");
                            } else if start.inner.clicked() {
                                self.run_action(&ctx, true, service::start);
                            }
                        }
                    }
                });
            });
        ui.add_space(14.0);

        let warnings = self.warnings();
        if !warnings.is_empty() {
            card(ui, |ui| {
                for warning in &warnings {
                    ui.horizontal(|ui| {
                        dot(ui, p.warn, 7.0);
                        ui.label(*warning);
                    });
                }
                ui.add_space(6.0);
                if ui.button("Go to API keys").clicked() {
                    self.page = Page::Keys;
                }
            });
            ui.add_space(14.0);
        }

        card(ui, |ui| match self.autostart {
            Some(mut enabled) => {
                row(
                    ui,
                    "Start at login",
                    "Run Hydra in the background whenever you log in.",
                    |ui| {
                        if toggle(ui, &mut enabled).changed() {
                            match service::set_autostart(enabled) {
                                Ok(()) => self.autostart = Some(service::autostart_enabled()),
                                Err(error) => {
                                    self.status = Some(Status::Error(format!("{error:#}")));
                                }
                            }
                        }
                    },
                );
            }
            None => row(
                ui,
                "Start at login",
                "No systemd user session was found. Add hydra-stt --daemon to your \
                 compositor's startup commands instead.",
                |_| {},
            ),
        });

        section_title(
            ui,
            "Shortcut",
            "Bind this command to a key in your compositor, such as Super+Space.",
        );
        let command = service::toggle_command();
        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add(egui::Label::new(RichText::new(&command).monospace()).wrap());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Copy").clicked() {
                        ui.ctx().copy_text(command.clone());
                    }
                });
            });
            divider(ui);
            ui.label(
                RichText::new("In Niri's binds block:")
                    .small()
                    .color(p.mist),
            );
            ui.label(
                RichText::new(format!(
                    "Mod+Space {{ spawn \"{}\" \"--toggle\"; }}",
                    command.trim_end_matches(" --toggle")
                ))
                .monospace()
                .color(p.mist),
            );
        });
    }

    fn keys_page(&mut self, ui: &mut egui::Ui) {
        let p = theme::Palette::of(ui);
        page_title(
            ui,
            "API keys",
            Some(
                "Keys are saved in your settings file, which only your user can read. \
                 Environment variables override them.",
            ),
        );

        section_title(ui, "Groq", "Transcribes your speech. Required.");
        card(ui, |ui| {
            secret(
                ui,
                &mut self.config.groq.api_key,
                &mut self.show_groq_key,
                "gsk_…",
            );
            env_note(ui, "GROQ_API_KEY");
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let testing = self.groq_test.is_some();
                if ui
                    .add_enabled(!testing, egui::Button::new("Test key"))
                    .clicked()
                {
                    self.test_groq_key(ui.ctx());
                }
                if testing {
                    ui.spinner();
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.hyperlink_to("Get a key", "https://console.groq.com/keys");
                });
            });
            match &self.groq_result {
                Some(Ok(message)) => {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        dot(ui, p.accent, 7.0);
                        ui.add(egui::Label::new(message.as_str()).wrap());
                    });
                }
                Some(Err(message)) => {
                    ui.add_space(4.0);
                    ui.add(egui::Label::new(RichText::new(message).color(p.error)).wrap());
                }
                None => {}
            }
        });

        section_title(
            ui,
            "IsoQuant",
            "Picks a profile for each recording and rewrites the text.",
        );
        card(ui, |ui| {
            let isoquant = &mut self.config.isoquant;
            ui.add_enabled_ui(isoquant.enabled, |ui| {
                secret(ui, &mut isoquant.api_key, &mut self.show_isoquant_key, "");
                env_note(ui, "ISO_QUANT_API_KEY");
            });
            divider(ui);
            row(
                ui,
                "Process with IsoQuant",
                "When off, Hydra types exactly what you said.",
                |ui| {
                    toggle(ui, &mut isoquant.enabled);
                },
            );
        });
    }

    fn speech_page(&mut self, ui: &mut egui::Ui) {
        page_title(ui, "Speech & sound", None);

        section_title(ui, "Transcription", "");
        card(ui, |ui| {
            field(
                ui,
                "Speech model",
                if self.groq_models.is_empty() {
                    "The Groq Whisper model. Test your key on the API keys page to list \
                     the models you can use."
                } else {
                    "The Groq Whisper model."
                },
                |ui| {
                    ui.horizontal(|ui| {
                        text(ui, &mut self.config.groq.model, "whisper-large-v3-turbo");
                        if !self.groq_models.is_empty() {
                            egui::ComboBox::from_id_salt("groq_models")
                                .selected_text("Choose")
                                .show_ui(ui, |ui| {
                                    for model in &self.groq_models {
                                        ui.selectable_value(
                                            &mut self.config.groq.model,
                                            model.clone(),
                                            model,
                                        );
                                    }
                                });
                        }
                    });
                    env_note(ui, "GROQ_MODEL");
                },
            );
            divider(ui);
            field(
                ui,
                "Microphone",
                "An ALSA capture device, as listed by arecord -L. Leave empty for the \
                 system default.",
                |ui| text(ui, &mut self.config.recording.device, "System default"),
            );
        });

        section_title(ui, "Sounds", "");
        card(ui, |ui| {
            let sounds = &mut self.config.sounds;
            row(
                ui,
                "Click sounds",
                "Play a click when recording starts and stops.",
                |ui| {
                    toggle(ui, &mut sounds.enabled);
                },
            );
            ui.add_enabled_ui(sounds.enabled, |ui| {
                divider(ui);
                row(ui, "Volume", "", |ui| {
                    ui.add(egui::Slider::new(&mut sounds.volume, 0..=100).suffix("%"));
                });
                divider(ui);
                field(
                    ui,
                    "Custom sounds",
                    "WAV files to play instead of the built-in clicks.",
                    |ui| {
                        optional_path(ui, "Start", &mut sounds.press, "Built-in");
                        optional_path(ui, "Stop", &mut sounds.release, "Built-in");
                    },
                );
            });
            divider(ui);
            row(
                ui,
                "Pause media while recording",
                "Pauses music and videos, and resumes them when you stop.",
                |ui| {
                    toggle(ui, &mut self.config.media.pause_while_recording);
                },
            );
        });
    }

    fn processing_page(&mut self, ui: &mut egui::Ui) {
        page_title(
            ui,
            "Processing",
            Some("How IsoQuant turns what you said into the text that gets typed."),
        );
        card(ui, |ui| {
            row(
                ui,
                "Process with IsoQuant",
                "When off, or when a request fails, Hydra types exactly what you said.",
                |ui| {
                    toggle(ui, &mut self.config.isoquant.enabled);
                },
            );
        });

        let enabled = self.config.isoquant.enabled;
        ui.add_enabled_ui(enabled, |ui| {
            section_title(ui, "Connection", "");
            card(ui, |ui| {
                let isoquant = &mut self.config.isoquant;
                field(ui, "API address", "", |ui| {
                    text(ui, &mut isoquant.api_url, "https://api.isoquant.ai/v1");
                });
                divider(ui);
                row(
                    ui,
                    "Timeout",
                    "How long to wait before typing the raw text.",
                    |ui| {
                        ui.add(
                            egui::DragValue::new(&mut isoquant.timeout_secs)
                                .range(1..=600)
                                .suffix(" s"),
                        );
                    },
                );
            });

            section_title(
                ui,
                "Choosing a profile",
                "With more than one profile, System One picks one for each recording from \
                 the profile descriptions.",
            );
            card(ui, |ui| {
                let router = &mut self.config.router;
                field(ui, "Model", "", |ui| {
                    text(ui, &mut router.model, "isoquant/system-one");
                });
                divider(ui);
                field(ui, "Instructions", "", |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut router.instructions)
                            .desired_rows(2)
                            .margin(vec2(10.0, 8.0))
                            .desired_width(f32::INFINITY),
                    );
                });
                divider(ui);
                row(
                    ui,
                    "Minimum confidence",
                    "Below this, the default profile is used.",
                    |ui| {
                        ui.add(egui::Slider::new(&mut router.min_confidence, 0.0..=1.0));
                    },
                );
            });

            section_title(
                ui,
                "History",
                "Each profile sees its last few recordings, so it can follow \"do that \
                 again\" and keep spellings consistent.",
            );
            card(ui, |ui| {
                let history = &mut self.config.history;
                row(ui, "Keep history", "", |ui| {
                    toggle(ui, &mut history.enabled);
                });
                ui.add_enabled_ui(history.enabled, |ui| {
                    divider(ui);
                    row(ui, "Recordings to remember", "Per profile.", |ui| {
                        ui.add(egui::DragValue::new(&mut history.entries).range(0..=100));
                    });
                });
            });
        });
    }

    fn profiles_page(&mut self, ui: &mut egui::Ui) {
        let p = theme::Palette::of(ui);
        page_title(
            ui,
            "Profiles",
            Some(
                "Each profile rewrites what you said with its own prompt. \"default\" is \
                 required and used whenever no other profile fits. Empty fields on built-in \
                 profiles use the built-in text, shown in grey.",
            ),
        );

        let mut names: Vec<String> = self.config.profiles.0.keys().cloned().collect();
        names.sort_by_key(|name| name != profiles::DEFAULT);
        let mut remove = None;
        for name in &names {
            let builtin = profiles::builtin(name);
            let profile = self.config.profiles.0.get_mut(name).expect("listed above");
            ui.add_space(14.0);
            card(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(name).font(theme::bold(17.0)).color(p.ink));
                    if builtin.is_some() {
                        pill(ui, "Built-in", p.mist);
                    }
                    if name != profiles::DEFAULT {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let button = egui::Button::new(RichText::new("Remove").color(p.error))
                                .frame(false);
                            if ui.add(button).clicked() {
                                remove = Some(name.clone());
                            }
                        });
                    }
                });
                ui.add_space(6.0);
                profile_editor(ui, profile, builtin);
            });
        }
        if let Some(name) = remove {
            self.config.profiles.0.remove(&name);
        }

        section_title(
            ui,
            "Add a profile",
            "Names use letters, digits, - and _. Then give it a description and a prompt.",
        );
        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.new_profile)
                        .hint_text("email")
                        .margin(vec2(10.0, 7.0))
                        .desired_width(260.0),
                );
                let name = self.new_profile.trim().to_owned();
                let valid = !name.is_empty()
                    && name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                    && !self.config.profiles.0.contains_key(&name);
                if ui
                    .add_enabled(valid, egui::Button::new("Add profile"))
                    .clicked()
                {
                    self.config.profiles.0.insert(name, Profile::default());
                    self.new_profile.clear();
                }
            });
        });
    }

    fn output_page(&mut self, ui: &mut egui::Ui) {
        page_title(ui, "Typing & logs", None);
        section_title(ui, "Typing", "");
        card(ui, |ui| {
            let output = &mut self.config.output;
            row(
                ui,
                "Press Enter after typing",
                "Sends chat messages and runs commands right away.",
                |ui| {
                    toggle(ui, &mut output.press_enter);
                },
            );
            divider(ui);
            field(
                ui,
                "Line breaks",
                "A typed line break is a Return key press, which can send a message early.",
                |ui| {
                    ui.radio_value(
                        &mut output.newlines,
                        Newlines::Space,
                        "Join everything into one line",
                    );
                    ui.radio_value(
                        &mut output.newlines,
                        Newlines::ShiftEnter,
                        "Keep line breaks, typed as Shift+Enter",
                    );
                },
            );
        });

        section_title(ui, "Files", "");
        card(ui, |ui| {
            field(
                ui,
                "Log folder",
                "Logs include what you dictated, never your keys or audio.",
                |ui| {
                    optional_path(
                        ui,
                        "",
                        &mut self.config.logging.dir,
                        "~/.local/state/hydra-stt",
                    );
                },
            );
            divider(ui);
            let path = self.path.display().to_string();
            row(ui, "Settings file", &path, |ui| {
                if ui.button("Reload from disk").clicked() {
                    self.reload();
                    self.status = None;
                }
            });
        });
    }
}

fn agent_page(ui: &mut egui::Ui) {
    let p = theme::Palette::of(ui);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("Computer agent")
                .font(theme::bold(28.0))
                .color(p.ink),
        );
        pill(ui, "Upcoming", p.warn);
    });
    ui.add_space(8.0);
    ui.label(
        RichText::new(
            "Ask Hydra to do things on your desktop instead of typing what you said: open \
             an app or a website, fill out a form, run a command, or tell you what is on \
             your screen. It is being reworked and will come back in a later release.",
        )
        .color(p.mist),
    );
}

fn profile_editor(ui: &mut egui::Ui, profile: &mut Profile, builtin: Option<(&str, &str)>) {
    let (builtin_prompt, builtin_description) = builtin.unzip();
    row(ui, "Model", "", |ui| {
        ui.add(
            egui::TextEdit::singleline(&mut profile.model)
                .hint_text("glm-5.3-flash")
                .margin(vec2(10.0, 7.0))
                .desired_width(220.0),
        );
    });
    divider(ui);
    field(
        ui,
        "Description",
        "What kind of speech this profile is for. System One matches against it.",
        |ui| {
            ui.add(
                egui::TextEdit::multiline(&mut profile.description)
                    .hint_text(builtin_description.unwrap_or("For example: an email or a reply"))
                    .desired_rows(2)
                    .margin(vec2(10.0, 8.0))
                    .desired_width(f32::INFINITY),
            );
        },
    );
    divider(ui);
    field(ui, "Prompt", "", |ui| {
        ui.add(
            egui::TextEdit::multiline(&mut profile.prompt)
                .hint_text(builtin_prompt.unwrap_or("How to rewrite the transcript"))
                .desired_rows(6)
                .margin(vec2(10.0, 8.0))
                .desired_width(f32::INFINITY),
        );
        if let Some(prompt) = builtin_prompt
            && profile.prompt.trim().is_empty()
            && ui.button("Edit the built-in prompt").clicked()
        {
            profile.prompt = prompt.to_owned();
        }
    });
}

// Building blocks.

fn page_title(ui: &mut egui::Ui, title: &str, intro: Option<&str>) {
    let p = theme::Palette::of(ui);
    ui.label(RichText::new(title).font(theme::bold(28.0)).color(p.ink));
    if let Some(intro) = intro {
        ui.add_space(4.0);
        ui.label(RichText::new(intro).color(p.mist));
    }
    ui.add_space(6.0);
}

fn section_title(ui: &mut egui::Ui, title: &str, help: &str) {
    let p = theme::Palette::of(ui);
    ui.add_space(22.0);
    ui.label(
        RichText::new(title)
            .font(theme::semibold(16.0))
            .color(p.ink),
    );
    if !help.is_empty() {
        ui.label(RichText::new(help).color(p.mist));
    }
    ui.add_space(8.0);
}

fn card<R>(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let p = theme::Palette::of(ui);
    egui::Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0, p.line))
        .corner_radius(12)
        .inner_margin(Margin::symmetric(20, 16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            contents(ui)
        })
        .inner
}

fn divider(ui: &mut egui::Ui) {
    let p = theme::Palette::of(ui);
    ui.add_space(10.0);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(rect, 0, p.line);
    ui.add_space(10.0);
}

/// A setting with its control on the right.
fn row(ui: &mut egui::Ui, title: &str, help: &str, control: impl FnOnce(&mut egui::Ui)) {
    let p = theme::Palette::of(ui);
    let width = ui.available_width();
    ui.horizontal(|ui| {
        ui.set_width(width);
        ui.vertical(|ui| {
            ui.set_width((width - 260.0).max(width * 0.5));
            ui.label(
                RichText::new(title)
                    .font(theme::semibold(14.5))
                    .color(p.ink),
            );
            if !help.is_empty() {
                ui.add(egui::Label::new(RichText::new(help).small().color(p.mist)).wrap());
            }
        });
        ui.with_layout(Layout::right_to_left(Align::Center), control);
    });
}

/// A setting with a wide control below its title.
fn field(ui: &mut egui::Ui, title: &str, help: &str, control: impl FnOnce(&mut egui::Ui)) {
    let p = theme::Palette::of(ui);
    ui.label(
        RichText::new(title)
            .font(theme::semibold(14.5))
            .color(p.ink),
    );
    if !help.is_empty() {
        ui.add(egui::Label::new(RichText::new(help).small().color(p.mist)).wrap());
    }
    ui.add_space(6.0);
    control(ui);
}

fn text(ui: &mut egui::Ui, value: &mut String, hint: &str) {
    ui.add(
        egui::TextEdit::singleline(value)
            .hint_text(hint)
            .margin(vec2(10.0, 7.0))
            .desired_width(380.0),
    );
}

fn secret(ui: &mut egui::Ui, value: &mut String, show: &mut bool, hint: &str) {
    ui.horizontal(|ui| {
        let width = (ui.available_width() - 80.0).max(200.0);
        ui.add(
            egui::TextEdit::singleline(value)
                .password(!*show)
                .hint_text(hint)
                .font(TextStyle::Monospace)
                .margin(vec2(10.0, 8.0))
                .desired_width(width),
        );
        ui.toggle_value(show, if *show { "Hide" } else { "Show" });
    });
}

fn env_note(ui: &mut egui::Ui, variable: &str) {
    if env::var_os(variable).is_some() {
        let p = theme::Palette::of(ui);
        ui.label(
            RichText::new(format!(
                "{variable} is set in this environment and overrides this value."
            ))
            .small()
            .color(p.warn),
        );
    }
}

/// A path field where empty means "not set".
fn optional_path(ui: &mut egui::Ui, label: &str, value: &mut Option<PathBuf>, hint: &str) {
    ui.horizontal(|ui| {
        if !label.is_empty() {
            ui.add_sized([44.0, 20.0], egui::Label::new(label));
        }
        let mut text = value
            .as_deref()
            .map(Path::to_string_lossy)
            .unwrap_or_default()
            .into_owned();
        let response = ui.add(
            egui::TextEdit::singleline(&mut text)
                .hint_text(hint)
                .margin(vec2(10.0, 7.0))
                .desired_width(380.0),
        );
        if response.changed() {
            *value = (!text.trim().is_empty()).then(|| PathBuf::from(text.trim()));
        }
    });
}

fn primary_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let p = theme::Palette::of(ui);
    ui.add(
        egui::Button::new(
            RichText::new(label)
                .font(theme::semibold(14.5))
                .color(p.on_accent),
        )
        .fill(p.accent)
        .stroke(Stroke::NONE)
        .corner_radius(8)
        .min_size(vec2(0.0, 34.0)),
    )
}

fn pill(ui: &mut egui::Ui, label: &str, color: Color32) {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.16))
        .corner_radius(255)
        .inner_margin(Margin::symmetric(9, 2))
        .show(ui, |ui| {
            ui.label(RichText::new(label).small().color(color));
        });
}

fn dot(ui: &mut egui::Ui, color: Color32, size: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    ui.painter().circle_filled(rect.center(), size / 2.0, color);
}

fn nav_item(ui: &mut egui::Ui, label: &str, selected: bool, badge: Option<&str>) -> egui::Response {
    let p = theme::Palette::of(ui);
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::click());
    let fill = if selected {
        p.accent.gamma_multiply(0.14)
    } else if response.hovered() {
        p.raised
    } else {
        Color32::TRANSPARENT
    };
    let painter = ui.painter();
    painter.rect_filled(rect, 8, fill);
    if response.has_focus() {
        painter.rect_stroke(rect, 8, Stroke::new(1.5, p.accent), StrokeKind::Inside);
    }
    let (font, color) = if selected {
        (theme::semibold(14.5), p.accent)
    } else {
        (FontId::proportional(14.5), p.ink)
    };
    painter.text(
        rect.left_center() + vec2(12.0, 0.0),
        Align2::LEFT_CENTER,
        label,
        font,
        color,
    );
    if let Some(badge) = badge {
        let galley = painter.layout_no_wrap(badge.to_owned(), FontId::proportional(11.5), p.warn);
        let size = galley.size() + vec2(16.0, 4.0);
        let badge_rect = egui::Rect::from_min_size(
            egui::pos2(rect.right() - 10.0 - size.x, rect.center().y - size.y / 2.0),
            size,
        );
        painter.rect_filled(badge_rect, 255, p.warn.gamma_multiply(0.16));
        painter.galley(badge_rect.min + vec2(8.0, 2.0), galley, p.warn);
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, label)
    });
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn toggle(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let p = theme::Palette::of(ui);
    let (rect, mut response) = ui.allocate_exact_size(vec2(42.0, 24.0), Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), *on, "")
    });
    if ui.is_rect_visible(rect) {
        let t = ui.ctx().animate_bool_responsive(response.id, *on);
        let fade = |color: Color32| {
            if ui.is_enabled() {
                color
            } else {
                color.gamma_multiply(0.4)
            }
        };
        let track = p.raised.lerp_to_gamma(p.accent, t);
        let border = p.line.lerp_to_gamma(p.accent, t);
        let knob = p.mist.lerp_to_gamma(p.on_accent, t);
        let painter = ui.painter();
        painter.rect(
            rect,
            12,
            fade(track),
            Stroke::new(1.0, fade(border)),
            StrokeKind::Inside,
        );
        let x = egui::lerp((rect.left() + 12.0)..=(rect.right() - 12.0), t);
        painter.circle_filled(egui::pos2(x, rect.center().y), 8.0, fade(knob));
        if response.has_focus() {
            painter.rect_stroke(
                rect.expand(2.0),
                14,
                Stroke::new(1.5, p.accent),
                StrokeKind::Outside,
            );
        }
    }
    response
}

/// The Home page's voice line: moving while Hydra listens, sweeping while it
/// starts or stops, and flat when it is off.
fn voice_wave(ui: &mut egui::Ui, state: State, time: f64) {
    const BARS: usize = 36;
    let p = theme::Palette::of(ui);
    let (bar, gap, height) = (5.0_f32, 4.0_f32, 64.0_f32);
    let width = BARS as f32 * (bar + gap) - gap;
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let painter = ui.painter();
    let time = time as f32;
    for index in 0..BARS {
        let u = index as f32 / (BARS - 1) as f32;
        let envelope = (std::f32::consts::PI * u).sin().powf(0.8);
        let (level, color) = match state {
            State::Running => {
                let a = (time * 1.6 - index as f32 * 0.38).sin();
                let b = (time * 0.9 + index as f32 * 0.83).cos();
                let wave = 0.5 + 0.5 * a * b;
                (envelope * (0.25 + 0.75 * wave), p.accent)
            }
            State::Working => {
                let head = (time * 0.8).fract();
                let near = (1.0 - (u - head).abs() * 5.0).max(0.0);
                (0.15 + 0.6 * near, p.warn)
            }
            State::Stopped => (envelope * 0.12, p.line),
        };
        let bar_height = 5.0 + (height - 5.0) * level;
        let center = egui::pos2(
            rect.left() + index as f32 * (bar + gap) + bar / 2.0,
            rect.center().y,
        );
        painter.rect_filled(
            egui::Rect::from_center_size(center, vec2(bar, bar_height)),
            CornerRadius::same((bar / 2.0) as u8),
            color,
        );
    }
}

mod theme {
    //! Colors and type for the window, in dark and light.

    use super::*;

    #[derive(Clone, Copy)]
    pub struct Palette {
        pub bg: Color32,
        pub sidebar: Color32,
        pub card: Color32,
        pub raised: Color32,
        pub line: Color32,
        pub ink: Color32,
        pub mist: Color32,
        pub accent: Color32,
        pub on_accent: Color32,
        pub warn: Color32,
        pub error: Color32,
    }

    const DARK: Palette = Palette {
        bg: Color32::from_rgb(0x0F, 0x16, 0x22),
        sidebar: Color32::from_rgb(0x0B, 0x11, 0x1B),
        card: Color32::from_rgb(0x18, 0x21, 0x2F),
        raised: Color32::from_rgb(0x22, 0x2D, 0x3E),
        line: Color32::from_rgb(0x29, 0x35, 0x47),
        ink: Color32::from_rgb(0xE7, 0xED, 0xF4),
        mist: Color32::from_rgb(0x8E, 0x9C, 0xB0),
        accent: Color32::from_rgb(0x5B, 0xD3, 0xBC),
        on_accent: Color32::from_rgb(0x06, 0x2A, 0x24),
        warn: Color32::from_rgb(0xED, 0xB8, 0x5A),
        error: Color32::from_rgb(0xFF, 0x7D, 0x73),
    };

    const LIGHT: Palette = Palette {
        bg: Color32::from_rgb(0xF4, 0xF6, 0xF9),
        sidebar: Color32::from_rgb(0xE9, 0xED, 0xF2),
        card: Color32::from_rgb(0xFF, 0xFF, 0xFF),
        raised: Color32::from_rgb(0xE6, 0xEA, 0xF0),
        line: Color32::from_rgb(0xD8, 0xDE, 0xE6),
        ink: Color32::from_rgb(0x15, 0x1F, 0x2E),
        mist: Color32::from_rgb(0x5C, 0x6A, 0x7D),
        accent: Color32::from_rgb(0x0E, 0x8A, 0x75),
        on_accent: Color32::from_rgb(0xFF, 0xFF, 0xFF),
        warn: Color32::from_rgb(0xA8, 0x6A, 0x00),
        error: Color32::from_rgb(0xC4, 0x3B, 0x30),
    };

    impl Palette {
        pub fn of(ui: &egui::Ui) -> Self {
            if ui.visuals().dark_mode { DARK } else { LIGHT }
        }
    }

    const SEMIBOLD: &str = "semibold";
    const BOLD: &str = "bold";

    pub fn semibold(size: f32) -> FontId {
        FontId::new(size, FontFamily::Name(SEMIBOLD.into()))
    }

    pub fn bold(size: f32) -> FontId {
        FontId::new(size, FontFamily::Name(BOLD.into()))
    }

    pub fn install(ctx: &egui::Context) {
        let mut fonts = egui::FontDefinitions::default();
        for (name, bytes) in [
            (
                "manrope",
                &include_bytes!("../assets/fonts/Manrope-Regular.ttf")[..],
            ),
            (
                SEMIBOLD,
                &include_bytes!("../assets/fonts/Manrope-SemiBold.ttf")[..],
            ),
            (
                BOLD,
                &include_bytes!("../assets/fonts/Manrope-Bold.ttf")[..],
            ),
        ] {
            fonts.font_data.insert(
                name.to_owned(),
                Arc::new(egui::FontData::from_static(bytes)),
            );
        }
        // Manrope covers Latin; the default fonts cover everything else.
        let fallback = fonts.families[&FontFamily::Proportional].clone();
        fonts
            .families
            .get_mut(&FontFamily::Proportional)
            .expect("default family")
            .insert(0, "manrope".to_owned());
        for name in [SEMIBOLD, BOLD] {
            let mut family = vec![name.to_owned()];
            family.extend(fallback.iter().cloned());
            fonts.families.insert(FontFamily::Name(name.into()), family);
        }
        ctx.set_fonts(fonts);

        ctx.set_visuals_of(Theme::Dark, visuals(&DARK, egui::Visuals::dark()));
        ctx.set_visuals_of(Theme::Light, visuals(&LIGHT, egui::Visuals::light()));
        ctx.all_styles_mut(|style| {
            style.text_styles = [
                (TextStyle::Small, FontId::proportional(12.5)),
                (TextStyle::Body, FontId::proportional(14.5)),
                (TextStyle::Button, FontId::proportional(14.5)),
                (TextStyle::Monospace, FontId::monospace(13.0)),
                (TextStyle::Heading, bold(22.0)),
            ]
            .into();
            let spacing = &mut style.spacing;
            spacing.item_spacing = vec2(10.0, 6.0);
            spacing.button_padding = vec2(14.0, 7.0);
            spacing.interact_size = vec2(40.0, 30.0);
            spacing.slider_width = 200.0;
            spacing.combo_width = 140.0;
            spacing.icon_width = 18.0;
            spacing.icon_width_inner = 10.0;
        });
    }

    fn visuals(p: &Palette, mut v: egui::Visuals) -> egui::Visuals {
        v.panel_fill = p.bg;
        v.window_fill = p.card;
        v.window_stroke = Stroke::new(1.0, p.line);
        v.window_corner_radius = CornerRadius::same(14);
        v.menu_corner_radius = CornerRadius::same(10);
        v.faint_bg_color = p.card;
        v.extreme_bg_color = p.bg;
        v.text_edit_bg_color = Some(p.bg);
        v.code_bg_color = p.raised;
        v.warn_fg_color = p.warn;
        v.error_fg_color = p.error;
        v.hyperlink_color = p.accent;
        v.weak_text_color = Some(p.mist);
        v.selection.bg_fill = p.accent.gamma_multiply(0.3);
        v.selection.stroke = Stroke::new(1.0, p.accent);
        v.text_cursor.stroke = Stroke::new(2.0, p.accent);
        v.slider_trailing_fill = true;
        v.indent_has_left_vline = false;

        let radius = CornerRadius::same(8);
        let widgets = &mut v.widgets;
        widgets.noninteractive.bg_fill = p.card;
        widgets.noninteractive.weak_bg_fill = p.card;
        widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.line);
        widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.ink);
        widgets.noninteractive.corner_radius = radius;
        widgets.inactive.bg_fill = p.raised;
        widgets.inactive.weak_bg_fill = p.raised;
        widgets.inactive.bg_stroke = Stroke::new(1.0, p.line);
        widgets.inactive.fg_stroke = Stroke::new(1.0, p.ink);
        widgets.inactive.corner_radius = radius;
        let hover = p.raised.lerp_to_gamma(p.ink, 0.08);
        widgets.hovered.bg_fill = hover;
        widgets.hovered.weak_bg_fill = hover;
        widgets.hovered.bg_stroke = Stroke::new(1.0, p.mist);
        widgets.hovered.fg_stroke = Stroke::new(1.5, p.ink);
        widgets.hovered.corner_radius = radius;
        widgets.hovered.expansion = 0.0;
        widgets.active.bg_fill = p.accent.gamma_multiply(0.25);
        widgets.active.weak_bg_fill = p.accent.gamma_multiply(0.25);
        widgets.active.bg_stroke = Stroke::new(1.0, p.accent);
        widgets.active.fg_stroke = Stroke::new(1.5, p.ink);
        widgets.active.corner_radius = radius;
        widgets.active.expansion = 0.0;
        widgets.open = widgets.hovered;
        v
    }
}
