use crate::config::{Config, Model, launch_args};
use crate::server::{self, Health, Running};
use crate::{actions, opencode, tune};
use eframe::egui::{self, Color32, RichText};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Default)]
struct Snapshot {
    running: Option<Running>,
    health: Option<Health>,
    log: String,
}

#[derive(Default)]
struct TuneState {
    running: bool,
    lines: Vec<String>,
    result: Option<tune::Tuning>,
}

#[derive(PartialEq)]
enum View {
    Log,
    Tune,
}

struct App {
    cfg: Config,
    models: Vec<Model>,
    selected: usize,
    snap: Arc<Mutex<Snapshot>>,
    msg: Arc<Mutex<String>>,
    /// Name of the action currently running on a worker thread, shown on the button.
    busy: Arc<Mutex<Option<&'static str>>>,
    view: View,
    tune: Arc<Mutex<TuneState>>,
    cancel: Arc<AtomicBool>,
    tune_ctx: Vec<(u32, bool)>,
    headroom: u32,
}

pub fn run() -> anyhow::Result<()> {
    let cfg = Config::load()?;
    let snap = Arc::new(Mutex::new(Snapshot::default()));
    spawn_poller(cfg.clone(), snap.clone());

    let models = cfg.scan_models();
    let app = App {
        cfg,
        models,
        selected: 0,
        snap,
        msg: Arc::new(Mutex::new(String::new())),
        busy: Arc::new(Mutex::new(None)),
        view: View::Log,
        tune: Arc::new(Mutex::new(TuneState::default())),
        cancel: Arc::new(AtomicBool::new(false)),
        tune_ctx: tune::DEFAULT_CONTEXTS.iter().map(|&c| (c, c <= 131072)).collect(),
        headroom: tune::DEFAULT_HEADROOM_MB,
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([620.0, 560.0]).with_min_inner_size([480.0, 420.0]),
        ..Default::default()
    };
    eframe::run_native("LLM Switch", options, Box::new(|_cc| Ok(Box::new(app))))
        .map_err(|e| anyhow::anyhow!("{e}"))
}

fn spawn_poller(cfg: Config, snap: Arc<Mutex<Snapshot>>) {
    std::thread::spawn(move || {
        let mut sys = server::new_system();
        loop {
            let running = server::running(&mut sys);
            let health = running.as_ref().map(|_| server::health(&cfg));
            let log = if running.is_some() { server::log_tail(&cfg, 40) } else { String::new() };
            *snap.lock().unwrap() = Snapshot { running, health, log };
            std::thread::sleep(Duration::from_millis(500));
        }
    });
}

impl App {
    /// Runs `job` on a worker thread so the window never freezes while a server stops or starts.
    fn spawn(&self, what: &'static str, job: impl FnOnce() -> anyhow::Result<String> + Send + 'static) {
        let (msg, busy) = (self.msg.clone(), self.busy.clone());
        *busy.lock().unwrap() = Some(what);
        *msg.lock().unwrap() = "working...".into();
        std::thread::spawn(move || {
            let text = match job() {
                Ok(t) => t,
                Err(e) => format!("error: {e:#}"),
            };
            *msg.lock().unwrap() = text;
            *busy.lock().unwrap() = None;
        });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        ui.ctx().request_repaint_after(Duration::from_millis(500));
        egui::Frame::new().inner_margin(egui::Margin::same(12)).show(ui, |ui| self.draw(ui));
    }
}

impl App {
    fn draw(&mut self, ui: &mut egui::Ui) {
        let (running, health, log) = {
            let s = self.snap.lock().unwrap();
            (s.running.clone(), s.health, s.log.clone())
        };
        let tuning = self.tune.lock().unwrap().running;
        let job = *self.busy.lock().unwrap();
        let busy = job.is_some() || tuning;
        let selected = self.models.get(self.selected).cloned();

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let (color, text) = match (&running, health) {
                (None, _) => (Color32::GRAY, "Stopped".to_string()),
                (Some(r), Some(Health::Ready)) => (Color32::from_rgb(60, 190, 90), format!("Ready  (pid {})", r.pid)),
                (Some(r), _) => (Color32::from_rgb(230, 170, 40), format!("Loading...  (pid {})", r.pid)),
            };
            let (dot, _) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
            ui.painter().circle_filled(dot.center(), 7.0, color);
            ui.label(RichText::new(text).size(18.0).strong());
        });
        if let Some(path) = running.as_ref().and_then(|r| r.model_path.as_ref()) {
            let file = path.rsplit(['\\', '/']).next().unwrap_or(path);
            ui.label(format!("Serving: {file}   at {}", self.cfg.base_url()));
        }
        ui.separator();

        ui.label(RichText::new(format!("Models in {}", self.cfg.models_dir.display())).strong());
        if self.models.is_empty() {
            ui.colored_label(Color32::LIGHT_RED, "No .gguf files found. Check models_dir in the config.");
        }
        for (i, m) in self.models.iter().enumerate() {
            let is_running = running.as_ref().is_some_and(|r| server::is_model(r, m));
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.selected, i, format!("{}   ({:.1} GB)", m.name, m.size_gb));
                if is_running {
                    ui.label(RichText::new("running").color(Color32::from_rgb(60, 190, 90)));
                }
                if !m.configured {
                    ui.label(RichText::new("default settings").weak());
                }
            });
        }
        ui.add_space(6.0);

        if let Some(m) = &selected {
            let s = &m.settings;
            ui.group(|ui| {
                ui.label(RichText::new(format!("Settings for {}", m.file)).strong());
                ui.label(format!(
                    "context {}   |   GPU layers {}   |   experts on CPU {}   |   flash-attn {}   |   KV cache {}/{}",
                    s.ctx,
                    s.ngl,
                    s.n_cpu_moe.map_or("-".to_string(), |n| n.to_string()),
                    if s.flash_attn { "on" } else { "off" },
                    s.cache_type_k.as_deref().unwrap_or("f16"),
                    s.cache_type_v.as_deref().unwrap_or("f16"),
                ));
                ui.label(format!(
                    "OpenCode: {}/{}   |   auto-sync {}",
                    self.cfg.opencode.provider_id,
                    m.opencode_id,
                    if self.cfg.opencode.auto_sync { "on" } else { "off" }
                ));
            });
        }
        ui.add_space(6.0);

        ui.horizontal(|ui| {
            let is_this_running = match (&running, &selected) {
                (Some(r), Some(m)) => server::is_model(r, m),
                _ => false,
            };
            let label = job.unwrap_or(if is_this_running { "Stop" } else if running.is_some() { "Switch to this model" } else { "Start" });
            // The button does exactly what its label says. It never toggles on whatever state it finds
            // when the click lands, so a late second click cannot undo a stop by starting the server again.
            if ui.add_enabled(!busy && selected.is_some(), egui::Button::new(RichText::new(label).size(16.0)).min_size(egui::vec2(150.0, 32.0))).clicked() {
                let (cfg, m) = (self.cfg.clone(), selected.clone().unwrap());
                if is_this_running {
                    self.spawn("Stopping...", move || actions::stop_server());
                } else {
                    self.spawn("Starting...", move || actions::start_model(&cfg, &m));
                }
            }
            if ui.add_enabled(!busy && running.is_some(), egui::Button::new("Restart")).clicked() {
                if let Some(m) = selected.clone() {
                    let cfg = self.cfg.clone();
                    self.spawn("Restarting...", move || actions::start_model(&cfg, &m));
                }
            }
            if ui.add_enabled(!busy && selected.is_some(), egui::Button::new("Sync OpenCode")).clicked() {
                let (cfg, m) = (self.cfg.clone(), selected.clone().unwrap());
                self.spawn("Syncing...", move || opencode::sync(&cfg, &m));
            }
            if ui.button("Reload config").clicked() {
                match Config::load() {
                    Ok(c) => {
                        self.cfg = c;
                        self.models = self.cfg.scan_models();
                        self.selected = self.selected.min(self.models.len().saturating_sub(1));
                        *self.msg.lock().unwrap() = "config reloaded".into();
                    }
                    Err(e) => *self.msg.lock().unwrap() = format!("error: {e:#}"),
                }
            }
            if ui.button("Open config").clicked() {
                crate::platform::open_in_editor(&Config::path());
            }
        });

        let msg = self.msg.lock().unwrap().clone();
        if !msg.is_empty() {
            ui.label(RichText::new(msg).color(Color32::LIGHT_BLUE));
        }
        if let Some(m) = &selected {
            ui.collapsing("Launch command", |ui| {
                ui.add(egui::Label::new(
                    RichText::new(format!("{} {}", self.cfg.llama_server.display(), launch_args(&self.cfg, m).join(" "))).monospace(),
                ));
            });
        }
        ui.separator();
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.view, View::Log, "Server log");
            ui.selectable_value(&mut self.view, View::Tune, "Auto-tune");
        });
        match self.view {
            View::Log => {
                egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
                    ui.add(egui::Label::new(RichText::new(if log.is_empty() { "(no log yet)".to_string() } else { log }).monospace().size(11.0)));
                });
            }
            View::Tune => self.draw_tune(ui, selected.as_ref(), tuning),
        }
    }

    fn start_tune(&self, model: Model) {
        let contexts: Vec<u32> = self.tune_ctx.iter().filter(|c| c.1).map(|c| c.0).collect();
        let opts = tune::Options { contexts, headroom_mb: self.headroom };
        let (cfg, state, cancel) = (self.cfg.clone(), self.tune.clone(), self.cancel.clone());
        cancel.store(false, Ordering::Relaxed);
        {
            let mut s = state.lock().unwrap();
            *s = TuneState { running: true, ..Default::default() };
        }
        std::thread::spawn(move || {
            let st = state.clone();
            let log = move |l: &str| st.lock().unwrap().lines.push(l.to_string());
            let res = actions::tune_and_restore(&cfg, &model, &opts, &log, &cancel);
            let mut s = state.lock().unwrap();
            match res {
                Ok(r) => {
                    s.lines.push("finished".into());
                    s.result = Some(r);
                }
                Err(e) => s.lines.push(format!("stopped: {e:#}")),
            }
            s.running = false;
        });
    }

    fn draw_tune(&mut self, ui: &mut egui::Ui, selected: Option<&Model>, tuning: bool) {
        ui.label(
            RichText::new(
                "Restarts the server with trial settings, measures real VRAM use and speed, and recommends the best fit. \
                 Takes a few minutes; your server is restored afterwards.",
            )
            .weak(),
        );
        ui.horizontal(|ui| {
            ui.label("Contexts:");
            for (ctx, on) in &mut self.tune_ctx {
                ui.add_enabled(!tuning, egui::Checkbox::new(on, format!("{}K", *ctx / 1024)));
            }
        });
        ui.add_enabled(!tuning, egui::Slider::new(&mut self.headroom, 300..=2000).text("VRAM headroom (MiB)"));
        ui.horizontal(|ui| {
            let can_run = !tuning && selected.is_some() && self.tune_ctx.iter().any(|c| c.1);
            if ui.add_enabled(can_run, egui::Button::new("Run auto-tune")).clicked() {
                if let Some(m) = selected {
                    self.start_tune(m.clone());
                }
            }
            if ui.add_enabled(tuning, egui::Button::new("Cancel")).clicked() {
                self.cancel.store(true, Ordering::Relaxed);
            }
        });

        let (lines, result) = {
            let s = self.tune.lock().unwrap();
            (s.lines.join("\n"), s.result.clone())
        };
        if let Some(r) = &result {
            ui.separator();
            egui::Grid::new("tune_results").striped(true).show(ui, |ui| {
                for h in ["Context", r.knob_name(), "VRAM MiB", "Decode tok/s", "Prefill tok/s", ""] {
                    ui.label(RichText::new(h).strong());
                }
                ui.end_row();
                for t in &r.trials {
                    let rec = r.recommended.as_ref().is_some_and(|x| x.ctx == t.ctx);
                    ui.label(t.ctx.to_string());
                    ui.label(t.knob.to_string());
                    ui.label(t.vram_mb.to_string());
                    ui.label(format!("{:.1}", t.decode_tps));
                    ui.label(format!("{:.0}", t.prefill_tps));
                    ui.label(if rec { RichText::new("recommended").color(Color32::from_rgb(60, 190, 90)) } else { RichText::new("") });
                    ui.end_row();
                }
            });
            for (ctx, why) in &r.skipped {
                ui.label(RichText::new(format!("{ctx}: skipped, {why}")).weak());
            }
            if let Some(rec) = &r.recommended {
                if ui.button(format!("Apply recommended (context {}, {} {})", rec.ctx, r.knob_name(), rec.knob)).clicked() {
                    match self.cfg.apply_tuning(&r.file, rec.ctx, r.moe, rec.knob) {
                        Ok(()) => {
                            self.models = self.cfg.scan_models();
                            *self.msg.lock().unwrap() = "saved to config; press Restart to use it".into();
                        }
                        Err(e) => *self.msg.lock().unwrap() = format!("error: {e:#}"),
                    }
                }
            }
        }
        egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
            ui.add(egui::Label::new(RichText::new(lines).monospace().size(11.0)));
        });
    }
}

/// Shows a startup error where the user will see it (the Windows release build has no console).
pub fn fatal(text: &str) {
    crate::platform::show_error(text);
}
