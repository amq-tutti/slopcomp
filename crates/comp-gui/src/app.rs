use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::Instant;

use comp_core::comp::{CompResult, Event};
use comp_core::pairing::Pairing;
use comp_core::project::Project;
use comp_core::season::{self, SeasonEvent, SeasonResult};
use eframe::egui::{self, RichText, TextureHandle, TextureOptions};

use crate::preview::{self, PreviewImage, Reply, Request, SourceConfig};
use crate::theme;
use crate::setup::{label_for, random_seed, Field, Order, Setup, SourceRow, MATRICES};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Select,
    Sync,
    Create,
    Viewer,
}

struct SyncState {
    episode: usize,
    sent: Option<Vec<SourceConfig>>,
    ready: Option<(i32, Vec<f64>)>,
    frame: i32,
    last_req: Option<(i32, Vec<(f64, i32)>, bool, bool)>,
    status: String,
    error: Option<String>,
    images: Vec<Result<(TextureHandle, i32, char, (u32, u32)), String>>,
    flicker: Option<usize>,
    /// Offset suggestions per source index (None while running).
    suggestions: std::collections::HashMap<usize, Option<Result<comp_core::sync::SyncSuggestion, String>>>,
    /// The last finished suggestion per source, still drawn while its row fades out.
    last_suggestion: std::collections::HashMap<usize, Result<comp_core::sync::SyncSuggestion, String>>,
    /// The last non-empty status, still drawn while the status line fades out.
    status_shown: String,
}

enum RunMsg {
    Event(SeasonEvent),
    Done(Result<SeasonResult, String>),
}

struct SourceProgress {
    label: String,
    indexing: f32,
    analysis: Option<f32>,
    opened: bool,
}

/// One comparison being (or having been) created. Kept for the session as history.
struct Job {
    /// "Name · N episodes", fixed when the job starts.
    title: String,
    rx: Receiver<RunMsg>,
    cancel: Arc<AtomicBool>,
    started: Instant,
    log: Vec<String>,
    episode: (usize, usize, String),
    sources: Vec<SourceProgress>,
    shots: (usize, usize),
    results: Vec<CompResult>,
    failed: Vec<(String, String)>,
    finished: Option<Result<(), String>>,
    elapsed: f64,
    /// Local clock time the job ended ("14:02"), shown in the history.
    finished_at: Option<String>,
}

pub struct App {
    setup: Setup,
    tab: Tab,
    pairing: Option<(Vec<String>, Result<Pairing, String>)>,
    worker: preview::Worker,
    sync: SyncState,
    /// This session's jobs, oldest first; only the last can still be running.
    jobs: Vec<Job>,
    toast: Option<(String, bool, Instant)>,
    project_path: Option<PathBuf>,
    /// Developer flag --auto-sync: request offset suggestions once the sync sources are open.
    auto_sync: bool,
    viewer: crate::viewer::Viewer,
}

const STORAGE_KEY: &str = "setup";

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_fonts(&cc.egui_ctx);
        crate::theme::apply(&cc.egui_ctx);
        let mut setup = cc.storage.and_then(|s| eframe::get_value::<Setup>(s, STORAGE_KEY)).unwrap_or_default();
        // `slopcomp project.toml` opens a project.
        let mut project_path = None;
        if let Some(arg) = std::env::args().skip(1).find(|a| a.ends_with(".toml")) {
            let p = PathBuf::from(arg);
            if let Ok(s) = Project::load(&p).map_err(|e| e.to_string()).and_then(|pr| Setup::from_project(&pr)) {
                setup = s;
                project_path = Some(p);
            }
        }
        // Developer flags: --tab=select|sync|create|viewer, --start (begin a run immediately),
        // --auto-sync (suggest offsets once the sync view has opened its sources).
        let args: Vec<String> = std::env::args().collect();
        let tab = match args.iter().find_map(|a| a.strip_prefix("--tab=")) {
            Some("sync") => Tab::Sync,
            Some("create" | "jobs" | "run") => Tab::Create,
            Some("viewer") => Tab::Viewer,
            _ => Tab::Select,
        };
        let autostart = args.iter().any(|a| a == "--start");
        let ctx = cc.egui_ctx.clone();
        let mut app = Self {
            setup,
            tab,
            pairing: None,
            worker: preview::Worker::spawn(move || ctx.request_repaint()),
            sync: SyncState {
                episode: 0,
                sent: None,
                ready: None,
                frame: 0,
                last_req: None,
                status: String::new(),
                error: None,
                images: Vec::new(),
                flicker: None,
                suggestions: Default::default(),
                last_suggestion: Default::default(),
                status_shown: String::new(),
            },
            jobs: Vec::new(),
            toast: None,
            project_path,
            auto_sync: args.iter().any(|a| a == "--auto-sync"),
            viewer: crate::viewer::Viewer::new(&cc.egui_ctx),
        };

        app.tab = tab;
        // Developer flag --view=DIR (repeatable): open comparisons in the viewer; the last one shows.
        for d in args.iter().filter_map(|a| a.strip_prefix("--view=")) {
            app.viewer.open_folder(Path::new(d));
            app.tab = Tab::Viewer;
        }
        if autostart && app.setup.problems().is_empty() {
            app.start_run(&cc.egui_ctx, None);
        }
        app
    }

    fn notify(&mut self, msg: impl Into<String>, error: bool) {
        self.toast = Some((msg.into(), error, Instant::now()));
    }

    fn running(&self) -> bool {
        self.jobs.last().is_some_and(|j| j.finished.is_none())
    }

    // ---------------------------------------------------------------- inputs

    fn add_paths(&mut self, paths: Vec<PathBuf>) {
        for p in paths {
            let taken: Vec<String> = self.setup.sources.iter().map(|s| s.label.clone()).collect();
            let label = label_for(&p, &taken);
            self.setup.sources.push(SourceRow::new(label, p.to_string_lossy().into_owned()));
        }
    }

    fn handle_drops(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        if !dropped.is_empty() && !self.running() {
            self.add_paths(dropped);
            self.tab = Tab::Select;
        }
    }

    fn pairing(&mut self) -> Option<&Result<Pairing, String>> {
        let key: Vec<String> = self.setup.sources.iter().map(|s| s.path.clone()).collect();
        if key.len() < 2 {
            return None;
        }
        if self.pairing.as_ref().is_none_or(|(k, _)| *k != key) {
            let paths: Vec<PathBuf> = key.iter().map(PathBuf::from).collect();
            let res = season::plan(&paths).map(|(_, p)| p).map_err(|e| e.to_string());
            self.pairing = Some((key, res));
        }
        self.pairing.as_ref().map(|(_, r)| r)
    }

    // ---------------------------------------------------------------- project files

    fn save_project(&mut self, ask: bool) {
        let path = match (&self.project_path, ask) {
            (Some(p), false) => Some(p.clone()),
            _ => rfd::FileDialog::new()
                .add_filter("SlopComp project", &["toml"])
                .set_file_name(format!("{}.toml", comp_core::comp::folder_name(&self.setup.display_name())))
                .save_file(),
        };
        if let Some(p) = path {
            match self.setup.to_project().save(&p) {
                Ok(()) => {
                    self.notify(format!("Saved {}", p.display()), false);
                    self.project_path = Some(p);
                }
                Err(e) => self.notify(format!("Couldn't save: {e}"), true),
            }
        }
    }

    /// Clears sources, name and offsets; keeps settings (output folder, frames, rules), the job
    /// history and the comparisons open in the viewer (those are finished folders on disk).
    fn clear_comparison(&mut self) {
        self.setup.sources.clear();
        self.setup.name.clear();
        self.setup.frame_type = None;
        self.setup.seed = random_seed();
        self.setup.skip_episodes.clear();
        self.project_path = None;
        self.pairing = None;
        self.sync.episode = 0;
        self.sync.frame = 0;
        self.sync.sent = None;
        self.sync.ready = None;
        self.sync.images.clear();
        self.sync.suggestions.clear();
        self.sync.last_suggestion.clear();
        self.sync.error = None;
        self.sync.status.clear();
        self.tab = Tab::Select;
        self.notify("Cleared sources, name, and offsets. Settings kept.", false);
    }

    fn open_project(&mut self) {
        if let Some(p) = rfd::FileDialog::new().add_filter("SlopComp project", &["toml"]).pick_file() {
            match Project::load(&p).map_err(|e| e.to_string()).and_then(|pr| Setup::from_project(&pr)) {
                Ok(s) => {
                    self.setup = s;
                    self.project_path = Some(p.clone());
                    self.notify(format!("Opened {}", p.display()), false);
                }
                Err(e) => self.notify(format!("Couldn't open project: {e}"), true),
            }
        }
    }

    // ---------------------------------------------------------------- run

    /// Complete episodes (present in every source) when sources are folders; empty otherwise.
    fn complete_episodes(&mut self) -> Vec<String> {
        match self.pairing() {
            Some(Ok(p)) if p.rows.len() > 1 => p.complete().map(|r| r.episode.to_string()).collect(),
            _ => Vec::new(),
        }
    }

    /// Ticked episodes: None when everything is ticked (or sources aren't folders).
    fn selected_episodes(&mut self) -> Option<Vec<String>> {
        let all = self.complete_episodes();
        if all.is_empty() {
            return None;
        }
        let sel: Vec<String> = all.iter().filter(|e| !self.setup.skip_episodes.contains(e)).cloned().collect();
        if sel.len() == all.len() { None } else { Some(sel) }
    }

    fn start_run(&mut self, ctx: &egui::Context, only: Option<Vec<String>>) {
        let cancel = Arc::new(AtomicBool::new(false));
        let spec = self.setup.to_spec(Some(cancel.clone()));
        let (tx, rx) = mpsc::channel();
        let ctx2 = ctx.clone();
        let only_thread = only.clone();
        std::thread::Builder::new()
            .name("comparison".into())
            .spawn(move || {
                let send = |m: RunMsg| {
                    let _ = tx.send(m);
                    ctx2.request_repaint();
                };
                let res = season::run(&spec, only_thread.as_deref(), &|e| send(RunMsg::Event(e)));
                send(RunMsg::Done(res.map_err(|e| e.to_string())));
            })
            .expect("spawn comparison thread");
        let n = only.as_ref().map(|v| v.len()).unwrap_or_else(|| self.complete_episodes().len());
        let title = match &only {
            Some(v) if v.len() == 1 => format!("{} · {}", self.setup.display_name(), v[0]),
            _ if n > 1 => format!("{} · {n} episodes", self.setup.display_name()),
            _ => self.setup.display_name(),
        };
        self.jobs.push(Job {
            title,
            rx,
            cancel,
            started: Instant::now(),
            log: Vec::new(),
            episode: (0, 1, String::new()),
            sources: self
                .setup
                .sources
                .iter()
                .map(|s| SourceProgress {
                    label: s.label.clone(),
                    indexing: 0.0,
                    analysis: (s.field != Field::Progressive).then_some(0.0),
                    opened: false,
                })
                .collect(),
            shots: (0, 0),
            results: Vec::new(),
            failed: Vec::new(),
            finished: None,
            elapsed: 0.0,
            finished_at: None,
        });
        self.tab = Tab::Create;
    }

    fn pump_run(&mut self) {
        let Some(run) = self.jobs.last_mut() else { return };
        while let Ok(msg) = run.rx.try_recv() {
            match msg {
                RunMsg::Event(e) => match e {
                    SeasonEvent::Paired(p) => {
                        run.log.push(stamped(format!("{} of {} episodes found in every source", p.complete().count(), p.rows.len())))
                    }
                    SeasonEvent::EpisodeStarted { index, total, episode } => {
                        run.episode = (index, total, episode.clone());
                        run.shots = (0, 0);
                        for s in &mut run.sources {
                            s.indexing = 0.0;
                            s.opened = false;
                            if let Some(a) = s.analysis.as_mut() {
                                *a = 0.0;
                            }
                        }
                        if !episode.is_empty() {
                            run.log.push(stamped(format!("== {episode} ==")));
                        }
                    }
                    SeasonEvent::Episode { event, .. } => match event {
                        Event::Indexing { source, done, total } if total > 0 => {
                            run.sources[source].indexing = done as f32 / total as f32;
                        }
                        Event::Analyzing { source, done, total } if total > 0 => {
                            run.sources[source].indexing = 1.0;
                            run.sources[source].analysis = Some(done as f32 / total as f32);
                        }
                        Event::Opened { source, info, vfr } => {
                            let s = &mut run.sources[source];
                            s.opened = true;
                            s.indexing = 1.0;
                            if let Some(a) = s.analysis.as_mut() {
                                *a = 1.0;
                            }
                            run.log.push(stamped(format!(
                                "{}: {}×{} {}-bit, {}, {} frames, {}",
                                s.label,
                                info.width,
                                info.height,
                                info.layout.bit_depth,
                                // With variable frame rate the stream's rate is only an average.
                                if vfr {
                                    format!("VFR (avg {} fps)", fps_text(info.fps_num, info.fps_den))
                                } else {
                                    format!("{} fps CFR", fps_text(info.fps_num, info.fps_den))
                                },
                                info.num_frames,
                                info.container
                            )));
                        }
                        Event::Excluded { ranges } => {
                            for (a, b, why) in ranges {
                                run.log.push(stamped(format!("skipping {:.1}–{:.1} s: {why}", a, b)));
                            }
                        }
                        Event::FramesPicked { frames, .. } => {
                            run.shots = (0, frames.len() * run.sources.len());
                            run.log.push(stamped(format!("picked {} frames", frames.len())));
                        }
                        Event::Shot { .. } => run.shots.0 += 1,
                        _ => {}
                    },
                    SeasonEvent::EpisodeDone { result, .. } => {
                        run.log.push(stamped(format!("saved to {}", result.out_dir.display())));
                        self.viewer.add(&result.out_dir);
                        run.results.push(*result);
                    }
                    SeasonEvent::EpisodeFailed { episode, error, .. } => {
                        run.log.push(stamped(format!("{episode} failed: {error}")));
                        run.failed.push((episode, error));
                    }
                    SeasonEvent::IndexWritten(_) => {}
                },
                RunMsg::Done(res) => {
                    run.elapsed = run.started.elapsed().as_secs_f64();
                    run.finished_at = Some(chrono::Local::now().format("%H:%M").to_string());
                    match res {
                        Ok(_) => {
                            run.finished = Some(Ok(()));
                        }
                        Err(e) => {
                            run.log.push(stamped(format!("stopped: {e}")));
                            run.finished = Some(Err(e));
                        }
                    }
                }
            }
        }
    }
}

/// IBM Plex Sans for UI text, IBM Plex Mono for monospace, IBM Plex Sans JP as the fallback for
/// Japanese (file names, chapter titles), then egui's built-in fonts for any remaining symbols.
fn install_fonts(ctx: &egui::Context) {
    use eframe::egui::{FontData, FontDefinitions, FontFamily};
    let mut fonts = FontDefinitions::default();
    for (name, bytes) in [
        ("instrument-sans", &include_bytes!("../fonts/InstrumentSans-Regular.ttf")[..]),
        ("plex-mono", &include_bytes!("../fonts/IBMPlexMono-Regular.ttf")[..]),
        ("plex-sans-jp", &include_bytes!("../fonts/IBMPlexSansJP-Regular.ttf")[..]),
    ] {
        fonts.font_data.insert(name.to_owned(), Arc::new(FontData::from_static(bytes)));
    }
    let prop = fonts.families.entry(FontFamily::Proportional).or_default();
    // Instrument Sans for the UI; IBM Plex Sans JP behind it for Japanese file names and titles.
    prop.insert(0, "instrument-sans".to_owned());
    prop.insert(1, "plex-sans-jp".to_owned());
    let mono = fonts.families.entry(FontFamily::Monospace).or_default();
    mono.insert(0, "plex-mono".to_owned());
    mono.insert(1, "plex-sans-jp".to_owned());
    ctx.set_fonts(fonts);
}

/// A log line with the local clock time in front: "14:02:31 picked 15 frames".
fn stamped(msg: impl Into<String>) -> String {
    format!("{} {}", chrono::Local::now().format("%H:%M:%S"), msg.into())
}

/// Splits a stamped log line into its time and message (lines without a time return None).
fn split_stamp(line: &str) -> (Option<&str>, &str) {
    let b = line.as_bytes();
    if b.len() > 9 && b[2] == b':' && b[5] == b':' && b[8] == b' ' {
        (Some(&line[..8]), &line[9..])
    } else {
        (None, line)
    }
}

/// Frame rate for display: "23.976", "25", "29.97" (up to three decimals, trailing zeros dropped).
fn fps_text(num: i32, den: i32) -> String {
    if num <= 0 || den <= 0 {
        return "?".into();
    }
    let s = format!("{:.3}", num as f64 / den as f64);
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// "SlopComp <version>", shown on hover over the logo and name and under the shortcuts list.
pub const VERSION_LABEL: &str = concat!("SlopComp ", env!("CARGO_PKG_VERSION"));

/// Keyboard shortcuts by screen and view mode, for the "?" list in the top bar. The current screen's
/// groups come first. One action per line.
fn shortcut_groups(tab: Tab) -> Vec<theme::ShortcutGroup> {
    use theme::ShortcutGroup as G;
    let (sync, viewer) = (tab == Tab::Sync, tab == Tab::Viewer);
    let mut groups = vec![
        G { title: "Sync", current: sync, sub: false, rows: vec![(vec!["←", "→"], "Previous or next frame")] },
        G {
            title: "Sync · Flicker",
            current: sync,
            sub: true,
            rows: vec![(vec!["1", "–", "5"], "Show that source"), (vec!["Space"], "Show the next source")],
        },
        G {
            title: "Viewer",
            current: viewer,
            sub: false,
            rows: vec![
                (vec!["←", "→"], "Previous or next frame"),
                (vec!["G", "S", "F"], "Side by side, Slider, Flicker"),
                (vec!["scroll"], "Zoom all panels together"),
                (vec!["drag"], "Pan when zoomed in"),
                (vec!["0"], "Fit the frame to the window"),
            ],
        },
        G {
            title: "Viewer · Slider",
            current: viewer,
            sub: true,
            rows: vec![
                (vec!["1", "–", "9"], "Put that source on the right"),
                (vec!["Shift", "1", "–", "9"], "Put that source on the left"),
                (vec!["drag"], "Move the divider"),
            ],
        },
        G {
            title: "Viewer · Flicker",
            current: viewer,
            sub: true,
            rows: vec![
                (vec!["1", "–", "9"], "Show that source (from Side by side too)"),
                (vec!["Space"], "Show the next source"),
            ],
        },
    ];
    // Stable sort: the current screen's groups move to the top and keep their order.
    groups.sort_by_key(|g| !g.current);
    groups.push(G { title: "Anywhere", current: false, sub: false, rows: vec![(vec!["?"], "Show or hide this list")] });
    groups
}

pub fn open_external(target: &Path) {
    let t = target.as_os_str();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(t).spawn();
    #[cfg(target_os = "windows")]
    let r = std::process::Command::new("explorer").arg(t).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(t).spawn();
    let _ = r;
}

pub fn open_url(url: &str) {
    open_external(Path::new(url));
}

impl eframe::App for App {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, STORAGE_KEY, &self.setup);
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_drops(ctx);
        self.pump_run();
        self.pump_preview(ctx);

        let bar = egui::Frame::new()
            .fill(theme::BAR)
            .inner_margin(egui::Margin::symmetric(16, theme::BAR_PAD_Y as i8))
            .stroke(egui::Stroke::new(1.0, theme::BORDER));
        egui::TopBottomPanel::top("top").frame(bar).show(ctx, |ui| {
            ui.horizontal(|ui| {
                // Full row height before anything is placed, so the logo and name centre on the same
                // line as the 32 pt tabs instead of on egui's default 26 pt row.
                ui.set_min_height(32.0);
                // Narrow windows: first drop the name next to the logo, then fold the project
                // buttons into a menu. Decided from the widths the parts took last frame.
                let fit_id = egui::Id::new("top_bar_fit");
                let (prev_tabs, prev_buttons) = ui.ctx().data(|d| d.get_temp::<(f32, f32)>(fit_id)).unwrap_or((600.0, 290.0));
                let title_font = egui::FontId::proportional(16.0);
                let title_w = ui.fonts(|f| f.layout_no_wrap("SlopComp".into(), title_font.clone(), theme::TEXT).size().x) + 8.0;
                let avail = ui.available_width();
                let show_title = avail >= prev_tabs + title_w + prev_buttons + 24.0;
                let menu = avail < prev_tabs + prev_buttons + 24.0;
                let start_x = ui.cursor().min.x;
                let logo = theme::logo(ui);
                logo.on_hover_text(VERSION_LABEL);
                if show_title {
                    ui.label(RichText::new("SlopComp").font(title_font).color(theme::TEXT_BRIGHT)).on_hover_text(VERSION_LABEL);
                }
                ui.add_space(if show_title { 18.0 } else { 8.0 });
                let running = self.running();
                // Numbered tabs are the steps to make a comparison; the viewer stands apart (it also
                // opens earlier comparisons) and stays usable while a job runs.
                ui.spacing_mut().item_spacing.x = 2.0;
                let steps = [(Tab::Select, 1, "Select"), (Tab::Sync, 2, "Sync"), (Tab::Create, 3, "Create")];
                for (k, (tab, n, name)) in steps.into_iter().enumerate() {
                    if k > 0 {
                        theme::step_link(ui);
                    }
                    let enabled = !(running && tab != Tab::Create);
                    let r = theme::step_tab(ui, Some(n), name, self.tab == tab, enabled);
                    let r = if enabled { r } else { r.on_hover_text("Locked while a comparison is being created") };
                    if r.clicked() {
                        self.tab = tab;
                    }
                }
                ui.add_space(14.0);
                let (sep, _) = ui.allocate_exact_size(egui::vec2(1.0, 22.0), egui::Sense::hover());
                ui.painter().rect_filled(sep, 0.0, theme::BORDER);
                ui.add_space(14.0);
                if theme::step_tab(ui, None, "Viewer", self.tab == Tab::Viewer, true).clicked() {
                    self.tab = Tab::Viewer;
                }
                let tabs_w = ui.cursor().min.x - start_x - if show_title { title_w } else { 0.0 };
                ui.spacing_mut().item_spacing.x = 8.0;
                let clear_hint = "Start over: removes the sources, name, and offsets. Settings, history, and the Viewer's comparisons stay.";
                let buttons = ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let w = ui.add_enabled_ui(!running, |ui| {
                        if menu {
                            ui.menu_button("Project", |ui| {
                                ui.set_min_width(150.0);
                                if ui.button("Open…").clicked() {
                                    ui.close_menu();
                                    self.open_project();
                                }
                                if ui.button("Save").on_hover_text("Save the setup as a TOML project").clicked() {
                                    ui.close_menu();
                                    self.save_project(false);
                                }
                                if ui.button("Save as…").clicked() {
                                    ui.close_menu();
                                    self.save_project(true);
                                }
                                ui.separator();
                                if ui.button("Clear").on_hover_text(clear_hint).clicked() {
                                    ui.close_menu();
                                    self.clear_comparison();
                                }
                            });
                            return;
                        }
                        if ui.button("Save as…").clicked() {
                            self.save_project(true);
                        }
                        if ui.button("Save").on_hover_text("Save the setup as a TOML project").clicked() {
                            self.save_project(false);
                        }
                        if ui.button("Open…").clicked() {
                            self.open_project();
                        }
                        if ui.button("Clear").on_hover_text(clear_hint).clicked() {
                            self.clear_comparison();
                        }
                    })
                    .response
                    .rect
                    .width();
                    // Keyboard shortcuts for every screen, in one place (usable while a job runs).
                    let w0 = ui.min_rect().width();
                    theme::shortcuts_button(ui, &shortcut_groups(self.tab));
                    w + (ui.min_rect().width() - w0) + ui.spacing().item_spacing.x
                });
                // Only the full row of buttons tells how much room they need.
                let buttons_w = if menu { prev_buttons } else { buttons.inner };
                ui.ctx().data_mut(|d| d.insert_temp(fit_id, (tabs_w, buttons_w)));
            });
        });

        let page = egui::Frame::new().fill(theme::BG).inner_margin(egui::Margin::symmetric(18, 14));
        egui::CentralPanel::default().frame(page).show(ctx, |ui| match self.tab {
            Tab::Select => self.ui_sources(ui),
            Tab::Sync => self.ui_sync(ui, ctx),
            Tab::Create => self.ui_create(ui, ctx),
            Tab::Viewer => {
                self.viewer.ui(ui, ctx);
            }
        });

        // Toast: a floating card in the bottom-right corner, fading out after a few seconds.
        if let Some((msg, err, at)) = &self.toast {
            let age = at.elapsed().as_secs_f32();
            if age > 5.0 {
                self.toast = None;
            } else {
                let (msg, err) = (msg.clone(), *err);
                let alpha = ((5.0 - age) / 0.4).min(1.0);
                egui::Area::new(egui::Id::new("toast"))
                    .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-18.0, -18.0))
                    .order(egui::Order::Foreground)
                    .interactable(false)
                    .show(ctx, |ui| {
                        ui.set_opacity(alpha);
                        theme::tile().fill(theme::SURFACE_2).show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let (dot, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                                ui.painter().circle_filled(dot.center(), 4.0, if err { theme::ERR } else { theme::OK });
                                ui.label(RichText::new(msg).color(theme::TEXT_BRIGHT));
                            });
                        });
                    });
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
            }
        }

        theme::pointer_cursor_for_clickables(ctx);
    }
}

// ===================================================================================== Select tab

impl App {
    fn ui_sources(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.label(
                RichText::new("Drop video files or episode folders here, or add them below. The first source is the reference: the others are aligned to it.")
                    .weak(),
            );
            ui.add_space(10.0);
            theme::section(ui, "Sources");
            self.ui_source_rows(ui);
            ui.horizontal(|ui| {
                if ui.button("Add file…").clicked() {
                    if let Some(f) = rfd::FileDialog::new()
                        .add_filter("Video", &["mkv", "mp4", "m2ts", "ts", "mpg", "vob", "m2v", "avi", "webm"])
                        .pick_files()
                    {
                        self.add_paths(f);
                    }
                }
                if ui.button("Add folder…").clicked() {
                    if let Some(d) = rfd::FileDialog::new().pick_folders() {
                        self.add_paths(d);
                    }
                }
            });

            if let Some(p) = self.pairing() {
                ui.add_space(14.0);
                match p {
                    Ok(p) if p.rows.len() > 1 => {
                        let (complete, total) = (p.complete().count(), p.rows.len());
                        let rows = p.rows.clone();
                        theme::section(ui, "Episodes");
                        theme::card().show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        let n_ticked = rows.iter().filter(|r| r.files.iter().all(|f| f.is_some()) && !self.setup.skip_episodes.contains(&r.episode.to_string())).count();
                        let title = if complete == total {
                            format!("{total} episodes found in every source")
                        } else {
                            format!("{complete} of {total} episodes found in every source")
                        };
                        theme::disclosure(
                            ui,
                            egui::Id::new("pairing"),
                            false,
                            |ui| {
                                ui.label(RichText::new(title).color(theme::TEXT_BRIGHT));
                                theme::pill(ui, &format!("{n_ticked} ticked"), if n_ticked == 0 { theme::WARN } else { theme::ACCENT });
                            },
                            |ui| {
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("Only ticked episodes are compared; the others aren't opened.").color(theme::TEXT_WEAK));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.small_button("Untick all").clicked() {
                                        self.setup.skip_episodes = rows.iter().map(|r| r.episode.to_string()).collect();
                                    }
                                    if ui.small_button("Tick all").clicked() {
                                        self.setup.skip_episodes.clear();
                                    }
                                });
                            });
                            ui.add_space(4.0);
                            // The table sits in an inset with column headings and a line under each row.
                            theme::code_panel().inner_margin(egui::Margin::symmetric(12, 6)).show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                // Fixed columns for the checkbox and episode; the source columns share what is
                                // left, and long file names are cut with "…" (full path on hover). Laid out by
                                // hand rather than with a Grid, which sizes columns to their longest text.
                                let full_w = ui.available_width();
                                let (check_w, ep_w, gap, row_h) = (16.0, 72.0, 16.0, 30.0);
                                let n_src = self.setup.sources.len().max(1) as f32;
                                let col_w = ((full_w - check_w - ep_w - gap * (n_src + 1.0)) / n_src).max(60.0);
                                let cell = |ui: &mut egui::Ui, w: f32, add: &mut dyn FnMut(&mut egui::Ui)| {
                                    ui.allocate_ui_with_layout(egui::vec2(w, row_h), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                        ui.set_min_size(egui::vec2(w, row_h));
                                        add(ui);
                                    });
                                };
                                let line = |ui: &mut egui::Ui, strong: bool| {
                                    let y = ui.cursor().top();
                                    let x = ui.min_rect().left();
                                    let col = if strong { theme::BORDER } else { theme::BORDER.gamma_multiply(0.5) };
                                    ui.painter().line_segment([egui::pos2(x, y), egui::pos2(x + full_w, y)], egui::Stroke::new(1.0, col));
                                };
                                ui.spacing_mut().item_spacing = egui::vec2(gap, 0.0);
                                ui.horizontal(|ui| {
                                    cell(ui, check_w, &mut |_| {});
                                    let head = |t: &str| RichText::new(t.to_uppercase()).size(11.0).color(theme::TEXT_WEAK).extra_letter_spacing(1.0);
                                    cell(ui, ep_w, &mut |ui| {
                                        ui.add(egui::Label::new(head("Episode")).truncate());
                                    });
                                    for s in &self.setup.sources {
                                        cell(ui, col_w, &mut |ui| {
                                            ui.add(egui::Label::new(head(&s.label)).truncate());
                                        });
                                    }
                                });
                                for r in &rows {
                                    line(ui, std::ptr::eq(r, &rows[0]));
                                    let ep = r.episode.to_string();
                                    let complete_row = r.files.iter().all(|f| f.is_some());
                                    let mut on = complete_row && !self.setup.skip_episodes.contains(&ep);
                                    // Unticked rows are dimmed so the selection reads at a glance.
                                    let (ep_col, file_col) = if on { (theme::TEXT_BRIGHT, theme::TEXT) } else { (theme::TEXT_WEAK, theme::TEXT_WEAK) };
                                    let mut toggled = false;
                                    ui.horizontal(|ui| {
                                        cell(ui, check_w, &mut |ui| {
                                            if complete_row && theme::checkbox(ui, &mut on, "").changed() {
                                                toggled = true;
                                            }
                                        });
                                        cell(ui, ep_w, &mut |ui| {
                                            ui.label(RichText::new(&ep).color(ep_col).strong());
                                        });
                                        for f in &r.files {
                                            cell(ui, col_w, &mut |ui| match f {
                                                Some(f) => {
                                                    let name = f.file_name().unwrap_or_default().to_string_lossy().into_owned();
                                                    ui.add(egui::Label::new(RichText::new(&name).size(13.0).color(file_col)).truncate())
                                                        .on_hover_text(f.display().to_string());
                                                }
                                                None => {
                                                    theme::pill(ui, "missing", theme::ERR);
                                                }
                                            });
                                        }
                                    });
                                    if toggled {
                                        if on {
                                            self.setup.skip_episodes.retain(|e| e != &ep);
                                        } else {
                                            self.setup.skip_episodes.push(ep.clone());
                                        }
                                    }
                                }
                            });
                            },
                        );
                        });
                    }
                    Ok(_) => {}
                    Err(e) => {
                        ui.colored_label(theme::ERR, e.clone());
                    }
                }
            }

            ui.add_space(18.0);
            theme::section(ui, "Settings");
            theme::card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                self.ui_settings(ui);
            });

            ui.add_space(16.0);
            let problems = self.setup.problems();
            if ui.add_enabled(problems.is_empty(), theme::primary("Next: Sync  →")).clicked() {
                self.tab = Tab::Sync;
            }
            for p in problems {
                ui.label(RichText::new(p).weak());
            }
        });
    }

    fn ui_source_rows(&mut self, ui: &mut egui::Ui) {
        let n = self.setup.sources.len();
        let mut action: Option<(usize, i32)> = None; // (index, -1 up / +1 down / 0 remove)
        for i in 0..n {
            let ref_row = i == 0;
            theme::tile().show(ui, |ui| {
                ui.set_width(ui.available_width());
                let row = &mut self.setup.sources[i];
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut row.label).desired_width(140.0).hint_text("Label"));
                    if ref_row {
                        theme::pill(ui, "reference", theme::ACCENT).on_hover_text("The first source is the timeline the others are aligned to");
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        if theme::icon_button(ui, theme::Icon::Remove, true).on_hover_text("Remove").clicked() {
                            action = Some((i, 0));
                        }
                        if theme::icon_button(ui, theme::Icon::Down, i + 1 < n).on_hover_text("Move down").clicked() {
                            action = Some((i, 1));
                        }
                        if theme::icon_button(ui, theme::Icon::Up, i > 0).on_hover_text("Move up (the top source is the reference)").clicked() {
                            action = Some((i, -1));
                        }
                    });
                });
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut row.path).desired_width(ui.available_width() - 150.0).hint_text("File or folder"));
                    if ui.button("File…").clicked() {
                        if let Some(f) = rfd::FileDialog::new().pick_file() {
                            row.path = f.to_string_lossy().into_owned();
                        }
                    }
                    if ui.button("Folder…").clicked() {
                        if let Some(d) = rfd::FileDialog::new().pick_folder() {
                            row.path = d.to_string_lossy().into_owned();
                        }
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label("Fields");
                    egui::ComboBox::from_id_salt(("field", i))
                        .selected_text(match row.field {
                            Field::Progressive => "Progressive",
                            Field::Ivtc => "IVTC (telecined)",
                            Field::Deinterlace => "Deinterlace",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut row.field, Field::Progressive, "Progressive");
                            ui.selectable_value(&mut row.field, Field::Ivtc, "IVTC (telecined)")
                                .on_hover_text("29.97i telecined film (1080i BD remux, NTSC DVD) to 23.976p");
                            ui.selectable_value(&mut row.field, Field::Deinterlace, "Deinterlace");
                        });
                    if row.field != Field::Progressive {
                        egui::ComboBox::from_id_salt(("order", i))
                            .selected_text(match row.order {
                                Order::Auto => "Field order: auto",
                                Order::Tff => "Top field first",
                                Order::Bff => "Bottom field first",
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut row.order, Order::Auto, "Field order: auto");
                                ui.selectable_value(&mut row.order, Order::Tff, "Top field first");
                                ui.selectable_value(&mut row.order, Order::Bff, "Bottom field first");
                            });
                    }
                    ui.add_space(10.0);
                    ui.label("Matrix");
                    egui::ComboBox::from_id_salt(("matrix", i))
                        .selected_text(MATRICES.iter().find(|(m, _)| *m == row.matrix).map(|(_, n)| *n).unwrap_or("?"))
                        .show_ui(ui, |ui| {
                            for (m, name) in MATRICES {
                                ui.selectable_value(&mut row.matrix, m, name);
                            }
                        });
                });
            });
            ui.add_space(4.0);
        }
        if let Some((i, a)) = action {
            match a {
                0 => {
                    self.setup.sources.remove(i);
                }
                -1 => self.setup.sources.swap(i, i - 1),
                _ => self.setup.sources.swap(i, i + 1),
            }
            self.setup.frame_type = None;
        }
    }

    fn ui_settings(&mut self, ui: &mut egui::Ui) {
        let s = &mut self.setup;
        egui::Grid::new("settings").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
            ui.label("Name");
            let hint = s.display_name();
            ui.add(egui::TextEdit::singleline(&mut s.name).hint_text(hint).desired_width(360.0));
            ui.end_row();

            ui.label("Output folder");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut s.out_dir).desired_width(300.0));
                if ui.button("…").clicked() {
                    if let Some(d) = rfd::FileDialog::new().pick_folder() {
                        s.out_dir = d.to_string_lossy().into_owned();
                    }
                }
            });
            ui.end_row();

            ui.label("Frames");
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut s.frames).range(1..=500));
                ui.label("per episode");
                ui.add_space(12.0);
                ui.label("Seed").on_hover_text("The same seed picks the same frames again");
                ui.add(egui::DragValue::new(&mut s.seed));
                if ui.button("New seed").on_hover_text("Pick different frames").clicked() {
                    s.seed = random_seed();
                }
            });
            ui.end_row();

            ui.label("Skip");
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    theme::checkbox(ui, &mut s.skip_black, "Black screens");
                    if s.skip_black {
                        ui
                            .add(egui::Slider::new(&mut s.black_threshold, 0.01..=0.25).text("threshold"))
                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                            .on_hover_text("Frames darker than this are skipped. Raise it to skip dark scenes too.");
                    }
                });
                theme::checkbox(ui, &mut s.skip_scene_changes, "Scene changes (cuts and fades)");
                theme::checkbox(ui, &mut s.skip_duplicates, "Near-duplicate shots");
                theme::checkbox(ui, &mut s.skip_flat, "Flat frames (solid colours, title cards)");
                theme::checkbox(ui, &mut s.exclude_op_ed, "OP/ED (from chapter names)");
            });
            ui.end_row();

            ui.label("Frame type");
            ui.horizontal(|ui| {
                let mut on = s.frame_type.is_some();
                theme::checkbox(ui, &mut on, "Only frames that are");
                if on && s.frame_type.is_none() {
                    s.frame_type = Some((s.sources.len().saturating_sub(1), "B".into()));
                }
                if !on {
                    s.frame_type = None;
                }
                if let Some((idx, types)) = s.frame_type.as_mut() {
                    egui::ComboBox::from_id_salt("ftype")
                        .selected_text(types.clone())
                        .show_ui(ui, |ui| {
                            for t in ["B", "P", "I", "BP"] {
                                ui.selectable_value(types, t.to_string(), t);
                            }
                        });
                    ui.label("frames in");
                    egui::ComboBox::from_id_salt("ftype_src")
                        .selected_text(s.sources.get(*idx).map(|r| r.label.clone()).unwrap_or_default())
                        .show_ui(ui, |ui| {
                            for (i, r) in s.sources.iter().enumerate() {
                                ui.selectable_value(idx, i, &r.label);
                            }
                        });
                }
            });
            ui.end_row();

            ui.label("Anamorphic");
            theme::checkbox(ui, &mut s.stretch_anamorphic, "Stretch to display aspect (DVD 720×480 becomes 853×480)");
            ui.end_row();

            ui.label("Borders");
            theme::checkbox(ui, &mut s.crop_borders, "Crop black borders (pillarbox and letterbox), detected per source");
            ui.end_row();
        });
    }
}

// ======================================================================================= Sync tab

impl App {
    fn pump_preview(&mut self, ctx: &egui::Context) {
        while let Ok(r) = self.worker.rx.try_recv() {
            match r {
                Reply::Status(s) => self.sync.status = s,
                Reply::Ready { ref_frames, ref_times } => {
                    self.sync.status.clear();
                    self.sync.error = None;
                    self.sync.frame = self.sync.frame.clamp(0, ref_frames - 1).max(ref_frames / 10).min(ref_frames - 1);
                    self.sync.ready = Some((ref_frames, ref_times));
                    self.sync.last_req = None;
                }
                Reply::Images { ref_frame, images } => {
                    // Ignore replies for frames the user has already moved away from.
                    if self.sync.last_req.as_ref().is_some_and(|(f, ..)| *f != ref_frame) {
                        continue;
                    }
                    self.sync.images = images
                        .into_iter()
                        .enumerate()
                        .map(|(i, r)| {
                            r.map(|img: PreviewImage| {
                                let ci = egui::ColorImage::from_rgb([img.width, img.height], &img.rgb);
                                let tex = ctx.load_texture(format!("preview{i}"), ci, TextureOptions::LINEAR);
                                (tex, img.frame, img.pict_type, img.native)
                            })
                        })
                        .collect();
                }
                Reply::Suggestion { index, result } => {
                    self.sync.last_suggestion.insert(index, result.clone());
                    self.sync.suggestions.insert(index, Some(result));
                }
                Reply::Error(e) => {
                    self.sync.status.clear();
                    self.sync.error = Some(e);
                    self.sync.ready = None;
                }
            }
        }
    }

    /// Files to preview: for folder sources, the selected complete episode.
    fn sync_files(&mut self) -> Result<(Vec<PathBuf>, Vec<String>), String> {
        let ep = self.sync.episode;
        match self.pairing() {
            None => Err("Add at least two sources in step 1, Select.".into()),
            Some(Err(e)) => Err(e.clone()),
            Some(Ok(p)) => {
                let rows: Vec<_> = p.complete().cloned().collect();
                if rows.is_empty() {
                    return Err("No episode was found in every source. Check the folders in step 1, Select.".into());
                }
                let names = rows.iter().map(|r| r.episode.to_string()).collect();
                let row = &rows[ep.min(rows.len() - 1)];
                Ok((row.files.iter().map(|f| f.clone().unwrap()).collect(), names))
            }
        }
    }

    fn sync_episode_picker(&mut self, ui: &mut egui::Ui, episodes: &[String]) {
        if episodes.len() > 1 {
            egui::ComboBox::from_id_salt("sync_ep")
                .selected_text(episodes.get(self.sync.episode).cloned().unwrap_or_default())
                .show_ui(ui, |ui| {
                    for (i, e) in episodes.iter().enumerate() {
                        ui.selectable_value(&mut self.sync.episode, i, e);
                    }
                });
            theme::vsep(ui);
        }
    }

    /// Side by side / Flicker, the same switch as the viewer's.
    fn sync_view_switch(&mut self, ui: &mut egui::Ui) {
        let modes: [(&str, &str, &[&str]); 2] =
            [("Side by side", "All sources at once", &[]), ("Flicker", "One source at a time", &[])];
        if let Some(i) = theme::segmented(ui, "sync_view_mode", &modes, self.sync.flicker.is_some() as usize) {
            self.sync.flicker = (i == 1).then_some(0);
        }
    }

    fn sync_next_button(&mut self, ui: &mut egui::Ui) {
        if ui.add(theme::primary("Next: Create  →")).on_hover_text("Review the setup and create the comparison").clicked() {
            self.tab = Tab::Create;
        }
    }

    fn sync_step_back(&mut self, ui: &mut egui::Ui, ready: &Option<(i32, Vec<f64>)>) {
        if ready.is_some() {
            for (d, txt) in [(-24, "-1s"), (-1, "-1")] {
                if ui.button(txt).clicked() {
                    self.sync.frame = (self.sync.frame + d).max(0);
                }
            }
        }
    }

    /// Right-to-left: Random, time, frame number, +1s, +1, then the slider filling what is left.
    /// Returns the slider's width.
    fn sync_frame_tail(&mut self, ui: &mut egui::Ui, ready: &Option<(i32, Vec<f64>)>) -> f32 {
        let Some((n, times)) = ready else { return 0.0 };
        let n = *n;
        if ui.button("Random").clicked() {
            let seed = random_seed();
            self.sync.frame = (seed % n as u64) as i32;
        }
        let t = times.get(self.sync.frame as usize).copied().unwrap_or(0.0);
        ui.label(RichText::new(format!("{}:{:06.3}", (t / 60.0) as i64, t % 60.0)).monospace().color(theme::TEXT_WEAK));
        ui.add(egui::DragValue::new(&mut self.sync.frame).range(0..=n - 1).prefix("frame "));
        theme::vsep(ui);
        for (d, txt) in [(24, "+1s"), (1, "+1")] {
            if ui.button(txt).clicked() {
                self.sync.frame = (self.sync.frame + d).min(n - 1);
            }
        }
        let w = (ui.available_width() - 8.0).max(80.0);
        ui.spacing_mut().slider_width = w;
        ui.add(egui::Slider::new(&mut self.sync.frame, 0..=n - 1).show_value(false)).on_hover_cursor(egui::CursorIcon::PointingHand);
        w
    }

    fn ui_sync(&mut self, ui: &mut egui::Ui, _ctx: &egui::Context) {
        let (files, episodes) = match self.sync_files() {
            Ok(v) => v,
            Err(e) => {
                ui.label(e);
                return;
            }
        };
        let cfgs: Vec<SourceConfig> = files
            .iter()
            .zip(&self.setup.sources)
            .map(|(p, r)| SourceConfig { path: p.clone(), field: r.field_options(), matrix: r.matrix })
            .collect();
        if self.sync.sent.as_ref() != Some(&cfgs) {
            self.worker.send(Request::Configure(cfgs.clone()));
            self.sync.sent = Some(cfgs);
            self.sync.ready = None;
            self.sync.images.clear();
            self.sync.suggestions.clear();
            self.sync.last_suggestion.clear();
        }

        // Controls: one toolbar. Episode and frame stepping on the left (the slider takes the spare
        // width), view switch and the way on to step 3 on the right. When that doesn't fit, two
        // rows: episode, view switch and Next above; frame stepping below.
        let ready = self.sync.ready.take();
        theme::toolbar().show(ui, |ui| {
            ui.set_width(ui.available_width());
            let fit_id = egui::Id::new("sync_toolbar_need");
            let need: f32 = ui.ctx().data(|d| d.get_temp(fit_id)).unwrap_or(980.0);
            let one_row = ui.available_width() >= need;
            ui.horizontal(|ui| {
                ui.set_min_height(32.0); // the primary button's height, so both ends centre on the same line
                self.sync_episode_picker(ui, &episodes);
                if !one_row {
                    self.sync_view_switch(ui);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| self.sync_next_button(ui));
                    return;
                }
                let row_left = ui.min_rect().left();
                self.sync_step_back(ui, &ready);
                let left_w = ui.cursor().min.x - row_left;
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    self.sync_next_button(ui);
                    theme::vsep(ui);
                    self.sync_view_switch(ui);
                    if ready.is_some() {
                        theme::vsep(ui);
                    }
                    let slider_w = self.sync_frame_tail(ui, &ready);
                    // Room this row needs: the controls on both sides plus a usable slider. Only
                    // measured once the frame controls exist, or the row would look smaller than it is.
                    if ready.is_some() {
                        let right_w = ui.min_rect().width() - slider_w;
                        ui.ctx().data_mut(|d| d.insert_temp(fit_id, left_w + right_w + 160.0));
                    }
                });
            });
            if !one_row && ready.is_some() {
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    self.sync_step_back(ui, &ready);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        self.sync_frame_tail(ui, &ready);
                    });
                });
            }
        });
        self.sync.ready = ready;
        ui.add_space(4.0);
        if !self.sync.status.is_empty() {
            self.sync.status_shown = self.sync.status.clone();
        }
        let n_src = self.setup.sources.len();
        let flick_on = self.sync.flicker.is_some();
        theme::fade(ui, "sync_flicker_sources", flick_on, |ui| {
            ui.horizontal(|ui| {
                let names: Vec<String> = self.setup.sources.iter().enumerate().map(|(i, r)| format!("{} {}", i + 1, r.label)).collect();
                let keys: Vec<String> = (1..=names.len()).map(|k| k.to_string()).collect();
                let key_refs: Vec<[&str; 1]> = keys.iter().map(|k| [k.as_str()]).collect();
                let opts: Vec<(&str, &str, &[&str])> =
                    names.iter().zip(&key_refs).map(|(n, k)| (n.as_str(), "Show this source", &k[..])).collect();
                let cur = self.sync.flicker.unwrap_or(0).min(n_src.saturating_sub(1));
                if let Some(i) = theme::segmented(ui, "sync_flicker_source", &opts, cur) {
                    self.sync.flicker = Some(i);
                }
                ui.label(RichText::new("Space for the next source").small().color(theme::TEXT_WEAK));
            });
            ui.add_space(4.0);
        });
        let status = &self.sync.status_shown;
        theme::fade(ui, "sync_status", !self.sync.status.is_empty(), |ui| {
            ui.horizontal(|ui| {
                theme::busy(ui, "sync_status", true, theme::BG);
                ui.label(status);
            });
        });
        if let Some(e) = &self.sync.error {
            ui.colored_label(theme::ERR, e);
        }

        // Offsets for non-reference sources, with automatic suggestions. With episode folders, edits
        // here apply to the episode on screen only (stored as an override of the default).
        let ready = self.sync.ready.is_some();
        let cur_ep: Option<String> = (episodes.len() > 1).then(|| episodes.get(self.sync.episode).cloned()).flatten();
        let mut ask: Vec<usize> = Vec::new();
        ui.horizontal_wrapped(|ui| {
            for (i, r) in self.setup.sources.iter_mut().enumerate().skip(1) {
                theme::tile().show(ui, |ui| {
                    ui.vertical(|ui| {
                        let (mut os, mut of) = r.offset_for(cur_ep.as_deref());
                        let before = (os, of);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&r.label).strong());
                            if ui.button("-1").clicked() {
                                of -= 1;
                            }
                            ui.add(egui::DragValue::new(&mut of).speed(0.2).suffix(" f"));
                            if ui.button("+1").clicked() {
                                of += 1;
                            }
                            ui.add(egui::DragValue::new(&mut os).speed(0.01).suffix(" s").max_decimals(3));
                            let busy = matches!(self.sync.suggestions.get(&i), Some(None));
                            if ui
                                .add_enabled(ready && !busy, egui::Button::new("Auto"))
                                .on_hover_text("Find the offset by matching scene cuts, then re-check it at a few later points to catch drift (decodes about two minutes of video)")
                                .clicked()
                            {
                                ask.push(i);
                            }
                        });
                        let state = self.sync.suggestions.get(&i);
                        theme::fade(ui, ("sugg_busy", i), matches!(state, Some(None)), |ui| {
                            ui.horizontal(|ui| {
                                theme::busy(ui, ("sugg", i), true, theme::SURFACE);
                                ui.label(RichText::new("matching scene cuts…").small());
                            });
                        });
                        let last = self.sync.last_suggestion.get(&i);
                        theme::fade(ui, ("sugg_result", i), matches!(state, Some(Some(_))), |ui| match last {
                            Some(Ok(sg)) => {
                                let precision =
                                    if sg.precision_frames == 0 { "frame-exact".to_string() } else { format!("±{} f", sg.precision_frames) };
                                let (verdict, color) = if sg.confidence >= 0.5 {
                                    ("confident", theme::OK)
                                } else if sg.confidence >= 0.3 {
                                    ("uncertain", theme::WARN)
                                } else {
                                    ("unreliable", theme::ERR)
                                };
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(format!("Suggested {:+.3} s", sg.offset_seconds)).color(theme::TEXT_BRIGHT));
                                    ui.label(RichText::new(precision).small().color(theme::TEXT_WEAK));
                                    theme::pill(ui, &format!("{verdict} {:.2}", sg.confidence), color).on_hover_text(format!(
                                        "{}: {} of {} cuts matched; correlation {:.2} vs {:.2} at wrong offsets",
                                        sg.method, sg.matched_cuts, sg.ref_cuts, sg.correlation, sg.baseline
                                    ));
                                    if sg.drift_frames >= 2 {
                                        let at = sg
                                            .drift_at
                                            .map(|t| format!(" Largest around {}:{:02} (reference time).", (t / 60.0) as u32, (t % 60.0) as u32))
                                            .unwrap_or_default();
                                        theme::pill(ui, &format!("drifts {} f", sg.drift_frames), theme::WARN).on_hover_text(format!(
                                            "Later in the episode the best offset is up to {} frames different: this source's timing \
                                             shifts within the episode, so one offset can't fit everywhere.{at} Check those parts \
                                             in the previews.",
                                            sg.drift_frames
                                        ));
                                    }
                                    if ui.button("Apply").clicked() {
                                        os = sg.offset_seconds;
                                        of = 0;
                                    }
                                });
                            }
                            Some(Err(e)) => {
                                ui.colored_label(theme::ERR, RichText::new(e).small());
                            }
                            None => {}
                        });
                        // Store edits: per episode when browsing episodes, else as the default.
                        if (os, of) != before {
                            match &cur_ep {
                                Some(ep) => {
                                    r.episode_offsets.insert(ep.clone(), (os, of));
                                }
                                None => {
                                    r.offset_seconds = os;
                                    r.offset_frames = of;
                                }
                            }
                        }
                        if let Some(ep) = &cur_ep {
                            ui.horizontal(|ui| {
                                if r.episode_offsets.contains_key(ep) {
                                    ui.label(RichText::new(format!("Own offset for {ep}")).small().color(theme::WARN))
                                        .on_hover_text("This episode has its own offset; other episodes use the default offset");
                                    if ui.small_button("Use for all episodes").on_hover_text("Make this the default and clear every episode's own offset").clicked() {
                                        r.offset_seconds = os;
                                        r.offset_frames = of;
                                        r.episode_offsets.clear();
                                    }
                                    if ui.small_button("Use default").on_hover_text("Go back to the default offset for this episode").clicked() {
                                        r.episode_offsets.remove(ep);
                                    }
                                } else {
                                    ui.label(RichText::new("Default offset").small().weak())
                                        .on_hover_text("Changing it here sets an offset for this episode only");
                                }
                            });
                        }
                    });
                });
            }
        });
        if self.auto_sync && ready {
            self.auto_sync = false;
            ask.extend(1..self.setup.sources.len());
        }
        for i in ask {
            self.sync.suggestions.insert(i, None);
            self.worker.send(Request::Suggest { index: i });
        }

        // Request frames when anything relevant changed.
        if self.sync.ready.is_some() {
            let cur_ep: Option<String> = (episodes.len() > 1).then(|| episodes.get(self.sync.episode).cloned()).flatten();
            let offsets: Vec<(f64, i32)> = self.setup.sources.iter().map(|r| r.offset_for(cur_ep.as_deref())).collect();
            let req = (self.sync.frame, offsets.clone(), self.setup.stretch_anamorphic, self.setup.crop_borders);
            if self.sync.last_req.as_ref() != Some(&req) {
                self.worker.send(Request::Frame {
                    ref_frame: self.sync.frame,
                    offsets,
                    max_width: 1600,
                    stretch: self.setup.stretch_anamorphic,
                    crop: self.setup.crop_borders,
                });
                self.sync.last_req = Some(req);
            }
        }

        // Keyboard: arrows step frames; in Flicker, digits pick the source and Space goes to the
        // next one (as in the viewer). Not while a text field is being edited.
        if let Some((n, _)) = self.sync.ready.as_ref().filter(|_| !ui.ctx().wants_keyboard_input()) {
            let n = *n;
            let n_src = self.setup.sources.len().max(1);
            ui.input(|i| {
                // Space steps through sources only while flickering; it doesn't switch modes.
                if i.key_pressed(egui::Key::Space) {
                    if let Some(f) = self.sync.flicker {
                        self.sync.flicker = Some((f + 1) % n_src);
                    }
                }
                if i.key_pressed(egui::Key::ArrowRight) {
                    self.sync.frame = (self.sync.frame + 1).min(n - 1);
                }
                if i.key_pressed(egui::Key::ArrowLeft) {
                    self.sync.frame = (self.sync.frame - 1).max(0);
                }
                // By physical key, so layouts where digits need Shift work too.
                for (idx, _) in theme::digit_presses(i) {
                    if idx < 5 && idx < self.setup.sources.len() && self.sync.flicker.is_some() {
                        self.sync.flicker = Some(idx);
                    }
                }
            });
        }

        ui.add_space(6.0);
        // Images.
        let shown: Vec<usize> = match self.sync.flicker {
            Some(i) => vec![i.min(self.sync.images.len().saturating_sub(1))],
            None => (0..self.sync.images.len()).collect(),
        };
        if shown.is_empty() || self.sync.images.is_empty() {
            return;
        }
        let avail = ui.available_size();
        let n = shown.len();
        let aspect = self.sync.images.iter().find_map(|r| r.as_ref().ok()).map(|(t, ..)| t.aspect_ratio()).unwrap_or(16.0 / 9.0);
        let mut best = (1usize, 0.0f32);
        for cols in 1..=n {
            let rows = n.div_ceil(cols);
            let w = (avail.x / cols as f32 - 8.0).min((avail.y / rows as f32 - 28.0) * aspect);
            if w > best.1 {
                best = (cols, w);
            }
        }
        let (cols, w) = best;
        egui::Grid::new("sync_imgs").spacing([8.0, 8.0]).show(ui, |ui| {
            for (k, &i) in shown.iter().enumerate() {
                ui.vertical(|ui| {
                    let label = &self.setup.sources.get(i).map(|s| s.label.clone()).unwrap_or_default();
                    match &self.sync.images[i] {
                        Ok((tex, f, pt, (nw, nh))) => {
                            let star = if self.setup.sources.get(i).is_some_and(|s| s.field != Field::Progressive) { "*" } else { "" };
                            let r = ui.label(RichText::new(format!("{label}  ·  #{f}  ·  {pt}{star}  ·  {nw}×{nh}")).small());
                            if !star.is_empty() {
                                r.on_hover_text(crate::viewer::STAR_EXPLANATION);
                            }
                            let h = w / tex.aspect_ratio();
                            ui.add(egui::Image::new(tex).fit_to_exact_size(egui::vec2(w, h)));
                        }
                        Err(e) => {
                            ui.label(RichText::new(label.as_str()).small());
                            ui.allocate_ui(egui::vec2(w, w / aspect), |ui| {
                                ui.centered_and_justified(|ui| ui.label(RichText::new(e).weak()));
                            });
                        }
                    }
                });
                if (k + 1) % cols == 0 {
                    ui.end_row();
                }
            }
        });
    }
}

// ===================================================================================== Create tab

impl App {
    /// Step 3: what will be made, the button that makes it, the job in progress, and this session's history.
    fn ui_create(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let running = self.running();
        let mut view: Vec<PathBuf> = Vec::new();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            theme::section(ui, "What will be created");
            let card = theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            self.ui_summary(ui);

            ui.add_space(12.0);
            let problems = self.setup.problems();
            let sel = self.selected_episodes();
            let none = sel.as_ref().is_some_and(|v| v.is_empty());
            let text = match &sel {
                Some(v) if v.len() == 1 => format!("Create comparison of {}", v[0]),
                Some(v) => format!("Create comparisons of {} episodes", v.len()),
                None if self.complete_episodes().len() > 1 => format!("Create comparisons of {} episodes", self.complete_episodes().len()),
                None => "Create comparison".to_string(),
            };
            let hover = if running {
                "A comparison is already being created"
            } else if none {
                "No episodes are ticked in step 1, Select"
            } else {
                "Existing comparisons with the same name are replaced"
            };
            if ui
                .add_enabled(problems.is_empty() && !none && !running, theme::primary(text))
                .on_hover_text(hover)
                .on_disabled_hover_text(hover)
                .clicked()
            {
                self.start_run(ctx, sel);
            }
            for p in problems {
                ui.label(RichText::new(p).color(theme::WARN));
            }
            });
            let _ = card;

            let n_jobs = self.jobs.len();
            if let Some(job) = self.jobs.last_mut() {
                // Keyed by job number, so each new job fades in fresh; a finished one fades out
                // showing its final state.
                theme::fade(ui, ("in_progress", n_jobs), job.finished.is_none(), |ui| {
                    ui.add_space(18.0);
                    theme::section(ui, "In progress");
                    theme::card().stroke(egui::Stroke::new(1.0, theme::ACCENT.gamma_multiply(0.5))).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui_progress(ui, ctx, job);
                    });
                });
            }

            let done = self.jobs.iter().filter(|j| j.finished.is_some()).count();
            if done > 0 {
                ui.add_space(18.0);
                theme::section(ui, "History");
                ui.label(RichText::new("Created since SlopComp opened, newest first.").small().color(theme::TEXT_WEAK));
                ui.add_space(4.0);
                let n = self.jobs.len();
                for (k, job) in self.jobs.iter().enumerate().rev().filter(|(_, j)| j.finished.is_some()) {
                    theme::fade(ui, ("history", k), true, |ui| {
                        if let Some(dirs) = ui_history_entry(ui, job, k + 1 == n) {
                            view = dirs;
                        }
                    });
                }
            }
        });
        if let Some(first) = view.first() {
            for d in &view {
                self.viewer.add(d);
            }
            self.viewer.open(first);
            self.tab = Tab::Viewer;
        }
    }

    /// Read-only overview of what the Create button will make.
    fn ui_summary(&mut self, ui: &mut egui::Ui) {
        let all = self.complete_episodes();
        let sel = self.selected_episodes();
        let s = &self.setup;
        let weak = |t: String| RichText::new(t).weak();
        let key = |ui: &mut egui::Ui, t: &str| {
            ui.label(RichText::new(t).color(theme::TEXT_WEAK));
        };
        egui::Grid::new("summary").num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
            key(ui, "Name");
            ui.label(s.display_name());
            ui.end_row();

            key(ui, "Output folder");
            if s.out_dir.trim().is_empty() {
                ui.label(weak("not set".into()));
            } else {
                theme::path_label(ui, &s.out_dir, theme::TEXT);
            }
            ui.end_row();

            if !all.is_empty() {
                key(ui, "Episodes");
                let list = sel.clone().unwrap_or_else(|| all.clone());
                ui.label(format!("{} of {}: {}", list.len(), all.len(), list.join(", ")));
                ui.end_row();
            }

            // Name at the top of the cell, level with the first source rather than centred on the list.
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| key(ui, "Sources"));
            if s.sources.is_empty() {
                ui.label(weak("none".into()));
            }
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                for (i, r) in s.sources.iter().enumerate() {
                    if i > 0 {
                        ui.add_space(6.0);
                    }
                    let mut fields = match r.field {
                        Field::Progressive => "progressive".to_string(),
                        Field::Ivtc => "IVTC".to_string(),
                        Field::Deinterlace => "deinterlace".to_string(),
                    };
                    if r.field != Field::Progressive {
                        fields += match r.order {
                            Order::Auto => "",
                            Order::Tff => " (TFF)",
                            Order::Bff => " (BFF)",
                        };
                    }
                    let matrix = MATRICES.iter().find(|(m, _)| *m == r.matrix).map(|(_, n)| *n).unwrap_or("?");
                    let offset = if i == 0 {
                        "reference".to_string()
                    } else {
                        let mut t = format!("offset {:+.3} s {:+} f", r.offset_seconds, r.offset_frames);
                        if !r.episode_offsets.is_empty() {
                            t += &format!(", own offset for {}", r.episode_offsets.keys().cloned().collect::<Vec<_>>().join(", "));
                        }
                        t
                    };
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(&r.label).strong().color(theme::TEXT_BRIGHT));
                        ui.label(RichText::new(format!("{fields} · matrix {} · {offset}", matrix.to_lowercase())).color(theme::TEXT));
                    });
                    theme::path_label(ui, &r.path, theme::TEXT_WEAK);
                }
            });
            ui.end_row();

            key(ui, "Frames");
            ui.label(format!("{} per episode, seed {}", s.frames, s.seed));
            ui.end_row();

            let mut skip = Vec::new();
            if s.skip_black {
                skip.push(format!("black screens (< {:.2})", s.black_threshold));
            }
            for (on, name) in [
                (s.skip_scene_changes, "scene changes"),
                (s.skip_duplicates, "near-duplicates"),
                (s.skip_flat, "flat frames"),
                (s.exclude_op_ed, "OP/ED"),
            ] {
                if on {
                    skip.push(name.to_string());
                }
            }
            key(ui, "Skip");
            ui.label(if skip.is_empty() { weak("nothing".into()) } else { RichText::new(skip.join(", ")) });
            ui.end_row();

            if let Some((idx, types)) = &s.frame_type {
                key(ui, "Frame type");
                ui.label(format!("only {types} frames in {}", s.sources.get(*idx).map(|r| r.label.as_str()).unwrap_or("?")));
                ui.end_row();
            }

            key(ui, "Picture");
            let mut pic = Vec::new();
            if s.stretch_anamorphic {
                pic.push("stretch anamorphic");
            }
            if s.crop_borders {
                pic.push("crop black borders");
            }
            ui.label(if pic.is_empty() { weak("unchanged".into()) } else { RichText::new(pic.join(", ")) });
            ui.end_row();
        });
    }
}

/// Live progress of the running job.
fn ui_progress(ui: &mut egui::Ui, ctx: &egui::Context, job: &mut Job) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(&job.title).size(15.0).color(theme::TEXT_BRIGHT));
        theme::busy(ui, "job", job.finished.is_none(), theme::SURFACE);
        let (i, total, ep) = &job.episode;
        if *total > 1 {
            ui.label(RichText::new(format!("episode {} of {total} · {ep}", i + 1)).color(theme::TEXT_WEAK));
        }
        let secs = if job.finished.is_some() { job.elapsed as u64 } else { job.started.elapsed().as_secs() };
        ui.label(RichText::new(format!("{}:{:02}", secs / 60, secs % 60)).color(theme::TEXT_WEAK).monospace());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.add_enabled(job.finished.is_none(), egui::Button::new("Cancel")).clicked() {
                job.cancel.store(true, Ordering::Relaxed);
            }
        });
    });
    ctx.request_repaint_after(std::time::Duration::from_millis(250));
    ui.add_space(6.0);
    let full = ui.available_width();
    if job.episode.1 > 1 {
        let done = job.results.len() + job.failed.len();
        theme::progress(ui, "episodes", done as f32 / job.episode.1 as f32, &format!("Episodes  {done}/{}", job.episode.1), full);
        ui.add_space(4.0);
    }
    // Per source: indexing, then field analysis for interlaced sources.
    let col = ((full - 120.0 - 16.0) / 2.0).max(140.0);
    for s in &job.sources {
        ui.horizontal(|ui| {
            // Source name on the same line as the bar labels, so the row reads as one line of text.
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(120.0, 28.0), egui::Sense::hover());
            let font = egui::FontId::proportional(12.5);
            let galley = ui.painter().layout(s.label.clone(), font, theme::TEXT_BRIGHT, rect.width() - 8.0);
            ui.painter_at(rect).galley(rect.left_top(), galley, theme::TEXT_BRIGHT);
            resp.on_hover_text(&s.label);
            theme::progress(ui, ("indexing", &s.label), s.indexing, "Indexing", col);
            ui.add_space(8.0);
            if let Some(a) = s.analysis {
                theme::progress(ui, ("analysis", &s.label), a, "Field analysis", col);
            }
        });
    }
    if job.shots.1 > 0 {
        ui.add_space(4.0);
        theme::progress(ui, "shots", job.shots.0 as f32 / job.shots.1 as f32, &format!("Screenshots  {}/{}", job.shots.0, job.shots.1), full);
    }
    ui.add_space(4.0);
    ui_log(ui, job, "log_running", true);
}

/// One finished job: outcome, its comparisons (open in the viewer), failures and log.
/// Returns the comparison folders to open in the viewer when a button was clicked (the first is shown).
fn ui_history_entry(ui: &mut egui::Ui, job: &Job, newest: bool) -> Option<Vec<PathBuf>> {
    let mut view = None;
    let (status, color) = match &job.finished {
        Some(Ok(())) if job.failed.is_empty() => ("done".to_string(), theme::OK),
        Some(Ok(())) => (format!("{} failed", job.failed.len()), theme::WARN),
        _ => ("stopped".to_string(), theme::ERR),
    };
    let id = ui.make_persistent_id(("job", job.started));
    let secs = job.elapsed as u64;
    theme::tile().show(ui, |ui| {
    ui.set_width(ui.available_width());
    theme::disclosure(ui, id, newest, |ui| {
        ui.label(RichText::new(&job.title).color(theme::TEXT_BRIGHT));
        theme::pill(ui, &status, color);
        ui.label(RichText::new(format!("{}:{:02}", secs / 60, secs % 60)).color(theme::TEXT_WEAK).monospace())
            .on_hover_text("How long it took");
        if let Some(at) = &job.finished_at {
            ui.label(RichText::new(format!("finished {at}")).color(theme::TEXT_WEAK));
        }
    },
    |ui| {
        ui.add_space(4.0);
        if let Some(Err(e)) = &job.finished {
            ui.colored_label(theme::ERR, format!("Stopped: {e}"));
        }
        if job.results.len() > 1 {
            let n = job.results.len();
            if ui
                .add(theme::primary(format!("Open all {n} in Viewer")))
                .on_hover_text("Adds every comparison from this run to the Viewer and shows the first")
                .clicked()
            {
                view = Some(job.results.iter().map(|r| r.out_dir.clone()).collect());
            }
        }
        egui::Grid::new(("results", job.started)).striped(true).show(ui, |ui| {
            for r in &job.results {
                ui.label(&r.name);
                ui.label(RichText::new(format!("{} frames", r.frames.len())).weak());
                if ui.small_button("Open in Viewer").clicked() {
                    view = Some(vec![r.out_dir.clone()]);
                }
                ui.end_row();
            }
        });
        for (ep, e) in &job.failed {
            ui.colored_label(theme::ERR, format!("{ep}: {e}"));
        }
        ui.add_space(4.0);
        ui_log(ui, job, &format!("log{:?}", job.started), false);
    });
    });
    ui.add_space(6.0);
    view
}

fn ui_log(ui: &mut egui::Ui, job: &Job, id: &str, open: bool) {
    let lines = job.log.len();
    theme::disclosure(
        ui,
        egui::Id::new(id),
        open,
        |ui| {
            ui.label(RichText::new("Log").color(theme::TEXT));
            ui.label(RichText::new(format!("{lines} lines")).small().color(theme::TEXT_WEAK));
        },
        |ui| {
            theme::code_panel().show(ui, |ui| {
                ui.set_width(ui.available_width());
                egui::ScrollArea::vertical().id_salt(id).max_height(240.0).stick_to_bottom(true).auto_shrink([false, true]).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 3.0;
                    for l in &job.log {
                        let (time, msg) = split_stamp(l);
                        // Episode headings stand out; failures are red; the rest is quiet. Times are dimmer still.
                        let col = if msg.starts_with("==") {
                            theme::TEXT_BRIGHT
                        } else if msg.contains("failed") || msg.starts_with("stopped") {
                            theme::ERR
                        } else {
                            theme::TEXT_WEAK
                        };
                        let font = egui::FontId::monospace(12.0);
                        ui.horizontal_top(|ui| {
                            ui.spacing_mut().item_spacing.x = 10.0;
                            if let Some(t) = time {
                                ui.label(RichText::new(t).font(font.clone()).color(theme::TEXT_WEAK.gamma_multiply(0.6)));
                            }
                            ui.add(egui::Label::new(RichText::new(msg).font(font).color(col)).wrap());
                        });
                    }
                });
            });
        },
    );
}
