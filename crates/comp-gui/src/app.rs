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
    Sources,
    Sync,
    Run,
}

struct SyncState {
    episode: usize,
    sent: Option<Vec<SourceConfig>>,
    ready: Option<(i32, Vec<f64>)>,
    frame: i32,
    last_req: Option<(i32, Vec<(f64, i32)>, bool)>,
    status: String,
    error: Option<String>,
    images: Vec<Result<(TextureHandle, i32, char, (u32, u32)), String>>,
    flicker: Option<usize>,
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

struct RunState {
    rx: Receiver<RunMsg>,
    cancel: Arc<AtomicBool>,
    started: Instant,
    log: Vec<String>,
    episode: (usize, usize, String),
    sources: Vec<SourceProgress>,
    shots: (usize, usize),
    results: Vec<CompResult>,
    failed: Vec<(String, String)>,
    index: Option<PathBuf>,
    finished: Option<Result<(), String>>,
    elapsed: f64,
}

pub struct App {
    setup: Setup,
    tab: Tab,
    pairing: Option<(Vec<String>, Result<Pairing, String>)>,
    worker: preview::Worker,
    sync: SyncState,
    run: Option<RunState>,
    toast: Option<(String, bool, Instant)>,
    project_path: Option<PathBuf>,
}

const STORAGE_KEY: &str = "setup";

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
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
        // Developer flags: --tab=sources|sync|run and --start (begin a run immediately).
        let args: Vec<String> = std::env::args().collect();
        let tab = match args.iter().find_map(|a| a.strip_prefix("--tab=")) {
            Some("sync") => Tab::Sync,
            Some("run") => Tab::Run,
            _ => Tab::Sources,
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
            },
            run: None,
            toast: None,
            project_path,
        };
        app.tab = tab;
        if autostart && app.setup.problems().is_empty() {
            app.start_run(&cc.egui_ctx);
        }
        app
    }

    fn notify(&mut self, msg: impl Into<String>, error: bool) {
        self.toast = Some((msg.into(), error, Instant::now()));
    }

    fn running(&self) -> bool {
        self.run.as_ref().is_some_and(|r| r.finished.is_none())
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
            self.tab = Tab::Sources;
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
                .add_filter("anime-comp project", &["toml"])
                .set_file_name(format!("{}.toml", comp_core::comp::file_label(&self.setup.display_name())))
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

    fn open_project(&mut self) {
        if let Some(p) = rfd::FileDialog::new().add_filter("anime-comp project", &["toml"]).pick_file() {
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

    fn start_run(&mut self, ctx: &egui::Context) {
        let cancel = Arc::new(AtomicBool::new(false));
        let spec = self.setup.to_spec(Some(cancel.clone()));
        let (tx, rx) = mpsc::channel();
        let ctx2 = ctx.clone();
        std::thread::Builder::new()
            .name("comparison".into())
            .spawn(move || {
                let send = |m: RunMsg| {
                    let _ = tx.send(m);
                    ctx2.request_repaint();
                };
                let res = season::run(&spec, &|e| send(RunMsg::Event(e)));
                send(RunMsg::Done(res.map_err(|e| e.to_string())));
            })
            .expect("spawn comparison thread");
        self.run = Some(RunState {
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
            index: None,
            finished: None,
            elapsed: 0.0,
        });
        self.tab = Tab::Run;
    }

    fn pump_run(&mut self) {
        let Some(run) = self.run.as_mut() else { return };
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
                        run.results.push(*result);
                    }
                    SeasonEvent::EpisodeFailed { episode, error, .. } => {
                        run.log.push(format!("{episode} failed: {error}"));
                        run.failed.push((episode, error));
                    }
                    SeasonEvent::IndexWritten(p) => run.index = Some(p),
                },
                RunMsg::Done(res) => {
                    run.elapsed = run.started.elapsed().as_secs_f64();
                    match res {
                        Ok(r) => {
                            run.index = r.index.or(run.index.take());
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

fn open_url(url: &str) {
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
                ui.heading("anime-comp");
                ui.add_space(12.0);
                let running = self.running();
                for (tab, name) in [(Tab::Sources, "1  Sources"), (Tab::Sync, "2  Sync"), (Tab::Run, "3  Run")] {
                    let enabled = !(running && tab != Tab::Run);
                    if ui.add_enabled(enabled, egui::SelectableLabel::new(self.tab == tab, name)).clicked() {
                        self.tab = tab;
                    }
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
            Tab::Sources => self.ui_sources(ui, ctx),
            Tab::Sync => self.ui_sync(ui, ctx),
            Tab::Run => self.ui_run(ui, ctx),
        });
    }
}

// ==================================================================================== Sources tab

impl App {
    fn ui_sources(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
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
                                egui::Grid::new("pairs").striped(true).show(ui, |ui| {
                                    ui.strong("Episode");
                                    for s in &self.setup.sources {
                                        ui.strong(&s.label);
                                    }
                                    ui.end_row();
                                    for r in &rows {
                                        ui.label(r.episode.to_string());
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
            ui.horizontal(|ui| {
                let ok = problems.is_empty();
                if ui.add_enabled(ok, egui::Button::new(RichText::new("Check sync").strong())).clicked() {
                    self.tab = Tab::Sync;
                }
                if ui.add_enabled(ok, egui::Button::new(RichText::new("Run comparison").strong())).clicked() {
                    self.start_run(ctx);
                }
            });
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
                    if !ref_row {
                        ui.add_space(10.0);
                        ui.label("Offset");
                        ui.add(egui::DragValue::new(&mut row.offset_seconds).speed(0.01).suffix(" s").max_decimals(3));
                        ui.add(egui::DragValue::new(&mut row.offset_frames).speed(0.2).suffix(" f"));
                    }
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

        // Offsets for non-reference sources.
        ui.horizontal_wrapped(|ui| {
            for (i, r) in self.setup.sources.iter_mut().enumerate().skip(1) {
                ui.group(|ui| {
                    ui.label(RichText::new(&r.label).strong());
                    if ui.small_button("-1").clicked() {
                        r.offset_frames -= 1;
                    }
                    ui.add(egui::DragValue::new(&mut r.offset_frames).speed(0.2).suffix(" f"));
                    if ui.small_button("+1").clicked() {
                        r.offset_frames += 1;
                    }
                    ui.add(egui::DragValue::new(&mut r.offset_seconds).speed(0.01).suffix(" s").max_decimals(3));
                    let _ = i;
                });
            }
            ui.separator();
            let mut flick = self.sync.flicker.is_some();
            if ui.checkbox(&mut flick, "Flicker").on_hover_text("Show one source at a time; keys 1-5 switch").changed() {
                self.sync.flicker = flick.then_some(0);
            }
        });

        // Request frames when anything relevant changed.
        if self.sync.ready.is_some() {
            let offsets: Vec<(f64, i32)> = self.setup.sources.iter().map(|r| (r.offset_seconds, r.offset_frames)).collect();
            let req = (self.sync.frame, offsets.clone(), self.setup.stretch_anamorphic);
            if self.sync.last_req.as_ref() != Some(&req) {
                self.worker.send(Request::Frame {
                    ref_frame: self.sync.frame,
                    offsets,
                    max_width: 1600,
                    stretch: self.setup.stretch_anamorphic,
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
                            ui.label(RichText::new(format!("{label}  ·  #{f}  ·  {pt}  ·  {nw}×{nh}")).small());
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

// ======================================================================================== Run tab

impl App {
    fn ui_run(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let Some(run) = self.run.as_mut() else {
            ui.label("No comparison has run yet.");
            let ok = self.setup.problems().is_empty();
            if ui.add_enabled(ok, egui::Button::new("Run comparison")).clicked() {
                self.start_run(ctx);
            }
            return;
        };
        let running = run.finished.is_none();
        ui.horizontal(|ui| {
            let (i, total, ep) = &run.episode;
            if *total > 1 {
                ui.label(format!("Episode {} of {total}  {ep}", i + 1));
            }
            let secs = if running { run.started.elapsed().as_secs_f64() } else { run.elapsed };
            ui.label(RichText::new(format!("{:.0}s", secs)).weak());
            if running {
                ui.spinner();
                if ui.button("Cancel").clicked() {
                    run.cancel.store(true, Ordering::Relaxed);
                }
                ctx.request_repaint_after(std::time::Duration::from_millis(250));
            }
        });
        if run.episode.1 > 1 {
            let done = run.results.len() + run.failed.len();
            ui.add(egui::ProgressBar::new(done as f32 / run.episode.1 as f32).text(format!("{done}/{} episodes", run.episode.1)));
        }
        ui.add_space(6.0);
        if running {
            egui::Grid::new("progress").num_columns(3).spacing([12.0, 6.0]).show(ui, |ui| {
                for s in &run.sources {
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
            if run.shots.1 > 0 {
                ui.add(egui::ProgressBar::new(run.shots.0 as f32 / run.shots.1 as f32).text(format!("screenshots {}/{}", run.shots.0, run.shots.1)));
            }
        }

        match &run.finished {
            Some(Ok(())) => {
                ui.add_space(8.0);
                ui.heading(format!("Done: {} comparison{}", run.results.len(), if run.results.len() == 1 { "" } else { "s" }));
                ui.horizontal(|ui| {
                    if let Some(idx) = &run.index {
                        if ui.button(RichText::new("Open viewer").strong()).clicked() {
                            open_external(idx);
                        }
                        if let Some(dir) = idx.parent() {
                            if ui.button("Show folder").clicked() {
                                open_external(dir);
                            }
                            if ui
                                .button("Upload to slow.pics manually…")
                                .on_hover_text("Opens the folder and slow.pics' upload page. Drag the PNGs in; names sort frame by frame.")
                                .clicked()
                            {
                                open_external(dir);
                                open_url("https://slow.pics/comparison");
                            }
                        }
                    }
                });
                if run.results.len() > 1 {
                    egui::Grid::new("results").striped(true).show(ui, |ui| {
                        for r in &run.results {
                            ui.label(&r.name);
                            ui.label(format!("{} frames", r.frames.len()));
                            if ui.small_button("Viewer").clicked() {
                                open_external(&r.out_dir.join("index.html"));
                            }
                            ui.end_row();
                        }
                    });
                }
                for (ep, e) in &run.failed {
                    ui.colored_label(Color32::from_rgb(220, 80, 80), format!("{ep}: {e}"));
                }
            }
            Some(Err(e)) => {
                ui.colored_label(Color32::from_rgb(220, 80, 80), format!("Stopped: {e}"));
            }
            None => {}
        }

        ui.add_space(8.0);
        egui::CollapsingHeader::new("Log").default_open(true).show(ui, |ui| {
            egui::ScrollArea::vertical().max_height(260.0).stick_to_bottom(true).auto_shrink([false, true]).show(ui, |ui| {
                for l in &run.log {
                    ui.label(RichText::new(l).monospace().small());
                }
            });
        });
        if !running && ui.button("Back to sources").clicked() {
            self.tab = Tab::Sources;
        }
    }
}
