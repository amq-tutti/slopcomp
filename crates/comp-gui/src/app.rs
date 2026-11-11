use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::Instant;

use comp_core::comp::{CompResult, Event};
use comp_core::pairing::Pairing;
use comp_core::project::Project;
use comp_core::season::{self, SeasonEvent, SeasonResult};
use eframe::egui::{self, Color32, RichText, TextureHandle, TextureOptions};

use crate::preview::{self, PreviewImage, Reply, Request, SourceConfig};
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
        let mut setup = cc.storage.and_then(|s| eframe::get_value::<Setup>(s, STORAGE_KEY)).unwrap_or_default();
        // `anime-comp project.toml` opens a project.
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
            },
            jobs: Vec::new(),
            toast: None,
            project_path,
            auto_sync: args.iter().any(|a| a == "--auto-sync"),
            viewer: crate::viewer::Viewer::new(&cc.egui_ctx),
        };

        app.tab = tab;
        // Developer flag --view=DIR: open a comparison in the viewer.
        if let Some(d) = args.iter().find_map(|a| a.strip_prefix("--view=")) {
            app.viewer.open(Path::new(d));
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
                .add_filter("Anime Comparisons project", &["toml"])
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
        self.sync.error = None;
        self.sync.status.clear();
        self.tab = Tab::Select;
        self.notify("Cleared sources, name and offsets; settings kept", false);
    }

    fn open_project(&mut self) {
        if let Some(p) = rfd::FileDialog::new().add_filter("Anime Comparisons project", &["toml"]).pick_file() {
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
        });
        self.tab = Tab::Create;
    }

    fn pump_run(&mut self) {
        let Some(run) = self.jobs.last_mut() else { return };
        while let Ok(msg) = run.rx.try_recv() {
            match msg {
                RunMsg::Event(e) => match e {
                    SeasonEvent::Paired(p) => {
                        run.log.push(format!("{} of {} episodes present in every source", p.complete().count(), p.rows.len()))
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
                            run.log.push(format!("== {episode} =="));
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
                        Event::Opened { source, info } => {
                            let s = &mut run.sources[source];
                            s.opened = true;
                            s.indexing = 1.0;
                            if let Some(a) = s.analysis.as_mut() {
                                *a = 1.0;
                            }
                            run.log.push(format!(
                                "{}: {}×{} {}-bit, {} frames, {}",
                                s.label, info.width, info.height, info.layout.bit_depth, info.num_frames, info.container
                            ));
                        }
                        Event::Excluded { ranges } => {
                            for (a, b, why) in ranges {
                                run.log.push(format!("excluding {:.1}-{:.1}s ({why})", a, b));
                            }
                        }
                        Event::FramesPicked { frames, .. } => {
                            run.shots = (0, frames.len() * run.sources.len());
                            run.log.push(format!("picked {} frames", frames.len()));
                        }
                        Event::Shot { .. } => run.shots.0 += 1,
                        _ => {}
                    },
                    SeasonEvent::EpisodeDone { result, .. } => {
                        run.log.push(format!("done: {}", result.out_dir.display()));
                        self.viewer.add(&result.out_dir);
                        run.results.push(*result);
                    }
                    SeasonEvent::EpisodeFailed { episode, error, .. } => {
                        run.log.push(format!("{episode} failed: {error}"));
                        run.failed.push((episode, error));
                    }
                    SeasonEvent::IndexWritten(_) => {}
                },
                RunMsg::Done(res) => {
                    run.elapsed = run.started.elapsed().as_secs_f64();
                    match res {
                        Ok(_) => {
                            run.finished = Some(Ok(()));
                        }
                        Err(e) => {
                            run.log.push(format!("stopped: {e}"));
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
        ("plex-sans", &include_bytes!("../fonts/IBMPlexSans.ttf")[..]),
        ("plex-mono", &include_bytes!("../fonts/IBMPlexMono-Regular.ttf")[..]),
        ("plex-sans-jp", &include_bytes!("../fonts/IBMPlexSansJP-Regular.ttf")[..]),
    ] {
        fonts.font_data.insert(name.to_owned(), Arc::new(FontData::from_static(bytes)));
    }
    let prop = fonts.families.entry(FontFamily::Proportional).or_default();
    prop.insert(0, "plex-sans".to_owned());
    prop.insert(1, "plex-sans-jp".to_owned());
    let mono = fonts.families.entry(FontFamily::Monospace).or_default();
    mono.insert(0, "plex-mono".to_owned());
    mono.insert(1, "plex-sans-jp".to_owned());
    ctx.set_fonts(fonts);
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

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.heading("Anime Comparisons");
                ui.add_space(12.0);
                let running = self.running();
                // Numbered tabs are the steps to make a comparison; the viewer stands apart (it also
                // opens earlier comparisons) and stays usable while a job runs.
                for (tab, name) in [(Tab::Select, "1  Select"), (Tab::Sync, "2  Sync"), (Tab::Create, "3  Create")] {
                    let enabled = !(running && tab != Tab::Create);
                    let r = ui
                        .add_enabled(enabled, egui::SelectableLabel::new(self.tab == tab, name))
                        .on_disabled_hover_text("Locked while a comparison is being created");
                    if r.clicked() {
                        self.tab = tab;
                    }
                }
                ui.separator();
                if ui.selectable_label(self.tab == Tab::Viewer, "Viewer").clicked() {
                    self.tab = Tab::Viewer;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_enabled_ui(!running, |ui| {
                        if ui.button("Save as…").clicked() {
                            self.save_project(true);
                        }
                        if ui.button("Save").on_hover_text("Save the setup as a TOML project").clicked() {
                            self.save_project(false);
                        }
                        if ui.button("Open…").clicked() {
                            self.open_project();
                        }
                        if ui
                            .button("Clear")
                            .on_hover_text("Clear sources, name and offsets to start over. Settings, history and comparisons open in the Viewer are kept.")
                            .clicked()
                        {
                            self.clear_comparison();
                        }
                    });
                });
            });
            ui.add_space(4.0);
        });

        if let Some((msg, err, at)) = &self.toast {
            if at.elapsed().as_secs_f32() > 5.0 {
                self.toast = None;
            } else {
                let (msg, err) = (msg.clone(), *err);
                egui::TopBottomPanel::bottom("toast").show(ctx, |ui| {
                    ui.colored_label(if err { Color32::from_rgb(220, 80, 80) } else { ui.visuals().text_color() }, msg);
                });
                ctx.request_repaint_after(std::time::Duration::from_millis(500));
            }
        }

        egui::CentralPanel::default().show(ctx, |ui| match self.tab {
            Tab::Select => self.ui_sources(ui),
            Tab::Sync => self.ui_sync(ui, ctx),
            Tab::Create => self.ui_create(ui, ctx),
            Tab::Viewer => {
                self.viewer.ui(ui, ctx);
            }
        });
    }
}

// ===================================================================================== Select tab

impl App {
    fn ui_sources(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.label(
                RichText::new("Drop video files or episode folders onto this window, or add them below. The first source is the reference timeline.")
                    .weak(),
            );
            ui.add_space(8.0);
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
                ui.add_space(6.0);
                match p {
                    Ok(p) if p.rows.len() > 1 => {
                        let (complete, total) = (p.complete().count(), p.rows.len());
                        let rows = p.rows.clone();
                        egui::CollapsingHeader::new(format!("Episodes: {complete} of {total} present in every source"))
                            .id_salt("pairing")
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("Ticked episodes go into the comparison; unticked ones are never opened.").weak());
                                    if ui.small_button("All").clicked() {
                                        self.setup.skip_episodes.clear();
                                    }
                                    if ui.small_button("None").clicked() {
                                        self.setup.skip_episodes = rows.iter().map(|r| r.episode.to_string()).collect();
                                    }
                                });
                                egui::Grid::new("pairs").striped(true).show(ui, |ui| {
                                    ui.strong("Include");
                                    ui.strong("Episode");
                                    for s in &self.setup.sources {
                                        ui.strong(&s.label);
                                    }
                                    ui.end_row();
                                    for r in &rows {
                                        let ep = r.episode.to_string();
                                        if r.files.iter().all(|f| f.is_some()) {
                                            let mut on = !self.setup.skip_episodes.contains(&ep);
                                            if ui.checkbox(&mut on, "").changed() {
                                                if on {
                                                    self.setup.skip_episodes.retain(|e| e != &ep);
                                                } else {
                                                    self.setup.skip_episodes.push(ep.clone());
                                                }
                                            }
                                        } else {
                                            ui.label("");
                                        }
                                        ui.label(&ep);
                                        for f in &r.files {
                                            match f {
                                                Some(f) => ui.label(f.file_name().unwrap_or_default().to_string_lossy()),
                                                None => ui.colored_label(Color32::from_rgb(220, 80, 80), "missing"),
                                            };
                                        }
                                        ui.end_row();
                                    }
                                });
                            });
                    }
                    Ok(_) => {}
                    Err(e) => {
                        ui.colored_label(Color32::from_rgb(220, 80, 80), e.clone());
                    }
                }
            }

            ui.add_space(14.0);
            ui.separator();
            ui.heading("Settings");
            ui.add_space(4.0);
            self.ui_settings(ui);

            ui.add_space(14.0);
            let problems = self.setup.problems();
            if ui.add_enabled(problems.is_empty(), egui::Button::new(RichText::new("Next: 2  Sync").strong())).clicked() {
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
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_width(ui.available_width());
                let row = &mut self.setup.sources[i];
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut row.label).desired_width(140.0).hint_text("Label"));
                    if ref_row {
                        ui.label(RichText::new("reference").small().weak());
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Remove").clicked() {
                            action = Some((i, 0));
                        }
                        if ui.add_enabled(i + 1 < n, egui::Button::new("Down").small()).on_hover_text("Move down").clicked() {
                            action = Some((i, 1));
                        }
                        if ui.add_enabled(i > 0, egui::Button::new("Up").small()).on_hover_text("Move up (top = reference)").clicked() {
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
                                Order::Auto => "Order: auto",
                                Order::Tff => "TFF",
                                Order::Bff => "BFF",
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut row.order, Order::Auto, "Order: auto");
                                ui.selectable_value(&mut row.order, Order::Tff, "TFF");
                                ui.selectable_value(&mut row.order, Order::Bff, "BFF");
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
                ui.label("per episode, seed");
                ui.add(egui::DragValue::new(&mut s.seed));
                if ui.button("New seed").on_hover_text("Pick a new random seed").clicked() {
                    s.seed = random_seed();
                }
            });
            ui.end_row();

            ui.label("Skip");
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.checkbox(&mut s.skip_black, "Black screens");
                    if s.skip_black {
                        ui.add(egui::Slider::new(&mut s.black_threshold, 0.01..=0.25).text("threshold"))
                            .on_hover_text("Mean brightness below which a frame is skipped. Raise it to also skip dark scenes.");
                    }
                });
                ui.checkbox(&mut s.skip_scene_changes, "Scene changes (cuts and fades)");
                ui.checkbox(&mut s.skip_duplicates, "Near-duplicate shots");
                ui.checkbox(&mut s.skip_flat, "Flat frames (solid colours, title cards)");
                ui.checkbox(&mut s.exclude_op_ed, "OP/ED (from chapter names)");
            });
            ui.end_row();

            ui.label("Frame type");
            ui.horizontal(|ui| {
                let mut on = s.frame_type.is_some();
                ui.checkbox(&mut on, "Only frames that are");
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
            ui.checkbox(&mut s.stretch_anamorphic, "Stretch to display aspect (e.g. DVD 720×480 to 853×480)");
            ui.end_row();

            ui.label("Borders");
            ui.checkbox(&mut s.crop_borders, "Crop black borders (pillarbox / letterbox), detected per source");
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
            None => Err("Add at least two sources.".into()),
            Some(Err(e)) => Err(e.clone()),
            Some(Ok(p)) => {
                let rows: Vec<_> = p.complete().cloned().collect();
                if rows.is_empty() {
                    return Err("No episode is present in every source.".into());
                }
                let names = rows.iter().map(|r| r.episode.to_string()).collect();
                let row = &rows[ep.min(rows.len() - 1)];
                Ok((row.files.iter().map(|f| f.clone().unwrap()).collect(), names))
            }
        }
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
        }

        // Controls.
        ui.horizontal_wrapped(|ui| {
            if episodes.len() > 1 {
                egui::ComboBox::from_id_salt("sync_ep")
                    .selected_text(episodes.get(self.sync.episode).cloned().unwrap_or_default())
                    .show_ui(ui, |ui| {
                        for (i, e) in episodes.iter().enumerate() {
                            ui.selectable_value(&mut self.sync.episode, i, e);
                        }
                    });
            }
            if ui.button(RichText::new("Next: 3  Create").strong()).on_hover_text("Review the setup and create the comparison").clicked() {
                self.tab = Tab::Create;
            }
            if let Some((n, times)) = &self.sync.ready {
                let n = *n;
                for (d, txt) in [(-24, "-1s"), (-1, "-1")] {
                    if ui.button(txt).clicked() {
                        self.sync.frame = (self.sync.frame + d).max(0);
                    }
                }
                ui.add(egui::Slider::new(&mut self.sync.frame, 0..=n - 1).show_value(false));
                for (d, txt) in [(1, "+1"), (24, "+1s")] {
                    if ui.button(txt).clicked() {
                        self.sync.frame = (self.sync.frame + d).min(n - 1);
                    }
                }
                ui.add(egui::DragValue::new(&mut self.sync.frame).range(0..=n - 1).prefix("frame "));
                let t = times.get(self.sync.frame as usize).copied().unwrap_or(0.0);
                ui.label(format!("{}:{:06.3}", (t / 60.0) as i64, t % 60.0));
                if ui.button("Random").clicked() {
                    let seed = random_seed();
                    self.sync.frame = (seed % n as u64) as i32;
                }
            }
        });
        if !self.sync.status.is_empty() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(&self.sync.status);
            });
        }
        if let Some(e) = &self.sync.error {
            ui.colored_label(Color32::from_rgb(220, 80, 80), e);
        }

        // Offsets for non-reference sources, with automatic suggestions. With episode folders, edits
        // here apply to the episode on screen only (stored as an override of the default).
        let ready = self.sync.ready.is_some();
        let cur_ep: Option<String> = (episodes.len() > 1).then(|| episodes.get(self.sync.episode).cloned()).flatten();
        let mut ask: Vec<usize> = Vec::new();
        ui.horizontal_wrapped(|ui| {
            for (i, r) in self.setup.sources.iter_mut().enumerate().skip(1) {
                ui.group(|ui| {
                    ui.vertical(|ui| {
                        let (mut os, mut of) = r.offset_for(cur_ep.as_deref());
                        let before = (os, of);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&r.label).strong());
                            if ui.small_button("-1").clicked() {
                                of -= 1;
                            }
                            ui.add(egui::DragValue::new(&mut of).speed(0.2).suffix(" f"));
                            if ui.small_button("+1").clicked() {
                                of += 1;
                            }
                            ui.add(egui::DragValue::new(&mut os).speed(0.01).suffix(" s").max_decimals(3));
                            let busy = matches!(self.sync.suggestions.get(&i), Some(None));
                            if ui
                                .add_enabled(ready && !busy, egui::Button::new("Auto").small())
                                .on_hover_text("Find the offset by matching scene cuts (decodes about a minute of video)")
                                .clicked()
                            {
                                ask.push(i);
                            }
                        });
                        match self.sync.suggestions.get(&i) {
                            Some(None) => {
                                ui.horizontal(|ui| {
                                    ui.spinner();
                                    ui.label(RichText::new("matching scene cuts…").small());
                                });
                            }
                            Some(Some(Ok(sg))) => {
                                let precision =
                                    if sg.precision_frames == 0 { "frame-exact".to_string() } else { format!("±{} f", sg.precision_frames) };
                                let (verdict, color) = if sg.confidence >= 0.5 {
                                    ("confident", Color32::from_rgb(80, 180, 100))
                                } else if sg.confidence >= 0.3 {
                                    ("check it", Color32::from_rgb(220, 170, 60))
                                } else {
                                    ("unreliable", Color32::from_rgb(220, 80, 80))
                                };
                                ui.horizontal(|ui| {
                                    ui.colored_label(
                                        color,
                                        RichText::new(format!("{:+.3} s ({precision}), {verdict} {:.2}", sg.offset_seconds, sg.confidence)).small(),
                                    )
                                    .on_hover_text(format!(
                                        "{}: {} of {} cuts matched; correlation {:.2} vs {:.2} at wrong offsets",
                                        sg.method, sg.matched_cuts, sg.ref_cuts, sg.correlation, sg.baseline
                                    ));
                                    if ui.small_button("Apply").clicked() {
                                        os = sg.offset_seconds;
                                        of = 0;
                                    }
                                });
                            }
                            Some(Some(Err(e))) => {
                                ui.colored_label(Color32::from_rgb(220, 80, 80), RichText::new(e).small());
                            }
                            None => {}
                        }
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
                                    ui.label(RichText::new(format!("{ep} only")).small().color(Color32::from_rgb(220, 170, 60)))
                                        .on_hover_text("This episode has its own offset; other episodes use the default offset");
                                    if ui.small_button("Use for all episodes").on_hover_text("Make this the default and clear every episode's own offset").clicked() {
                                        r.offset_seconds = os;
                                        r.offset_frames = of;
                                        r.episode_offsets.clear();
                                    }
                                    if ui.small_button("Reset").on_hover_text("Go back to the default offset for this episode").clicked() {
                                        r.episode_offsets.remove(ep);
                                    }
                                } else {
                                    ui.label(RichText::new("default offset").small().weak())
                                        .on_hover_text("Changing it here sets an offset for this episode only");
                                }
                            });
                        }
                    });
                });
            }
            ui.separator();
            let mut flick = self.sync.flicker.is_some();
            if ui.checkbox(&mut flick, "Flicker").on_hover_text("Show one source at a time; keys 1-5 switch").changed() {
                self.sync.flicker = flick.then_some(0);
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

        // Keyboard: arrows step frames, digits pick the flicker source.
        if let Some((n, _)) = &self.sync.ready {
            let n = *n;
            ui.input(|i| {
                if i.key_pressed(egui::Key::ArrowRight) {
                    self.sync.frame = (self.sync.frame + 1).min(n - 1);
                }
                if i.key_pressed(egui::Key::ArrowLeft) {
                    self.sync.frame = (self.sync.frame - 1).max(0);
                }
                for (k, idx) in [(egui::Key::Num1, 0), (egui::Key::Num2, 1), (egui::Key::Num3, 2), (egui::Key::Num4, 3), (egui::Key::Num5, 4)] {
                    if i.key_pressed(k) && idx < self.setup.sources.len() && self.sync.flicker.is_some() {
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
        let mut view: Option<PathBuf> = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.heading("Comparison");
            ui.add_space(4.0);
            self.ui_summary(ui);

            ui.add_space(10.0);
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
                "No episodes ticked in 1  Select"
            } else {
                "Existing comparisons with the same name are replaced"
            };
            if ui
                .add_enabled(problems.is_empty() && !none && !running, egui::Button::new(RichText::new(text).strong()))
                .on_hover_text(hover)
                .on_disabled_hover_text(hover)
                .clicked()
            {
                self.start_run(ctx, sel);
            }
            for p in problems {
                ui.label(RichText::new(p).weak());
            }

            if let Some(job) = self.jobs.last_mut().filter(|j| j.finished.is_none()) {
                ui.add_space(14.0);
                ui.separator();
                ui.heading("In progress");
                ui.add_space(4.0);
                ui_progress(ui, ctx, job);
            }

            let done = self.jobs.iter().filter(|j| j.finished.is_some()).count();
            if done > 0 {
                ui.add_space(14.0);
                ui.separator();
                ui.heading("History");
                ui.label(RichText::new("Comparisons created since the app was opened, newest first.").small().weak());
                ui.add_space(4.0);
                let n = self.jobs.len();
                for (k, job) in self.jobs.iter().enumerate().rev().filter(|(_, j)| j.finished.is_some()) {
                    if let Some(d) = ui_history_entry(ui, job, k + 1 == n) {
                        view = Some(d);
                    }
                }
            }
        });
        if let Some(d) = view {
            self.viewer.open(&d);
            self.tab = Tab::Viewer;
        }
    }

    /// Read-only overview of what the Create button will make.
    fn ui_summary(&mut self, ui: &mut egui::Ui) {
        let all = self.complete_episodes();
        let sel = self.selected_episodes();
        let s = &self.setup;
        let weak = |t: String| RichText::new(t).weak();
        egui::Grid::new("summary").num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
            ui.label("Name");
            ui.label(s.display_name());
            ui.end_row();

            ui.label("Output folder");
            ui.label(if s.out_dir.trim().is_empty() { weak("not set".into()) } else { RichText::new(&s.out_dir) });
            ui.end_row();

            if !all.is_empty() {
                ui.label("Episodes");
                let list = sel.clone().unwrap_or_else(|| all.clone());
                ui.label(format!("{} of {}: {}", list.len(), all.len(), list.join(", ")));
                ui.end_row();
            }

            ui.label("Sources");
            if s.sources.is_empty() {
                ui.label(weak("none yet".into()));
            }
            ui.vertical(|ui| {
                egui::Grid::new("summary_sources").num_columns(4).spacing([14.0, 4.0]).show(ui, |ui| {
                    for (i, r) in s.sources.iter().enumerate() {
                        ui.strong(&r.label);
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
                        ui.label(format!("{fields}, matrix {}", matrix.to_lowercase()));
                        let offset = if i == 0 {
                            weak("reference".into())
                        } else {
                            let mut t = format!("offset {:+.3} s {:+} f", r.offset_seconds, r.offset_frames);
                            if !r.episode_offsets.is_empty() {
                                t += &format!(", own offset for {}", r.episode_offsets.keys().cloned().collect::<Vec<_>>().join(", "));
                            }
                            RichText::new(t)
                        };
                        ui.label(offset);
                        ui.label(weak(r.path.clone())).on_hover_text(&r.path);
                        ui.end_row();
                    }
                });
            });
            ui.end_row();

            ui.label("Frames");
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
            ui.label("Skip");
            ui.label(if skip.is_empty() { weak("nothing".into()) } else { RichText::new(skip.join(", ")) });
            ui.end_row();

            if let Some((idx, types)) = &s.frame_type {
                ui.label("Frame type");
                ui.label(format!("only {types} frames in {}", s.sources.get(*idx).map(|r| r.label.as_str()).unwrap_or("?")));
                ui.end_row();
            }

            ui.label("Picture");
            let mut pic = Vec::new();
            if s.stretch_anamorphic {
                pic.push("stretch anamorphic");
            }
            if s.crop_borders {
                pic.push("crop black borders");
            }
            ui.label(if pic.is_empty() { weak("as decoded".into()) } else { RichText::new(pic.join(", ")) });
            ui.end_row();
        });
    }
}

/// Live progress of the running job.
fn ui_progress(ui: &mut egui::Ui, ctx: &egui::Context, job: &mut Job) {
    ui.horizontal(|ui| {
        ui.strong(&job.title);
        let (i, total, ep) = &job.episode;
        if *total > 1 {
            ui.label(format!("episode {} of {total}  {ep}", i + 1));
        }
        ui.label(RichText::new(format!("{:.0}s", job.started.elapsed().as_secs_f64())).weak());
        ui.spinner();
        if ui.button("Cancel").clicked() {
            job.cancel.store(true, Ordering::Relaxed);
        }
    });
    ctx.request_repaint_after(std::time::Duration::from_millis(250));
    if job.episode.1 > 1 {
        let done = job.results.len() + job.failed.len();
        ui.add(egui::ProgressBar::new(done as f32 / job.episode.1 as f32).text(format!("{done}/{} episodes", job.episode.1)));
    }
    egui::Grid::new("progress").num_columns(3).spacing([12.0, 6.0]).show(ui, |ui| {
        for s in &job.sources {
            ui.label(&s.label);
            ui.add(
                egui::ProgressBar::new(s.indexing)
                    .desired_width(220.0)
                    .text(if s.indexing >= 1.0 { "indexed".to_string() } else { format!("indexing {:.0}%", s.indexing * 100.0) }),
            );
            match s.analysis {
                Some(a) => ui.add(
                    egui::ProgressBar::new(a)
                        .desired_width(220.0)
                        .text(if a >= 1.0 { "fields analysed".to_string() } else { format!("field analysis {:.0}%", a * 100.0) }),
                ),
                None => ui.label(""),
            };
            ui.end_row();
        }
    });
    if job.shots.1 > 0 {
        ui.add(egui::ProgressBar::new(job.shots.0 as f32 / job.shots.1 as f32).text(format!("screenshots {}/{}", job.shots.0, job.shots.1)));
    }
    ui_log(ui, job, "log_running", true);
}

/// One finished job: outcome, its comparisons (open in the viewer), failures and log.
/// Returns a comparison folder to open in the viewer when one was clicked.
fn ui_history_entry(ui: &mut egui::Ui, job: &Job, newest: bool) -> Option<PathBuf> {
    let mut view = None;
    let (status, color) = match &job.finished {
        Some(Ok(())) if job.failed.is_empty() => ("done".to_string(), Color32::from_rgb(80, 180, 100)),
        Some(Ok(())) => (format!("{} failed", job.failed.len()), Color32::from_rgb(220, 170, 60)),
        _ => ("stopped".to_string(), Color32::from_rgb(220, 80, 80)),
    };
    let header = RichText::new(format!("{}  ·  {status}  ·  {:.0}s", job.title, job.elapsed)).color(color);
    egui::CollapsingHeader::new(header).id_salt(("job", job.started)).default_open(newest).show(ui, |ui| {
        if let Some(Err(e)) = &job.finished {
            ui.colored_label(Color32::from_rgb(220, 80, 80), format!("Stopped: {e}"));
        }
        egui::Grid::new(("results", job.started)).striped(true).show(ui, |ui| {
            for r in &job.results {
                ui.label(&r.name);
                ui.label(RichText::new(format!("{} frames", r.frames.len())).weak());
                if ui.small_button("Open in Viewer").clicked() {
                    view = Some(r.out_dir.clone());
                }
                ui.end_row();
            }
        });
        for (ep, e) in &job.failed {
            ui.colored_label(Color32::from_rgb(220, 80, 80), format!("{ep}: {e}"));
        }
        ui_log(ui, job, &format!("log{:?}", job.started), false);
    });
    view
}

fn ui_log(ui: &mut egui::Ui, job: &Job, id: &str, open: bool) {
    egui::CollapsingHeader::new("Log").id_salt(id).default_open(open).show(ui, |ui| {
        egui::ScrollArea::vertical().id_salt(id).max_height(260.0).stick_to_bottom(true).auto_shrink([false, true]).show(ui, |ui| {
            for l in &job.log {
                ui.label(RichText::new(l).monospace().small());
            }
        });
    });
}
