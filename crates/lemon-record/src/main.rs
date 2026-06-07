//! LEMON record — a standalone multi-track audio recorder (Lemon Audio).
//!
//! Aesthetic: Ableton meets a terminal. Flat dark panels, a monospace face,
//! a lemon-yellow brand accent, channel-strip meters. It owns its capture
//! directly (no daemon, no IPC) and couples to anything downstream only through
//! files: a per-session folder of per-take WAVs + a `session.json` manifest.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;
use egui::{FontFamily, FontId, Pos2, Rect, RichText, Rounding, Stroke, Vec2};
use recorder::config::{ArmMode, RecorderConfig, RecorderConfigState, TrackConfig};
use recorder::{list_input_devices, AudioDevice, Capture};

mod updater;
use updater::{UpdateState, Updater};

mod theme {
    use eframe::egui::Color32;
    pub const BG: Color32 = Color32::from_rgb(0x17, 0x18, 0x1a);
    pub const PANEL: Color32 = Color32::from_rgb(0x20, 0x22, 0x25);
    pub const STRIP: Color32 = Color32::from_rgb(0x1b, 0x1d, 0x20);
    pub const METER_BG: Color32 = Color32::from_rgb(0x0d, 0x0e, 0x10);
    pub const GRID: Color32 = Color32::from_rgb(0x2c, 0x2f, 0x33);
    pub const TEXT: Color32 = Color32::from_rgb(0xc9, 0xcc, 0xd0);
    pub const DIM: Color32 = Color32::from_rgb(0x70, 0x76, 0x7d);
    pub const GREEN: Color32 = Color32::from_rgb(0x4c, 0xd9, 0x64);
    pub const AMBER: Color32 = Color32::from_rgb(0xe8, 0xb5, 0x39);
    pub const LEMON: Color32 = Color32::from_rgb(0xf4, 0xd0, 0x3a); // Lemon Audio brand
    pub const RED: Color32 = Color32::from_rgb(0xe5, 0x4b, 0x4b);
    pub const REC: Color32 = Color32::from_rgb(0xe5, 0x3a, 0x3a);
}

const SAMPLE_RATES: &[u32] = &[44100, 48000, 88200, 96000];
const BIT_DEPTHS: &[u16] = &[16, 24, 32];

/// dBFS from a linear amplitude (mirrors recorder::metering, kept local so the
/// meter math doesn't reach across crates for one helper).
fn linear_to_db(v: f32) -> f32 {
    if v <= 0.0 {
        -96.0
    } else {
        20.0 * v.log10()
    }
}

fn main() -> eframe::Result {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([560.0, 700.0])
            .with_min_inner_size([460.0, 440.0])
            .with_title("LEMON record"),
        ..Default::default()
    };
    eframe::run_native(
        "LEMON record",
        native_options,
        Box::new(|cc| {
            install_theme(&cc.egui_ctx);
            Ok(Box::new(RecorderApp::new()))
        }),
    )
}

fn install_theme(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = theme::BG;
    v.window_fill = theme::PANEL;
    v.extreme_bg_color = theme::METER_BG;
    v.faint_bg_color = theme::STRIP;
    v.code_bg_color = theme::METER_BG;
    v.override_text_color = Some(theme::TEXT);

    let border = Stroke::new(1.0, theme::GRID);
    v.widgets.noninteractive.bg_fill = theme::PANEL;
    v.widgets.noninteractive.weak_bg_fill = theme::PANEL;
    v.widgets.noninteractive.bg_stroke = border;
    v.widgets.inactive.bg_fill = theme::STRIP;
    v.widgets.inactive.weak_bg_fill = theme::STRIP;
    v.widgets.inactive.bg_stroke = border;
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, theme::TEXT);
    v.widgets.hovered.bg_fill = theme::GRID;
    v.widgets.hovered.weak_bg_fill = theme::GRID;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, theme::DIM);
    v.widgets.active.bg_fill = theme::GRID;
    v.widgets.active.weak_bg_fill = theme::GRID;
    v.widgets.open.bg_fill = theme::STRIP;
    v.widgets.open.weak_bg_fill = theme::STRIP;
    v.widgets.open.bg_stroke = border;
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.rounding = Rounding::same(3.0);
    }
    v.selection.bg_fill = theme::GRID;
    v.selection.stroke = Stroke::new(1.0, theme::GREEN);

    let mut style = egui::Style::default();
    style.visuals = v;
    style.override_font_id = Some(FontId::new(13.0, FontFamily::Monospace));
    ctx.set_style(style);
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum View {
    Recorder,
    Settings,
}

struct RecorderApp {
    capture: Capture,
    config_state: RecorderConfigState,
    cfg: RecorderConfig,
    devices: Vec<AudioDevice>,
    selected_device: Option<String>,
    status_line: String,
    view: View,
    /// The active recording name shown in the field + window title. Auto
    /// (heroku) when not pinned; "New session" refreshes it.
    name: String,
    /// Last window title pushed to the OS, to avoid re-sending every frame.
    title: String,
    /// In-app auto-update (background worker + shared state).
    updater: Updater,
}

impl RecorderApp {
    fn new() -> Self {
        let config_state = RecorderConfigState::new();
        let cfg = config_state.get().unwrap_or_default();
        let devices = list_input_devices().unwrap_or_default();
        let selected_device = cfg
            .default_device
            .clone()
            .filter(|d| devices.iter().any(|x| &x.id == d))
            .or_else(|| devices.iter().find(|d| d.is_default).map(|d| d.id.clone()));

        // Resolve the visible name: a pinned name if set, else a fresh auto one.
        let name = if cfg.recording_name.trim().is_empty() {
            recorder::auto_recording_name()
        } else {
            cfg.recording_name.clone()
        };

        let mut app = Self {
            capture: Capture::new(),
            config_state,
            cfg,
            devices,
            selected_device,
            status_line: "ready".into(),
            view: View::Recorder,
            name,
            title: String::new(),
            updater: Updater::new(),
        };
        if let Some(dev) = app.selected_device.clone() {
            app.open_device(&dev);
        }
        app
    }

    fn persist(&mut self) {
        if let Err(e) = self.config_state.set(self.cfg.clone()) {
            self.status_line = format!("config save failed: {e}");
        }
    }

    fn open_device(&mut self, id: &str) {
        match self.capture.select_device(id, Some(self.cfg.sample_rate)) {
            Ok(()) => {
                self.selected_device = Some(id.to_string());
                self.cfg.default_device = Some(id.to_string());
                if self.cfg.tracks.is_empty() {
                    self.cfg.tracks =
                        RecorderConfig::default_tracks(self.capture.channels(), false);
                }
                self.persist();
                self.status_line = format!(
                    "{} · {} ch · {} Hz",
                    id,
                    self.capture.channels(),
                    self.capture.sample_rate()
                );
            }
            Err(e) => self.status_line = format!("device error: {e}"),
        }
    }

    /// Start a fresh session name (a new heroku auto name), unpinned.
    fn new_session(&mut self) {
        self.name = recorder::auto_recording_name();
        self.cfg.recording_name = String::new(); // back to auto mode
        self.persist();
        self.status_line = format!("new session · {}", self.name);
    }

    /// Config for the next recording, with the currently-visible name applied.
    fn record_cfg(&self) -> RecorderConfig {
        let mut cfg = self.cfg.clone();
        cfg.recording_name = self.name.clone();
        cfg
    }

    /// REC — start capturing immediately, regardless of the master arm mode
    /// (forces the session to ON for this take).
    fn start_recording_now(&mut self) {
        let mut cfg = self.record_cfg();
        cfg.master_arm = ArmMode::On;
        match self.capture.start_session(&cfg) {
            Ok(()) => self.status_line = "recording".into(),
            Err(e) => self.status_line = format!("record failed: {e}"),
        }
    }

    /// ARM — start an armed session that respects the master arm mode: with
    /// AUTO it waits and begins capturing when the master crosses threshold.
    fn arm_recording(&mut self) {
        match self.capture.start_session(&self.record_cfg()) {
            Ok(()) => {
                self.status_line = if self.cfg.master_arm == ArmMode::Auto {
                    "armed · waiting for signal".into()
                } else {
                    "recording".into()
                }
            }
            Err(e) => self.status_line = format!("arm failed: {e}"),
        }
    }

    fn stop(&mut self) {
        match self.capture.stop_session() {
            Ok(()) => self.status_line = "finalizing session…".into(),
            Err(e) => self.status_line = format!("stop failed: {e}"),
        }
    }

    fn reveal_in_finder(&mut self) {
        let path = self
            .capture
            .status()
            .session_dir
            .unwrap_or_else(|| self.cfg.output_dir.clone());
        let _ = std::fs::create_dir_all(&path);
        if let Err(e) = std::process::Command::new("open").arg(&path).spawn() {
            self.status_line = format!("reveal failed: {e}");
        }
    }

    /// Approximate master meter level: the linear sum of non-muted track peaks
    /// scaled by their volume, back to dB. A monitor approximation of the mix.
    fn master_level_db(&self, levels: &[recorder::ChannelLevel]) -> f32 {
        let mut lin = 0.0_f32;
        for t in &self.cfg.tracks {
            if t.muted {
                continue;
            }
            let peak = t
                .channels
                .iter()
                .filter_map(|&c| levels.get(c as usize))
                .map(|l| l.peak_db)
                .fold(-96.0, f32::max);
            lin += 10f32.powf(peak / 20.0) * t.volume;
        }
        linear_to_db(lin)
    }
}

impl eframe::App for RecorderApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        install_theme(ctx);
        ctx.request_repaint_after(std::time::Duration::from_millis(33));

        // Keep the OS window title in sync with the active session name.
        let want_title = format!("LEMON record · {}", self.name);
        if want_title != self.title {
            self.title = want_title.clone();
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(want_title));
        }

        if !self.capture.is_recording() && self.capture.is_finalized() {
            if let Some(m) = self.capture.last_manifest() {
                if self.status_line.starts_with("finalizing") {
                    self.status_line = format!(
                        "saved · {} take(s) · {:.1}s · {}",
                        m.take_count(),
                        m.duration_secs(),
                        m.recording
                    );
                }
            }
        }

        let levels = self.capture.levels();

        egui::TopBottomPanel::top("header")
            .frame(egui::Frame::none().fill(theme::BG).inner_margin(12.0))
            .show(ctx, |ui| self.header(ui));

        egui::TopBottomPanel::bottom("footer")
            .frame(egui::Frame::none().fill(theme::BG).inner_margin(10.0))
            .show(ctx, |ui| self.footer(ui));

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(theme::BG).inner_margin(12.0))
            .show(ctx, |ui| match self.view {
                View::Recorder => {
                    self.transport(ui);
                    ui.add_space(10.0);
                    self.mixer(ui, &levels);
                }
                View::Settings => self.settings(ui),
            });
    }
}

impl RecorderApp {
    fn header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("LEMON").color(theme::LEMON).strong().size(16.0));
            ui.label(RichText::new("record").color(theme::TEXT).strong().size(16.0));
            if self.view == View::Settings {
                ui.label(RichText::new("▸ settings").color(theme::DIM).size(16.0));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (glyph, hint) = match self.view {
                    View::Recorder => ("⚙", "settings"),
                    View::Settings => ("‹ back", "back to recorder"),
                };
                if ui.button(RichText::new(glyph).color(theme::TEXT)).on_hover_text(hint).clicked() {
                    self.view = if self.view == View::Settings { View::Recorder } else { View::Settings };
                }
                if ui
                    .button(RichText::new("🗁").color(theme::TEXT))
                    .on_hover_text("reveal the latest recording in Finder")
                    .clicked()
                {
                    self.reveal_in_finder();
                }
                ui.label(
                    RichText::new(format!(
                        "{} Hz · {}-bit",
                        self.capture.sample_rate().max(self.cfg.sample_rate),
                        self.cfg.bit_depth
                    ))
                    .color(theme::DIM),
                );
            });
        });

        if self.view == View::Recorder {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("input").color(theme::DIM));
                let current = self.selected_device.clone().unwrap_or_else(|| "— none —".into());
                let mut chosen: Option<String> = None;
                egui::ComboBox::from_id_salt("device")
                    .selected_text(RichText::new(current).color(theme::TEXT))
                    .width(360.0)
                    .show_ui(ui, |ui| {
                        for d in &self.devices {
                            let label = format!("{}  ({}ch)", d.name, d.max_channels);
                            if ui
                                .selectable_label(self.selected_device.as_deref() == Some(&d.id), label)
                                .clicked()
                            {
                                chosen = Some(d.id.clone());
                            }
                        }
                    });
                if ui.button("⟳").on_hover_text("rescan devices").clicked() {
                    self.devices = list_input_devices().unwrap_or_default();
                }
                if let Some(id) = chosen {
                    self.open_device(&id);
                }
            });
        }
    }

    fn transport(&mut self, ui: &mut egui::Ui) {
        let recording = self.capture.is_recording();
        egui::Frame::none()
            .fill(theme::PANEL)
            .rounding(Rounding::same(5.0))
            .inner_margin(12.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let monitoring = self.capture.is_monitoring();
                    if recording {
                        let stop = egui::Button::new(
                            RichText::new("■  STOP").color(theme::TEXT).strong().size(15.0),
                        )
                        .min_size(Vec2::new(104.0, 38.0))
                        .fill(theme::REC);
                        if ui.add_enabled(monitoring, stop).clicked() {
                            self.stop();
                        }
                    } else {
                        // REC: record now. ARM: start when the master is armed.
                        let rec = egui::Button::new(
                            RichText::new("●  REC").color(theme::REC).strong().size(15.0),
                        )
                        .min_size(Vec2::new(96.0, 38.0))
                        .fill(theme::STRIP);
                        if ui
                            .add_enabled(monitoring, rec)
                            .on_hover_text("record now (ignores threshold)")
                            .clicked()
                        {
                            self.start_recording_now();
                        }
                        ui.add_space(6.0);
                        let armed_cfg = self.cfg.master_arm != ArmMode::Off;
                        let arm = egui::Button::new(
                            RichText::new("◉  ARM")
                                .color(if armed_cfg { theme::AMBER } else { theme::DIM })
                                .strong()
                                .size(15.0),
                        )
                        .min_size(Vec2::new(96.0, 38.0))
                        .fill(theme::STRIP);
                        if ui
                            .add_enabled(monitoring && armed_cfg, arm)
                            .on_hover_text(
                                "start an armed session — with master AUTO it begins\n\
                                 recording when the signal crosses the threshold",
                            )
                            .clicked()
                        {
                            self.arm_recording();
                        }
                    }
                    ui.add_space(12.0);
                    // Current take clock — counts while capturing, resets to
                    // 00:00 when the take closes (hold) while staying armed.
                    let capturing = self.capture.is_capturing();
                    let take = self.capture.take_secs();
                    let secs = take as u64;
                    ui.label(
                        RichText::new(format!("{:02}:{:02}", secs / 60, secs % 60))
                            .color(if capturing { theme::REC } else { theme::DIM })
                            .size(26.0),
                    );

                    // State notification next to the clock.
                    ui.add_space(8.0);
                    if recording {
                        let (note, color) = if capturing {
                            ("● rec", theme::REC)
                        } else {
                            ("◉ armed", theme::AMBER)
                        };
                        ui.label(RichText::new(note).color(color).strong().size(13.0));
                    }
                    let done = self.capture.takes_done();
                    if done > 0 {
                        ui.label(RichText::new(format!("✓ {done} saved")).color(theme::GREEN).size(11.0))
                            .on_hover_text("takes saved this session");
                    }
                });

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui
                        .button(RichText::new("⟲").color(theme::TEXT))
                        .on_hover_text("new session (fresh name)")
                        .clicked()
                    {
                        self.new_session();
                    }
                    ui.label(RichText::new("name").color(theme::DIM));
                    let mut name = self.name.clone();
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut name)
                            .desired_width(220.0)
                            .text_color(theme::GREEN),
                    );
                    if resp.changed() {
                        self.name = name;
                    }
                    if resp.lost_focus() {
                        // Empty → fresh auto name; otherwise pin what was typed.
                        if self.name.trim().is_empty() {
                            self.name = recorder::auto_recording_name();
                            self.cfg.recording_name = String::new();
                        } else {
                            self.cfg.recording_name = self.name.clone();
                        }
                        self.persist();
                    }
                    ui.label(RichText::new("· id+datetime added").color(theme::DIM).size(10.0))
                        .on_hover_text(
                            "Files: <name>-<id>-tNN-<track>.wav   Folder: <name>-<datetime>/\n\
                             Each take is uniquely named so you can drag it anywhere.",
                        );
                });
            });
    }

    fn mixer(&mut self, ui: &mut egui::Ui, levels: &[recorder::ChannelLevel]) {
        let recording = self.capture.is_recording();
        let master_db = self.master_level_db(levels);
        let multi = self.cfg.tracks.len() > 1;
        // Live state: None when idle, Some(capturing) when a session is active.
        let active = recording.then_some(self.capture.is_capturing());

        let mut dirty = false;

        // Master strip — always shown; carries the arm + threshold.
        dirty |= master_strip(
            ui,
            &mut self.cfg.master_arm,
            &mut self.cfg.master_threshold_db,
            master_db,
            active,
        );

        // Per-track mixer rows — only when more than one track.
        if multi {
            ui.add_space(8.0);
            ui.label(RichText::new(format!("TRACKS {}", self.cfg.tracks.len())).color(theme::DIM).size(11.0));
            ui.add_space(4.0);
            egui::ScrollArea::vertical().show(ui, |ui| {
                for track in self.cfg.tracks.iter_mut() {
                    let peak_db = track
                        .channels
                        .iter()
                        .filter_map(|&c| levels.get(c as usize))
                        .map(|l| l.peak_db)
                        .fold(-96.0, f32::max);
                    dirty |= track_strip(ui, track, peak_db);
                    ui.add_space(6.0);
                }
            });
        }

        if dirty {
            self.persist();
        }
    }

    fn settings(&mut self, ui: &mut egui::Ui) {
        let mut dirty = false;
        let mut reopen = false;

        egui::ScrollArea::vertical().show(ui, |ui| {
            section(ui, "AUDIO", |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("sample rate").color(theme::DIM));
                    egui::ComboBox::from_id_salt("sr")
                        .selected_text(format!("{} Hz", self.cfg.sample_rate))
                        .show_ui(ui, |ui| {
                            for &sr in SAMPLE_RATES {
                                if ui.selectable_value(&mut self.cfg.sample_rate, sr, format!("{sr} Hz")).clicked() {
                                    dirty = true;
                                    reopen = true;
                                }
                            }
                        });
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new("bit depth").color(theme::DIM));
                    egui::ComboBox::from_id_salt("bd")
                        .selected_text(format!("{}-bit", self.cfg.bit_depth))
                        .show_ui(ui, |ui| {
                            for &bd in BIT_DEPTHS {
                                let label = if bd == 32 { "32-bit float".into() } else { format!("{bd}-bit") };
                                if ui.selectable_value(&mut self.cfg.bit_depth, bd, label).clicked() {
                                    dirty = true;
                                }
                            }
                        });
                });
            });

            section(ui, "MASTER AUTO-ARM", |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("hold").color(theme::DIM)).on_hover_text(
                        "How long the master can stay below threshold before AUTO stops recording.",
                    );
                    ui.spacing_mut().slider_width = 240.0;
                    let mut secs = self.cfg.master_timeout_ms as f32 / 1000.0;
                    if ui.add(egui::Slider::new(&mut secs, 0.5..=15.0).suffix(" s")).changed() {
                        self.cfg.master_timeout_ms = (secs * 1000.0).round() as u32;
                        dirty = true;
                    }
                });
            });

            section(ui, "PATHS", |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("output").color(theme::DIM));
                    let mut out = self.cfg.output_dir.clone();
                    let resp = ui.add(egui::TextEdit::singleline(&mut out).desired_width(360.0).text_color(theme::TEXT));
                    if resp.changed() {
                        self.cfg.output_dir = out;
                    }
                    if resp.lost_focus() {
                        dirty = true;
                    }
                });
                if ui.button("Reveal in Finder").clicked() {
                    self.reveal_in_finder();
                }
            });

            section(ui, "NAMING", |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("recording name").color(theme::DIM));
                    let mut name = self.cfg.recording_name.clone();
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut name)
                            .desired_width(240.0)
                            .hint_text("auto (heroku-style per session)")
                            .text_color(theme::TEXT),
                    );
                    if resp.changed() {
                        self.cfg.recording_name = name;
                    }
                    if resp.lost_focus() {
                        dirty = true;
                    }
                });
                ui.add_space(4.0);
                ui.label(RichText::new("convention (fixed):").color(theme::DIM).size(11.0));
                ui.label(RichText::new("  folder   <name>-<datetime>/").color(theme::GREEN).size(11.0));
                ui.label(RichText::new("  master   <name>-<id>-master.wav").color(theme::GREEN).size(11.0));
                ui.label(RichText::new("  stems    <name>-<id>-<track>.wav  (when >1 track)").color(theme::GREEN).size(11.0));
                ui.label(
                    RichText::new("blank name → an auto heroku name (e.g. amber-cascade) each session")
                        .color(theme::DIM)
                        .size(10.0),
                );
            });

            section(ui, "SOFTWARE UPDATE", |ui| self.update_panel(ui));
        });

        if dirty {
            self.persist();
        }
        if reopen {
            if let Some(dev) = self.selected_device.clone() {
                self.open_device(&dev);
            }
        }
    }

    /// Auto-update controls. A `cargo run` build can't self-update (nothing to
    /// swap), so we show the version and explain rather than offer a no-op.
    fn update_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("version").color(theme::DIM));
            ui.label(RichText::new(format!("v{}", Updater::current_version())).color(theme::TEXT));
        });

        if !self.updater.is_bundled() {
            ui.add_space(4.0);
            ui.label(
                RichText::new("running unbundled (cargo) — auto-update is only active in the packaged app")
                    .color(theme::DIM)
                    .size(10.0),
            );
            return;
        }

        ui.add_space(6.0);
        match self.updater.state() {
            UpdateState::Idle | UpdateState::UpToDate | UpdateState::Error(_) => {
                if ui.button(RichText::new("Check for updates").color(theme::TEXT)).clicked() {
                    self.updater.check();
                }
            }
            UpdateState::Checking => {
                ui.label(RichText::new("checking…").color(theme::DIM).size(11.0));
            }
            UpdateState::Available(ref v) => {
                if ui
                    .button(RichText::new(format!("Download & install v{v}")).color(theme::LEMON).strong())
                    .clicked()
                {
                    self.updater.install();
                }
            }
            UpdateState::Downloading => {
                ui.label(RichText::new("downloading & installing…").color(theme::AMBER).size(11.0));
            }
            UpdateState::Ready(ref v) => {
                if ui
                    .button(RichText::new(format!("Relaunch into v{v}")).color(theme::GREEN).strong())
                    .clicked()
                {
                    self.updater.relaunch_and_exit();
                }
            }
        }

        // A line of feedback for the terminal states.
        ui.add_space(4.0);
        let (msg, color) = match self.updater.state() {
            UpdateState::UpToDate => ("up to date".to_string(), theme::DIM),
            UpdateState::Available(v) => (format!("v{v} available"), theme::LEMON),
            UpdateState::Ready(_) => ("installed — relaunch to finish".to_string(), theme::GREEN),
            UpdateState::Error(e) => (format!("update error: {e}"), theme::RED),
            _ => (String::new(), theme::DIM),
        };
        if !msg.is_empty() {
            ui.label(RichText::new(msg).color(color).size(10.0));
        }
    }

    fn footer(&mut self, ui: &mut egui::Ui) {
        // Status only — all actions live at the top.
        ui.horizontal(|ui| {
            ui.label(RichText::new(&self.status_line).color(theme::DIM).size(11.0));
        });
    }
}

fn section(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui)) {
    ui.label(RichText::new(title).color(theme::DIM).size(11.0));
    ui.add_space(2.0);
    egui::Frame::none()
        .fill(theme::STRIP)
        .rounding(Rounding::same(4.0))
        .inner_margin(10.0)
        .show(ui, |ui| body(ui));
    ui.add_space(10.0);
}

/// The master strip: arm-mode segmented control, threshold + hold (AUTO), and
/// the master meter. This is where recording is armed for the whole session.
fn master_strip(
    ui: &mut egui::Ui,
    arm: &mut ArmMode,
    threshold_db: &mut f32,
    peak_db: f32,
    active: Option<bool>,
) -> bool {
    let mut changed = false;
    egui::Frame::none()
        .fill(theme::PANEL)
        .rounding(Rounding::same(4.0))
        .inner_margin(8.0)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("MASTER").color(theme::TEXT).strong());
                // Live session state: ARMED = waiting below threshold; REC =
                // a take is capturing. Nothing when idle.
                if let Some(capturing) = active {
                    let (txt, color) = if capturing {
                        ("● REC", theme::REC)
                    } else {
                        ("◉ ARMED", theme::AMBER)
                    };
                    ui.label(RichText::new(txt).color(color).strong().size(11.0));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    for (mode, glyph) in [(ArmMode::Auto, "AUTO"), (ArmMode::On, "ON"), (ArmMode::Off, "OFF")] {
                        let selected = *arm == mode;
                        let color = match mode {
                            ArmMode::On => theme::GREEN,
                            ArmMode::Auto => theme::AMBER,
                            ArmMode::Off => theme::DIM,
                        };
                        let txt = if selected {
                            RichText::new(glyph).color(color).strong()
                        } else {
                            RichText::new(glyph).color(theme::DIM)
                        };
                        if ui.selectable_label(selected, txt).clicked() {
                            *arm = mode;
                            changed = true;
                        }
                    }
                });
            });

            ui.add_space(4.0);
            let tick = (*arm == ArmMode::Auto).then_some(*threshold_db);
            meter(ui, peak_db, tick, false);

            if *arm == ArmMode::Auto {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("record ≥").color(theme::DIM).size(10.0))
                        .on_hover_text("Recording starts when the master crosses this level.");
                    ui.spacing_mut().slider_width = 220.0;
                    if ui
                        .add(egui::Slider::new(threshold_db, -72.0..=-6.0).suffix(" dB").show_value(true))
                        .changed()
                    {
                        changed = true;
                    }
                });
            }
        });
    changed
}

/// One channel strip in the mixer: name, channel badge, mute, volume, meter.
/// A muted track is greyed and contributes to neither the master nor a stem.
fn track_strip(ui: &mut egui::Ui, track: &mut TrackConfig, peak_db: f32) -> bool {
    let mut changed = false;
    egui::Frame::none()
        .fill(theme::STRIP)
        .rounding(Rounding::same(4.0))
        .inner_margin(8.0)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut track.name)
                        .desired_width(120.0)
                        .hint_text("instrument")
                        .text_color(if track.muted { theme::DIM } else { theme::TEXT }),
                );
                changed |= resp.lost_focus() && resp.changed();

                let ch = track.channels.iter().map(|c| (c + 1).to_string()).collect::<Vec<_>>().join("+");
                ui.label(RichText::new(format!("ch {ch}")).color(theme::DIM).size(11.0));

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let m = if track.muted {
                        RichText::new("MUTE").color(theme::RED).strong()
                    } else {
                        RichText::new("MUTE").color(theme::DIM)
                    };
                    if ui.selectable_label(track.muted, m).on_hover_text("exclude from master + recording").clicked() {
                        track.muted = !track.muted;
                        changed = true;
                    }
                });
            });

            ui.add_space(4.0);
            meter(ui, peak_db, None, track.muted);

            ui.horizontal(|ui| {
                ui.label(RichText::new("vol").color(theme::DIM).size(10.0));
                ui.spacing_mut().slider_width = 230.0;
                // Greyed but still shown when muted.
                let resp = ui.add_enabled(
                    !track.muted,
                    egui::Slider::new(&mut track.volume, 0.0..=1.5).show_value(true),
                );
                if resp.changed() {
                    changed = true;
                }
            });
        });
    changed
}

/// Horizontal meter, -60..0 dBFS. `threshold` (Some) draws an amber tick at the
/// record level (master only). `dimmed` greys it (muted track).
fn meter(ui: &mut egui::Ui, peak_db: f32, threshold: Option<f32>, dimmed: bool) {
    let h = 14.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), h), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, Rounding::same(2.0), theme::METER_BG);

    let norm = |db: f32| ((db + 60.0) / 60.0).clamp(0.0, 1.0);
    let t = norm(peak_db);
    if t > 0.0 {
        let fill = Rect::from_min_size(rect.min, Vec2::new(rect.width() * t, rect.height()));
        let color = if dimmed {
            theme::DIM.linear_multiply(0.5)
        } else if peak_db >= -3.0 {
            theme::RED
        } else if peak_db >= -12.0 {
            theme::AMBER
        } else {
            theme::GREEN
        };
        painter.rect_filled(fill, Rounding::same(2.0), color);
    }

    for db in [-48.0, -36.0, -24.0, -12.0, -6.0] {
        let x = rect.min.x + rect.width() * norm(db);
        painter.line_segment([Pos2::new(x, rect.min.y), Pos2::new(x, rect.max.y)], Stroke::new(1.0, theme::GRID));
    }

    if let Some(th) = threshold {
        let x = rect.min.x + rect.width() * norm(th);
        painter.line_segment(
            [Pos2::new(x, rect.min.y - 1.0), Pos2::new(x, rect.max.y + 1.0)],
            Stroke::new(2.0, theme::AMBER),
        );
    }
}
