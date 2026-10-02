//! The Hydra STT window, opened by running `hydra-stt`: starts and stops the
//! background daemon and edits `config.toml`. Saving rewrites only the
//! values, so comments in the file survive. The daemon reads the file at
//! startup, so changes apply after a restart.

use crate::{
    config::{self, Config, Newlines, Profile},
    profiles, service,
};
use anyhow::Result;
use eframe::egui;
use std::{
    env,
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// How long a freshly started daemon gets to open its control socket.
const START_GRACE: Duration = Duration::from_secs(4);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Home,
    Keys,
    Speech,
    Processing,
    Profiles,
    Agent,
    Output,
}

const PAGES: [(Page, &str); 7] = [
    (Page::Home, "Home"),
    (Page::Keys, "API keys"),
    (Page::Speech, "Speech & recording"),
    (Page::Processing, "Processing"),
    (Page::Profiles, "Profiles"),
    (Page::Agent, "Computer agent"),
    (Page::Output, "Output & logs"),
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
            .with_inner_size([920.0, 660.0])
            .with_min_inner_size([640.0, 420.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Hydra STT",
        options,
        Box::new(move |_| Ok(Box::new(Settings::new(path, runtime)))),
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
                let mut message = if self.running {
                    "Saved. Restart Hydra to apply the changes.".to_owned()
                } else {
                    "Saved.".to_owned()
                };
                for warning in self.warnings() {
                    message.push_str("\nNote: ");
                    message.push_str(&warning);
                }
                self.status = Some(Status::Info(message));
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
    fn warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        if self.config.groq.api_key.trim().is_empty() && env::var_os("GROQ_API_KEY").is_none() {
            warnings.push("no Groq API key; the daemon needs one to transcribe.".to_owned());
        }
        if self.config.isoquant.enabled
            && self.config.isoquant.api_key.trim().is_empty()
            && env::var_os("ISO_QUANT_API_KEY").is_none()
        {
            warnings
                .push("no IsoQuant API key; add one or turn off IsoQuant processing.".to_owned());
        }
        warnings
    }

    fn test_groq_key(&mut self, ctx: &egui::Context) {
        let key = match self.config.groq.api_key.trim() {
            "" => env::var("GROQ_API_KEY").unwrap_or_default(),
            key => key.to_owned(),
        };
        if key.is_empty() {
            self.status = Some(Status::Error("Enter a Groq API key to test.".to_owned()));
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
        self.status = Some(Status::Info("Testing the Groq key…".to_owned()));
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
        self.status = Some(match result {
            Ok(models) => {
                let message = format!("The Groq key works. Speech models: {}.", models.join(", "));
                self.groq_models = models;
                Status::Info(message)
            }
            Err(error) => Status::Error(error),
        });
    }
}

impl Settings {
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
                            "run `hydra-stt --daemon` in a terminal to see why".to_owned()
                        });
                    self.status = Some(Status::Error(format!(
                        "Hydra stopped right after starting: {reason}"
                    )));
                }
            }
        }
        ctx.request_repaint_after(Duration::from_secs(1));
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

        egui::Panel::bottom("actions").show(ui, |ui| {
            ui.add_space(6.0);
            self.status_line(ui);
            ui.horizontal(|ui| {
                ui.weak(self.path.display().to_string());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let dirty = self.dirty();
                    let save = ui.add_enabled(dirty, egui::Button::new("Save"));
                    if save.on_hover_text("Ctrl+S").clicked() {
                        self.save();
                    }
                    if ui
                        .add_enabled(dirty, egui::Button::new("Discard changes"))
                        .clicked()
                    {
                        self.config = self.saved.clone();
                        self.status = None;
                    }
                    if ui.button("Reload file").clicked() {
                        self.reload();
                        self.status = None;
                    }
                    if dirty {
                        ui.colored_label(ui.visuals().warn_fg_color, "Unsaved changes");
                    } else if self.restart_needed
                        && self.running
                        && self.action.is_none()
                        && ui.button("Restart Hydra to apply").clicked()
                    {
                        self.run_action(ui.ctx(), true, service::restart);
                    }
                });
            });
            ui.add_space(6.0);
        });

        egui::Panel::left("pages")
            .resizable(false)
            .exact_size(180.0)
            .show(ui, |ui| {
                ui.add_space(10.0);
                ui.heading("Hydra STT");
                ui.horizontal(|ui| {
                    let (color, label) = self.state_label(ui);
                    status_dot(ui, color, 8.0);
                    ui.weak(label);
                });
                ui.add_space(10.0);
                for (page, label) in PAGES {
                    let selected = self.page == page;
                    let button = egui::Button::selectable(selected, label)
                        .min_size(egui::vec2(ui.available_width(), 28.0));
                    if ui.add(button).clicked() {
                        self.page = page;
                    }
                }
            });

        egui::CentralPanel::default_margins().show(ui, |ui| {
            if let Some(error) = &self.load_error {
                ui.heading("The configuration file could not be read");
                ui.add_space(8.0);
                ui.colored_label(ui.visuals().error_fg_color, error);
                ui.add_space(8.0);
                ui.label("Fix the file in a text editor, then press Reload file.");
                return;
            }
            egui::ScrollArea::vertical()
                .auto_shrink(false)
                .show(ui, |ui| match self.page {
                    Page::Home => self.home_page(ui),
                    Page::Keys => self.keys_page(ui),
                    Page::Speech => self.speech_page(ui),
                    Page::Processing => self.processing_page(ui),
                    Page::Profiles => self.profiles_page(ui),
                    Page::Agent => self.agent_page(ui),
                    Page::Output => self.output_page(ui),
                });
        });

        if self.confirm_close {
            self.close_dialog(&ctx);
        }
    }
}

impl Settings {
    fn status_line(&self, ui: &mut egui::Ui) {
        match &self.status {
            Some(Status::Info(message)) => {
                ui.label(message);
            }
            Some(Status::Error(message)) => {
                ui.colored_label(ui.visuals().error_fg_color, message);
            }
            None => return,
        }
        ui.add_space(4.0);
    }

    fn close_dialog(&mut self, ctx: &egui::Context) {
        egui::Modal::new(egui::Id::new("confirm_close")).show(ctx, |ui| {
            ui.heading("Save changes?");
            ui.label("You have unsaved changes.");
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Save and close").clicked() {
                    self.save();
                    if !self.dirty() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    self.confirm_close = false;
                }
                if ui.button("Close without saving").clicked() {
                    self.config = self.saved.clone();
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                if ui.button("Cancel").clicked() {
                    self.confirm_close = false;
                }
            });
        });
    }

    fn state_label(&self, ui: &egui::Ui) -> (egui::Color32, &'static str) {
        if self.action.is_some() || (self.started.is_some() && !self.running) {
            (ui.visuals().warn_fg_color, "Working…")
        } else if self.running {
            (egui::Color32::from_rgb(80, 190, 110), "Running")
        } else {
            (ui.visuals().weak_text_color(), "Not running")
        }
    }

    fn home_page(&mut self, ui: &mut egui::Ui) {
        page_title(ui, "Hydra STT");
        ui.label(
            "Hydra runs in the background and listens for your shortcut. Closing this \
             window leaves it running.",
        );
        ui.add_space(12.0);

        egui::Frame::group(ui.style())
            .inner_margin(14.0)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let (color, label) = self.state_label(ui);
                    status_dot(ui, color, 12.0);
                    ui.label(egui::RichText::new(label).size(18.0).strong());
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let idle = self.action.is_none();
                    let ctx = ui.ctx().clone();
                    if self.running {
                        if ui.add_enabled(idle, egui::Button::new("Stop")).clicked() {
                            self.run_action(&ctx, false, service::stop);
                        }
                        if ui.add_enabled(idle, egui::Button::new("Restart")).clicked() {
                            self.run_action(&ctx, true, service::restart);
                        }
                    } else {
                        let start = egui::Button::new("Start").min_size(egui::vec2(90.0, 0.0));
                        let ready = idle && self.started.is_none() && !self.dirty();
                        let response = ui.add_enabled(ready, start);
                        if self.dirty() {
                            response.on_disabled_hover_text("Save your changes first");
                        } else if response.clicked() {
                            self.run_action(&ctx, true, service::start);
                        }
                    }
                });
                ui.add_space(8.0);
                match self.autostart {
                    Some(mut enabled) => {
                        let toggle = ui
                            .checkbox(&mut enabled, "Start Hydra in the background when I log in");
                        if toggle.changed() {
                            match service::set_autostart(enabled) {
                                Ok(()) => self.autostart = Some(service::autostart_enabled()),
                                Err(error) => {
                                    self.status = Some(Status::Error(format!("{error:#}")))
                                }
                            }
                        }
                    }
                    None => {
                        ui.weak(
                            "To start Hydra at login, add `hydra-stt --daemon` to your \
                             compositor's startup commands.",
                        );
                    }
                }
            });
        ui.add_space(12.0);

        let warnings = self.warnings();
        if !warnings.is_empty() {
            for warning in &warnings {
                ui.colored_label(ui.visuals().warn_fg_color, format!("Setup: {warning}"));
            }
            if ui.button("Add API keys").clicked() {
                self.page = Page::Keys;
            }
            ui.add_space(12.0);
        }

        section(
            ui,
            "Shortcut",
            "Bind this command to a key in your compositor. Press it to start recording, \
             and again to stop and type the text.",
        );
        let command = service::toggle_command();
        ui.horizontal(|ui| {
            ui.code(&command);
            if ui.small_button("Copy").clicked() {
                ui.ctx().copy_text(command.clone());
            }
        });
        ui.weak(format!(
            "Niri example: Mod+Space {{ spawn \"{}\" \"--toggle\"; }}",
            command.trim_end_matches(" --toggle")
        ));
    }

    fn keys_page(&mut self, ui: &mut egui::Ui) {
        page_title(ui, "API keys");
        ui.label(
            "Keys are stored in the config file, which only your user can read. \
             Environment variables override them.",
        );
        ui.add_space(12.0);

        section(ui, "Groq", "Transcribes your speech. Required.");
        secret(
            ui,
            &mut self.config.groq.api_key,
            &mut self.show_groq_key,
            "gsk_…",
        );
        env_note(ui, "GROQ_API_KEY");
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
        });
        ui.hyperlink_to("Get a Groq key", "https://console.groq.com/keys");
        ui.add_space(16.0);

        section(
            ui,
            "IsoQuant",
            "Routes transcripts to profiles and rewrites them. Needed unless IsoQuant \
             processing is turned off.",
        );
        let isoquant = &mut self.config.isoquant;
        secret(ui, &mut isoquant.api_key, &mut self.show_isoquant_key, "");
        env_note(ui, "ISO_QUANT_API_KEY");
        ui.checkbox(&mut isoquant.enabled, "Process transcripts with IsoQuant");
    }

    fn speech_page(&mut self, ui: &mut egui::Ui) {
        page_title(ui, "Speech & recording");
        section(ui, "Transcription model", "The Groq Whisper model.");
        ui.horizontal(|ui| {
            text(ui, &mut self.config.groq.model, "whisper-large-v3-turbo");
            if !self.groq_models.is_empty() {
                egui::ComboBox::from_id_salt("groq_models")
                    .selected_text("Choose")
                    .show_ui(ui, |ui| {
                        for model in &self.groq_models {
                            ui.selectable_value(&mut self.config.groq.model, model.clone(), model);
                        }
                    });
            }
        });
        if self.groq_models.is_empty() {
            ui.weak("Test your Groq key on the API keys page to list the models.");
        }
        env_note(ui, "GROQ_MODEL");
        ui.add_space(16.0);

        section(
            ui,
            "Microphone",
            "ALSA capture device for `arecord -D`. Leave empty for the system default; \
             list devices with `arecord -L`.",
        );
        text(ui, &mut self.config.recording.device, "System default");
        ui.add_space(16.0);

        section(ui, "Sounds", "Clicks when recording starts and stops.");
        let sounds = &mut self.config.sounds;
        ui.checkbox(&mut sounds.enabled, "Play sounds");
        ui.add_enabled_ui(sounds.enabled, |ui| {
            ui.add(egui::Slider::new(&mut sounds.volume, 0..=100).text("Volume (%)"));
            optional_path(ui, "Start sound", &mut sounds.press, "Built-in (WAV file)");
            optional_path(ui, "Stop sound", &mut sounds.release, "Built-in (WAV file)");
        });
        ui.add_space(16.0);

        section(ui, "Media", "");
        ui.checkbox(
            &mut self.config.media.pause_while_recording,
            "Pause playing media while recording, and resume it afterwards",
        );
    }

    fn processing_page(&mut self, ui: &mut egui::Ui) {
        page_title(ui, "Processing");
        let isoquant = &mut self.config.isoquant;
        ui.checkbox(&mut isoquant.enabled, "Process transcripts with IsoQuant");
        ui.weak("When off, or when a request fails, the raw transcript is typed.");
        ui.add_space(12.0);
        ui.add_enabled_ui(isoquant.enabled, |ui| {
            section(ui, "IsoQuant API", "");
            egui::Grid::new("isoquant")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    ui.label("API URL");
                    text(ui, &mut isoquant.api_url, "https://api.isoquant.ai/v1");
                    ui.end_row();
                    ui.label("Timeout");
                    ui.add(
                        egui::DragValue::new(&mut isoquant.timeout_secs)
                            .range(1..=600)
                            .suffix(" s"),
                    );
                    ui.end_row();
                });
            ui.add_space(16.0);

            section(
                ui,
                "Router",
                "With more than one profile, System One picks one for each transcript from \
                 the profile descriptions. Below the minimum confidence, or when routing \
                 fails, the default profile is used.",
            );
            let router = &mut self.config.router;
            egui::Grid::new("router")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    ui.label("Model");
                    text(ui, &mut router.model, "isoquant/system-one");
                    ui.end_row();
                    ui.label("Instructions");
                    ui.add(
                        egui::TextEdit::multiline(&mut router.instructions)
                            .desired_rows(2)
                            .desired_width(f32::INFINITY),
                    );
                    ui.end_row();
                    ui.label("Minimum confidence");
                    ui.add(egui::Slider::new(&mut router.min_confidence, 0.0..=1.0));
                    ui.end_row();
                });
        });
    }

    fn profiles_page(&mut self, ui: &mut egui::Ui) {
        page_title(ui, "Profiles");
        ui.label(
            "Each profile rewrites the transcript with its prompt, or, with agent tools, acts \
             on it. \"default\" is required and is the fallback. Empty fields on the built-in \
             profiles use the built-in text, shown greyed out.",
        );
        ui.add_space(12.0);

        let mut names: Vec<String> = self.config.profiles.0.keys().cloned().collect();
        names.sort_by_key(|name| name != profiles::DEFAULT);
        let mut remove = None;
        for name in &names {
            let builtin = profiles::builtin(name);
            let profile = self.config.profiles.0.get_mut(name).expect("listed above");
            egui::Frame::group(ui.style())
                .inner_margin(12.0)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.strong(name);
                        if builtin.is_some() {
                            ui.weak("built-in");
                        }
                        if name != profiles::DEFAULT {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.button("Remove").clicked() {
                                        remove = Some(name.clone());
                                    }
                                },
                            );
                        }
                    });
                    ui.add_space(6.0);
                    profile_editor(ui, name, profile, builtin);
                });
            ui.add_space(8.0);
        }
        if let Some(name) = remove {
            self.config.profiles.0.remove(&name);
        }

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.new_profile)
                    .hint_text("new profile name, e.g. email")
                    .desired_width(240.0),
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
        ui.weak("Names use letters, digits, - and _. Built-in names: default, prompt, computer.");
    }

    fn agent_page(&mut self, ui: &mut egui::Ui) {
        page_title(ui, "Computer agent");
        ui.label(
            "The agent takes screenshots, moves and clicks the mouse, types, runs commands, \
             and reads and writes files, all as you. Screenshots are sent to IsoQuant. Press \
             your toggle key while it works to stop it.",
        );
        ui.add_space(12.0);
        let computer = &mut self.config.computer;
        egui::Grid::new("computer")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("Maximum steps");
                ui.add(egui::DragValue::new(&mut computer.max_steps).range(1..=500));
                ui.end_row();
                ui.label("Command timeout");
                ui.add(
                    egui::DragValue::new(&mut computer.command_timeout_secs)
                        .range(1..=3600)
                        .suffix(" s"),
                );
                ui.end_row();
                ui.label("Screenshot size");
                ui.add(
                    egui::DragValue::new(&mut computer.screenshot_max_size)
                        .range(256..=4096)
                        .suffix(" px"),
                )
                .on_hover_text("Longest edge of screenshots sent to the model");
                ui.end_row();
            });
        ui.checkbox(
            &mut computer.notify,
            "Show notifications when it starts and finishes",
        );
        ui.checkbox(
            &mut computer.allow_privileged,
            "Allow sudo, su, pkexec, doas and run0 in commands",
        );
        ui.add_space(16.0);

        section(
            ui,
            "History",
            "Each profile sees its last few requests, so it can resolve \"do that again\" \
             and keep spellings consistent.",
        );
        let history = &mut self.config.history;
        ui.checkbox(&mut history.enabled, "Keep history");
        ui.add_enabled_ui(history.enabled, |ui| {
            ui.horizontal(|ui| {
                ui.label("Entries per profile");
                ui.add(egui::DragValue::new(&mut history.entries).range(0..=100));
            });
        });
    }

    fn output_page(&mut self, ui: &mut egui::Ui) {
        page_title(ui, "Output & logs");
        let output = &mut self.config.output;
        ui.checkbox(
            &mut output.press_enter,
            "Press Enter after typing (submits chat prompts and commands)",
        );
        ui.add_space(12.0);
        section(ui, "Line breaks", "How line breaks in the text are typed.");
        ui.radio_value(
            &mut output.newlines,
            Newlines::Space,
            "Join everything into one line (safe everywhere)",
        );
        ui.radio_value(
            &mut output.newlines,
            Newlines::ShiftEnter,
            "Keep line breaks, typed as Shift+Enter (some terminals submit early)",
        );
        ui.add_space(16.0);
        section(ui, "Logs", "");
        optional_path(
            ui,
            "Log directory",
            &mut self.config.logging.dir,
            "~/.local/state/hydra-stt",
        );
    }
}

fn profile_editor(
    ui: &mut egui::Ui,
    name: &str,
    profile: &mut Profile,
    builtin: Option<(&str, &str)>,
) {
    let (builtin_prompt, builtin_description) = builtin.unzip();
    egui::Grid::new(("profile", name))
        .num_columns(2)
        .spacing([12.0, 8.0])
        .show(ui, |ui| {
            ui.label("Model");
            text(ui, &mut profile.model, "glm-5.3-flash");
            ui.end_row();

            ui.label("Agent tools");
            let default_tools = name == "computer";
            let label = |tools: Option<bool>| match tools {
                None if default_tools => "Default (on)",
                None => "Default (off)",
                Some(true) => "On",
                Some(false) => "Off",
            };
            egui::ComboBox::from_id_salt(("tools", name))
                .selected_text(label(profile.tools))
                .show_ui(ui, |ui| {
                    for tools in [None, Some(true), Some(false)] {
                        ui.selectable_value(&mut profile.tools, tools, label(tools));
                    }
                });
            ui.end_row();

            ui.label("Description")
                .on_hover_text("What System One matches transcripts against");
            ui.add(
                egui::TextEdit::multiline(&mut profile.description)
                    .hint_text(builtin_description.unwrap_or("When to use this profile"))
                    .desired_rows(2)
                    .desired_width(f32::INFINITY),
            );
            ui.end_row();

            ui.label("Prompt");
            ui.vertical(|ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut profile.prompt)
                        .hint_text(builtin_prompt.unwrap_or("The system prompt"))
                        .desired_rows(6)
                        .desired_width(f32::INFINITY),
                );
                if let Some(prompt) = builtin_prompt
                    && profile.prompt.trim().is_empty()
                    && ui.small_button("Customize the built-in prompt").clicked()
                {
                    profile.prompt = prompt.to_owned();
                }
            });
            ui.end_row();
        });
}

fn status_dot(ui: &mut egui::Ui, color: egui::Color32, size: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), size / 2.0, color);
}

fn page_title(ui: &mut egui::Ui, title: &str) {
    ui.add_space(4.0);
    ui.heading(title);
    ui.add_space(8.0);
}

fn section(ui: &mut egui::Ui, title: &str, help: &str) {
    ui.strong(title);
    if !help.is_empty() {
        ui.weak(help);
    }
    ui.add_space(4.0);
}

fn text(ui: &mut egui::Ui, value: &mut String, hint: &str) {
    ui.add(
        egui::TextEdit::singleline(value)
            .hint_text(hint)
            .desired_width(360.0),
    );
}

fn secret(ui: &mut egui::Ui, value: &mut String, show: &mut bool, hint: &str) {
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(value)
                .password(!*show)
                .hint_text(hint)
                .desired_width(420.0),
        );
        ui.toggle_value(show, if *show { "Hide" } else { "Show" });
    });
}

fn env_note(ui: &mut egui::Ui, variable: &str) {
    if env::var_os(variable).is_some() {
        ui.colored_label(
            ui.visuals().warn_fg_color,
            format!("{variable} is set in this environment and overrides this value."),
        );
    }
}

/// A path field where empty means "not set".
fn optional_path(ui: &mut egui::Ui, label: &str, value: &mut Option<PathBuf>, hint: &str) {
    ui.horizontal(|ui| {
        ui.label(label);
        let mut text = value
            .as_deref()
            .map(Path::to_string_lossy)
            .unwrap_or_default()
            .into_owned();
        let response = ui.add(
            egui::TextEdit::singleline(&mut text)
                .hint_text(hint)
                .desired_width(360.0),
        );
        if response.changed() {
            *value = (!text.trim().is_empty()).then(|| PathBuf::from(text.trim()));
        }
    });
}
