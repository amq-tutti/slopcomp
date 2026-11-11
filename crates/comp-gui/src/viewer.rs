//! In-app comparison viewer: side by side / slider / flicker, synced zoom and pan, thumbnail
//! strip, and manual per-frame corrections (nudge a source ±1 frame, set an exact frame, replace
//! a frame). Edits run on a worker thread through `comp_core::edit::Editor`, which re-shoots the
//! PNG and updates comp.json and the HTML viewer.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};

use comp_core::comp::CompResult;
use comp_core::edit::Editor;
use crate::theme;
use eframe::egui::{self, Color32, Pos2, Rect, RichText, Sense, TextureHandle, TextureOptions, Vec2};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Grid,
    Slider,
    Flicker,
}

// ------------------------------------------------------------------------------------ workers

enum EditReq {
    Open(PathBuf),
    Set { rf: i32, s: usize, f: i32 },
    Nudge { rf: i32, s: usize, d: i32 },
    Replace { rf: i32, new: Option<i32> },
}

enum EditRep {
    Opened(Result<CompResult, String>),
    /// New state after an edit, and the files that were rewritten.
    Updated { result: CompResult, changed: Vec<String>, select_rf: i32 },
    Error(String),
}

fn spawn_editor(repaint: impl Fn() + Send + 'static) -> (Sender<EditReq>, Receiver<EditRep>) {
    let (tx, rx) = mpsc::channel::<EditReq>();
    let (rtx, rrx) = mpsc::channel::<EditRep>();
    std::thread::Builder::new()
        .name("viewer-edit".into())
        .spawn(move || {
            let mut ed: Option<Editor> = None;
            let send = |r: EditRep| {
                let _ = rtx.send(r);
                repaint();
            };
            for req in rx {
                match req {
                    EditReq::Open(dir) => match Editor::open(&dir) {
                        Ok(e) => {
                            send(EditRep::Opened(Ok(e.result.clone())));
                            ed = Some(e);
                        }
                        Err(e) => send(EditRep::Opened(Err(e.to_string()))),
                    },
                    other => {
                        let Some(e) = ed.as_mut() else { continue };
                        let before: Vec<String> = e.result.sources.iter().flat_map(|s| s.shots.iter().map(|x| x.file.clone())).collect();
                        let (res, select_rf) = match other {
                            EditReq::Set { rf, s, f } => (e.set_source_frame(rf, s, f), rf),
                            EditReq::Nudge { rf, s, d } => (e.nudge(rf, s, d), rf),
                            EditReq::Replace { rf, new } => {
                                let target = match new {
                                    Some(n) => Ok(n),
                                    None => e.random_replacement(rf),
                                };
                                match target {
                                    Ok(n) => (e.replace(rf, n), n),
                                    Err(err) => (Err(err), rf),
                                }
                            }
                            EditReq::Open(_) => unreachable!(),
                        };
                        match res {
                            Ok(()) => {
                                // Rewritten files: every file of the edited frame (same names are
                                // re-shot in place; replaced frames get new names).
                                let after: Vec<String> =
                                    e.result.sources.iter().flat_map(|s| s.shots.iter().map(|x| x.file.clone())).collect();
                                let mut changed: Vec<String> = after.iter().filter(|f| !before.contains(f)).cloned().collect();
                                if let Some(k) = e.result.frames.iter().position(|&f| f == select_rf) {
                                    changed.extend(e.result.sources.iter().map(|s| s.shots[k].file.clone()));
                                }
                                send(EditRep::Updated { result: e.result.clone(), changed, select_rf });
                            }
                            Err(err) => send(EditRep::Error(err.to_string())),
                        }
                    }
                }
            }
        })
        .expect("spawn viewer editor");
    (tx, rrx)
}

struct LoadReq {
    key: String,
    path: PathBuf,
    thumb: bool,
}

struct Loaded {
    key: String,
    image: Result<egui::ColorImage, String>,
}

fn spawn_loader(repaint: impl Fn() + Send + 'static) -> (Sender<LoadReq>, Receiver<Loaded>) {
    let (tx, rx) = mpsc::channel::<LoadReq>();
    let (rtx, rrx) = mpsc::channel::<Loaded>();
    std::thread::Builder::new()
        .name("viewer-load".into())
        .spawn(move || {
            for req in rx {
                let image = load_png(&req.path, req.thumb);
                let _ = rtx.send(Loaded { key: req.key, image });
                repaint();
            }
        })
        .expect("spawn viewer loader");
    (tx, rrx)
}

fn load_png(path: &Path, thumb: bool) -> Result<egui::ColorImage, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut dec = png::Decoder::new(std::io::BufReader::new(file));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut r = dec.read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![0; r.output_buffer_size()];
    let info = r.next_frame(&mut buf).map_err(|e| e.to_string())?;
    let (w, h) = (info.width as usize, info.height as usize);
    let rgb: Vec<u8> = match info.color_type {
        png::ColorType::Rgb => buf[..w * h * 3].to_vec(),
        png::ColorType::Rgba => buf[..w * h * 4].chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect(),
        png::ColorType::Grayscale => buf[..w * h].iter().flat_map(|&v| [v, v, v]).collect(),
        other => return Err(format!("unsupported PNG colour type {other:?}")),
    };
    if !thumb {
        return Ok(egui::ColorImage::from_rgb([w, h], &rgb));
    }
    // Area-average down to ~200 px wide for the strip.
    let f = (w / 200).max(1);
    let (tw, th) = (w / f, h / f);
    let mut out = vec![0u8; tw * th * 3];
    for y in 0..th {
        for x in 0..tw {
            let mut acc = [0u32; 3];
            for dy in 0..f {
                for dx in 0..f {
                    let p = ((y * f + dy) * w + x * f + dx) * 3;
                    acc[0] += rgb[p] as u32;
                    acc[1] += rgb[p + 1] as u32;
                    acc[2] += rgb[p + 2] as u32;
                }
            }
            let o = (y * tw + x) * 3;
            let a = (f * f) as u32;
            out[o] = (acc[0] / a) as u8;
            out[o + 1] = (acc[1] / a) as u8;
            out[o + 2] = (acc[2] / a) as u8;
        }
    }
    Ok(egui::ColorImage::from_rgb([tw, th], &out))
}

// ------------------------------------------------------------------------------------ viewer

pub struct Viewer {
    /// Comparison folders open in the viewer this session (oldest first); `dir` is one of them.
    list: Vec<PathBuf>,
    dir: Option<PathBuf>,
    result: Option<CompResult>,
    frame: usize,
    mode: Mode,
    flick: usize,
    left: usize,
    right: usize,
    split: f32,
    scale: f32,
    tx: f32,
    ty: f32,
    edit_tx: Sender<EditReq>,
    edit_rx: Receiver<EditRep>,
    load_tx: Sender<LoadReq>,
    load_rx: Receiver<Loaded>,
    textures: HashMap<String, TextureHandle>,
    pending: std::collections::HashSet<String>,
    /// Per-file version, bumped when an edit rewrites the file (invalidates textures).
    versions: HashMap<String, u32>,
    busy: bool,
    message: Option<(String, bool)>,
    replace_with: i32,
    /// Comparison frame `replace_with` was initialised for.
    replace_for: Option<i32>,
    /// Frame numbers being dragged/typed in the Adjust panel, keyed by (comparison frame, source).
    /// Applied when the drag ends or the field loses focus.
    frame_edits: HashMap<(i32, usize), i32>,
    show_fix: bool,
}

impl Viewer {
    pub fn new(ctx: &egui::Context) -> Self {
        let (c1, c2) = (ctx.clone(), ctx.clone());
        let (edit_tx, edit_rx) = spawn_editor(move || c1.request_repaint());
        let (load_tx, load_rx) = spawn_loader(move || c2.request_repaint());
        Self {
            list: Vec::new(),
            dir: None,
            result: None,
            frame: 0,
            mode: Mode::Grid,
            flick: 0,
            left: 0,
            right: 1,
            split: 0.5,
            scale: 1.0,
            tx: 0.0,
            ty: 0.0,
            edit_tx,
            edit_rx,
            load_tx,
            load_rx,
            textures: HashMap::new(),
            pending: Default::default(),
            versions: HashMap::new(),
            busy: false,
            message: None,
            replace_with: 0,
            replace_for: None,
            frame_edits: HashMap::new(),
            show_fix: false,
        }
    }

    /// Adds a comparison to the list without showing it (e.g. when a job finishes).
    pub fn add(&mut self, dir: &Path) {
        if !self.list.iter().any(|d| d == dir) {
            self.list.push(dir.to_owned());
        }
    }

    /// Removes a comparison from the list; if it was showing, shows the newest remaining one.
    fn close(&mut self, dir: &Path) {
        self.list.retain(|d| d != dir);
        if self.dir.as_deref() == Some(dir) {
            self.dir = None;
            self.result = None;
            self.textures.clear();
            self.pending.clear();
            self.versions.clear();
            self.frame_edits.clear();
            self.message = None;
            self.busy = false;
            if let Some(next) = self.list.last().cloned() {
                self.open(&next);
            }
        }
    }

    pub fn open(&mut self, dir: &Path) {
        self.add(dir);
        if self.dir.as_deref() == Some(dir) && self.result.is_some() {
            return;
        }
        self.dir = Some(dir.to_owned());
        self.result = None;
        self.textures.clear();
        self.pending.clear();
        self.versions.clear();
        self.frame = 0;
        self.busy = true;
        self.message = None;
        let _ = self.edit_tx.send(EditReq::Open(dir.to_owned()));
    }

    /// Opens a picked folder: the comparison itself if it has comp.json, otherwise every comparison
    /// found below it (added to the list, the first one shown).
    pub fn open_folder(&mut self, dir: &Path) {
        let found = find_comparisons(dir);
        let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| dir.display().to_string());
        match found.as_slice() {
            [] => {
                self.message = Some((
                    format!("No comparisons in \u{201c}{name}\u{201d}. Pick a folder that contains comp.json, or a folder above it."),
                    true,
                ));
            }
            [only] => self.open(&only.clone()),
            [first, rest @ ..] => {
                for d in rest {
                    self.add(d);
                }
                self.open(&first.clone());
                self.message = Some((format!("Opened {} comparisons from \u{201c}{name}\u{201d}.", found.len()), false));
            }
        }
    }

    fn key(&self, file: &str, thumb: bool) -> String {
        format!("{file}#{}#{}", self.versions.get(file).copied().unwrap_or(0), if thumb { "t" } else { "f" })
    }

    /// Texture for a screenshot (full size or thumbnail), requesting a load if needed.
    fn texture(&mut self, file: &str, thumb: bool) -> Option<TextureHandle> {
        let key = self.key(file, thumb);
        if let Some(t) = self.textures.get(&key) {
            return Some(t.clone());
        }
        if self.pending.insert(key.clone()) {
            let path = self.dir.as_ref()?.join(file);
            let _ = self.load_tx.send(LoadReq { key, path, thumb });
        }
        None
    }

    fn pump(&mut self, ctx: &egui::Context) {
        while let Ok(l) = self.load_rx.try_recv() {
            self.pending.remove(&l.key);
            if let Ok(img) = l.image {
                // Sharp pixels when zoomed in, smooth when scaled down.
                let opts = TextureOptions {
                    magnification: egui::TextureFilter::Nearest,
                    minification: egui::TextureFilter::Linear,
                    ..Default::default()
                };
                let tex = ctx.load_texture(&l.key, img, opts);
                self.textures.insert(l.key, tex);
            }
        }
        while let Ok(r) = self.edit_rx.try_recv() {
            self.busy = false;
            match r {
                EditRep::Opened(Ok(res)) => {
                    self.right = 1.min(res.sources.len().saturating_sub(1));
                    self.result = Some(res);
                }
                EditRep::Opened(Err(e)) => {
                    // A folder that vanished or lost its comp.json gets a plain explanation, not the OS error.
                    let gone = self.dir.as_deref().is_some_and(|d| !d.join("comp.json").is_file());
                    let msg = if gone {
                        let name = self.dir.as_deref().map(list_name).unwrap_or_default();
                        format!("Couldn't open \u{201c}{name}\u{201d}: its folder was moved or deleted, or it no longer has comp.json.")
                    } else {
                        format!("Couldn't open this comparison: {e}")
                    };
                    self.message = Some((msg, true));
                }
                EditRep::Updated { result, changed, select_rf } => {
                    for f in changed {
                        *self.versions.entry(f).or_insert(0) += 1;
                    }
                    if let Some(k) = result.frames.iter().position(|&f| f == select_rf) {
                        self.frame = k;
                    }
                    self.result = Some(result);
                    self.message = Some(("Saved. The screenshot, comp.json, and index.html are updated.".into(), false));
                }
                EditRep::Error(e) => self.message = Some((e, true)),
            }
        }
        // Keep the texture cache bounded: drop stale versions.
        if self.textures.len() > 120 {
            let keep: std::collections::HashSet<String> = self
                .result
                .iter()
                .flat_map(|r| r.sources.iter().flat_map(|s| s.shots.iter().map(|x| x.file.clone())))
                .flat_map(|f| [self.key(&f, true), self.key(&f, false)])
                .collect();
            self.textures.retain(|k, _| keep.contains(k));
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        self.pump(ctx);

        // Header: which comparison (its name, or the list when several are open); Open and Close on the right.
        ui.horizontal(|ui| {
            ui.set_min_height(30.0);
            let cur = self.dir.clone();
            let mut open: Option<PathBuf> = None;
            let mut close: Option<PathBuf> = None;
            let mut picked: Option<PathBuf> = None;
            let show_list = self.list.len() > 1 || (self.list.len() == 1 && cur.is_none());
            if show_list {
                // egui sizes the popup to the button and truncates wider rows, so size the
                // button for the longest name (plus the close button and scrollbar).
                let names: Vec<String> = self.list.iter().map(|d| list_name(d)).collect();
                let font = egui::TextStyle::Button.resolve(ui.style());
                let widest = ui.fonts(|f| {
                    names.iter().map(|n| f.layout_no_wrap(n.clone(), font.clone(), Color32::WHITE).size().x).fold(0.0, f32::max)
                });
                egui::ComboBox::from_id_salt("viewer_list")
                    .width((widest + 72.0).clamp(160.0, 540.0))
                    .height(420.0)
                    .selected_text(cur.as_deref().map(list_name).unwrap_or(format!("{} comparisons open", self.list.len())))
                    .show_ui(ui, |ui| {
                        for (d, name) in self.list.iter().zip(&names) {
                            // The row opens the comparison; the square × at the end closes it.
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 4.0;
                                let w = (ui.available_width() - 26.0 - 4.0).max(60.0);
                                let selected = cur.as_deref() == Some(d.as_path());
                                if theme::list_row(ui, selected, name, w).on_hover_text(d.display().to_string()).clicked() {
                                    open = Some(d.clone());
                                }
                                if theme::icon_button(ui, theme::Icon::Remove, true).on_hover_text("Close (files stay on disk)").clicked() {
                                    close = Some(d.clone());
                                }
                            });
                        }
                    })
                    .response
                    .on_hover_text("Switch between open comparisons");
            }
            // With several open, the list button already names the current one.
            if let Some(r) = self.result.as_ref().filter(|_| !show_list) {
                ui.label(RichText::new(&r.name).size(16.0).color(theme::TEXT_BRIGHT));
            }
            theme::busy(ui, "viewer", self.busy, theme::BG);
            if let Some((m, err)) = &self.message {
                ui.colored_label(if *err { crate::theme::ERR } else { ui.visuals().weak_text_color() }, m);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(c) = &cur {
                    if ui.button("Close").on_hover_text("Close this comparison (files stay on disk)").clicked() {
                        close = Some(c.clone());
                    }
                }
                if ui
                    .button("Open comparison…")
                    .on_hover_text("A comparison folder, or any folder above it: every comparison inside is opened")
                    .clicked()
                {
                    if let Some(d) = rfd::FileDialog::new().pick_folder() {
                        picked = Some(d);
                    }
                }
            });
            if let Some(d) = close {
                self.close(&d);
            } else if let Some(d) = open {
                self.open(&d);
            } else if let Some(d) = picked {
                self.open_folder(&d);
            }
        });
        ui.add_space(4.0);
        let Some(result) = self.result.clone() else {
            if !self.busy {
                ui.add_space(40.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("No comparison open").size(17.0).color(theme::TEXT_BRIGHT));
                    ui.add_space(4.0);
                    ui.label(RichText::new("Open a comparison folder, or create a new comparison in step 3, Create.").color(theme::TEXT_WEAK));
                });
            }
            return;
        };
        let n_src = result.sources.len();
        let n_frames = result.frames.len();
        if n_frames == 0 {
            ui.label("This comparison has no frames.");
            return;
        }
        self.frame = self.frame.min(n_frames - 1);
        self.flick = self.flick.min(n_src - 1);
        self.left = self.left.min(n_src - 1);
        self.right = self.right.min(n_src - 1);

        // Toolbar.
        theme::toolbar().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_wrapped(|ui| {
            let modes: [(Mode, &str, &str, &[&str]); 3] = [
                (Mode::Grid, "Side by side", "All sources at once", &["G"]),
                (Mode::Slider, "Slider", "Two sources, split by a line you drag", &["S"]),
                (Mode::Flicker, "Flicker", "One source at a time", &["F"]),
            ];
            let sel = modes.iter().position(|(m, ..)| *m == self.mode).unwrap_or(0);
            let opts: Vec<(&str, &str, &[&str])> = modes.iter().map(|(_, n, h, k)| (*n, *h, *k)).collect();
            if let Some(i) = theme::segmented(ui, "viewer_mode", &opts, sel) {
                self.mode = modes[i].0;
            }
            if self.mode != Mode::Grid {
                theme::vsep(ui);
            }
            match self.mode {
                Mode::Flicker => {
                    let names: Vec<String> = result.sources.iter().enumerate().map(|(i, s)| format!("{} {}", i + 1, s.label)).collect();
                    let keys: Vec<[String; 1]> = (1..=names.len()).map(|k| [k.to_string()]).collect();
                    let key_refs: Vec<[&str; 1]> = keys.iter().map(|k| [k[0].as_str()]).collect();
                    let opts: Vec<(&str, &str, &[&str])> =
                        names.iter().zip(&key_refs).map(|(n, k)| (n.as_str(), "Show this source", &k[..])).collect();
                    if let Some(i) = theme::segmented(ui, "viewer_flicker_source", &opts, self.flick) {
                        self.flick = i;
                    }
                }
                Mode::Slider => {
                    for (label, sel, id) in [("Left", &mut self.left, "vl"), ("Right", &mut self.right, "vr")] {
                        ui.label(label);
                        egui::ComboBox::from_id_salt(id).selected_text(result.sources[*sel].label.clone()).show_ui(ui, |ui| {
                            for (i, s) in result.sources.iter().enumerate() {
                                ui.selectable_value(sel, i, &s.label);
                            }
                        });
                    }
                }
                Mode::Grid => {}
            }
            theme::vsep(ui);
            if theme::key_hint(ui.button("Fit"), "Fit the frame to the window", &["0"]).clicked() {
                self.reset_zoom();
            }
            ui.label(RichText::new(format!("{:.0}%", self.scale * 100.0)).monospace().color(theme::TEXT_WEAK)).on_hover_text("Zoom (100% fits the window). Scroll to zoom, drag to pan.");
            theme::vsep(ui);
            let rf = result.frames[self.frame];
            let t = result.sources[0].shots[self.frame].reference_time;
            let r = ui.label(RichText::new(format!("Frame {} of {}", self.frame + 1, n_frames)).color(theme::TEXT_BRIGHT));
            theme::key_hint(r, "Previous or next frame", &["←", "→"]);
            ui.label(RichText::new(format!("reference #{rf} · {}:{:06.3}", (t / 60.0) as i64, t % 60.0)).color(theme::TEXT_WEAK))
                .on_hover_text("Frame number and time in the reference source");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.toggle_value(&mut self.show_fix, "Adjust frames").on_hover_cursor(egui::CursorIcon::PointingHand);
            });
        });
        });
        ui.add_space(6.0);

        // Keyboard (unless a text field has focus).
        if !ctx.wants_keyboard_input() {
            ctx.input(|i| {
                if i.key_pressed(egui::Key::ArrowRight) {
                    self.frame = (self.frame + 1).min(n_frames - 1);
                }
                if i.key_pressed(egui::Key::ArrowLeft) {
                    self.frame = self.frame.saturating_sub(1);
                }
                // Digit keys by physical key: with Shift held, the top-row keys arrive as "!", "@"…
                // on many layouts, so the logical key alone misses them (the numpad never did).
                for (idx, shift) in theme::digit_presses(i) {
                    if idx < n_src {
                        match self.mode {
                            // Slider: a digit puts that source on the right, Shift + digit on the left.
                            Mode::Slider if shift => self.left = idx,
                            Mode::Slider => self.right = idx,
                            _ => {
                                self.mode = Mode::Flicker;
                                self.flick = idx;
                            }
                        }
                    }
                }
                // Space steps through sources only while flickering; it doesn't switch modes.
                if i.key_pressed(egui::Key::Space) && self.mode == Mode::Flicker {
                    self.flick = (self.flick + 1) % n_src;
                }
                if i.key_pressed(egui::Key::G) {
                    self.mode = Mode::Grid;
                }
                if i.key_pressed(egui::Key::S) {
                    self.mode = Mode::Slider;
                }
                if i.key_pressed(egui::Key::F) {
                    self.mode = Mode::Flicker;
                }
                if i.key_pressed(egui::Key::Num0) {
                    self.reset_zoom();
                }
            });
        }

        // Adjust panel (right).
        if self.show_fix {
            egui::SidePanel::right("viewer_fix").resizable(false).default_width(250.0).show_inside(ui, |ui| {
                // Scrolls when the window is too short for the whole panel.
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| self.ui_fix(ui, &result));
            });
        }

        // Thumbnail strip (bottom).
        egui::TopBottomPanel::bottom("viewer_strip").resizable(false).show_inside(ui, |ui| {
            // Solid scrollbar in its own row below the thumbnails; egui's default floating bar
            // is drawn over them and hides the frame numbers when hovered.
            ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
            egui::ScrollArea::horizontal().show(ui, |ui| {
                ui.horizontal(|ui| {
                    for k in 0..n_frames {
                        let file = result.sources[0].shots[k].file.clone();
                        let size = Vec2::new(120.0, 68.0);
                        let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
                        let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
                        let painter = ui.painter_at(rect);
                        painter.rect_filled(rect, 3.0, Color32::BLACK);
                        if let Some(tex) = self.texture(&file, true) {
                            let fit = fit_rect(rect, tex.size_vec2());
                            painter.image(tex.id(), fit, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
                        }
                        // Frame number on a dark pill so it reads on light thumbnails too.
                        let galley = painter.layout_no_wrap(result.frames[k].to_string(), egui::FontId::proportional(11.0), Color32::WHITE);
                        let pad = Vec2::new(5.0, 2.0);
                        let pill = Rect::from_min_size(rect.right_bottom() - galley.size() - pad * 2.0 - Vec2::new(4.0, 4.0), galley.size() + pad * 2.0);
                        painter.rect_filled(pill, 4.0, Color32::from_black_alpha(180));
                        painter.galley(pill.min + pad, galley, Color32::WHITE);
                        if k == self.frame {
                            painter.rect_stroke(rect, 3.0, egui::Stroke::new(2.0, theme::ACCENT), egui::StrokeKind::Inside);
                        } else if resp.hovered() {
                            painter.rect_stroke(rect, 3.0, egui::Stroke::new(1.0, theme::TEXT_WEAK), egui::StrokeKind::Inside);
                        }
                        if resp.clicked() {
                            self.frame = k;
                        }
                    }
                });
            });
        });

        // Stage.
        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(Color32::from_gray(12))).show_inside(ui, |ui| {
            self.ui_stage(ui, &result);
        });
    }

    fn reset_zoom(&mut self) {
        self.scale = 1.0;
        self.tx = 0.0;
        self.ty = 0.0;
    }

    fn ui_stage(&mut self, ui: &mut egui::Ui, result: &CompResult) {
        let area = ui.available_rect_before_wrap();
        let k = self.frame;
        let shown: Vec<usize> = match self.mode {
            Mode::Grid => (0..result.sources.len()).collect(),
            Mode::Flicker => vec![self.flick],
            Mode::Slider => vec![self.left, self.right],
        };
        // Panel layout.
        let panels: Vec<Rect> = if self.mode == Mode::Grid {
            let n = shown.len();
            let ar = {
                let s = &result.sources[0].shots[k];
                s.width as f32 / s.height as f32
            };
            let mut best = (1usize, 0.0f32);
            for cols in 1..=n {
                let rows = n.div_ceil(cols);
                let w = (area.width() / cols as f32).min(area.height() / rows as f32 * ar);
                if w > best.1 {
                    best = (cols, w);
                }
            }
            let cols = best.0;
            let rows = n.div_ceil(cols);
            let (cw, ch) = (area.width() / cols as f32, area.height() / rows as f32);
            (0..n)
                .map(|i| {
                    let (c, r) = ((i % cols) as f32, (i / cols) as f32);
                    Rect::from_min_size(area.min + Vec2::new(c * cw, r * ch), Vec2::new(cw, ch)).shrink(2.0)
                })
                .collect()
        } else {
            vec![area.shrink(2.0)]
        };

        let resp = ui.allocate_rect(area, Sense::click_and_drag());
        // Zoom with the wheel around the pointer (normalised to the panel under it).
        if let Some(pos) = resp.hover_pos() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                let p = panels.iter().copied().find(|r| r.contains(pos)).unwrap_or(panels[0]);
                let (u, v) = ((pos.x - p.min.x) / p.width(), (pos.y - p.min.y) / p.height());
                let s2 = (self.scale * (scroll * 0.004).exp()).clamp(1.0, 64.0);
                let f = s2 / self.scale;
                self.tx = u - (u - self.tx) * f;
                self.ty = v - (v - self.ty) * f;
                self.scale = s2;
                self.clamp_pan();
            }
        }
        if resp.dragged() {
            let d = resp.drag_delta();
            let p = panels[0];
            if self.mode == Mode::Slider && self.scale <= 1.0 {
                if let Some(pos) = resp.interact_pointer_pos() {
                    self.split = ((pos.x - p.min.x) / p.width()).clamp(0.0, 1.0);
                }
            } else {
                self.tx += d.x / p.width();
                self.ty += d.y / p.height();
                self.clamp_pan();
            }
        }
        if resp.clicked() && self.mode == Mode::Slider {
            if let Some(pos) = resp.interact_pointer_pos() {
                self.split = ((pos.x - panels[0].min.x) / panels[0].width()).clamp(0.0, 1.0);
            }
        }

        let painter = ui.painter();
        let mut star_labels: Vec<Rect> = Vec::new();
        let mut label = |painter: &egui::Painter, rect: Rect, text: String, right: bool| {
            let has_star = text.contains('*');
            let galley = painter.layout_no_wrap(text, egui::FontId::proportional(12.0), Color32::WHITE);
            let pos = if right {
                Pos2::new(rect.max.x - galley.size().x - 10.0, rect.min.y + 8.0)
            } else {
                rect.min + Vec2::new(8.0, 8.0)
            };
            let bg = Rect::from_min_size(pos - Vec2::new(4.0, 2.0), galley.size() + Vec2::new(8.0, 4.0));
            painter.rect_filled(bg, 4.0, Color32::from_black_alpha(170));
            painter.galley(pos, galley, Color32::WHITE);
            if has_star {
                star_labels.push(bg);
            }
        };
        let shot_label = |s: usize| {
            let src = &result.sources[s];
            let sh = &src.shots[k];
            format!("{} · #{} · {} · {}×{}", src.label, sh.source_frame, sh.pict_type, sh.width, sh.height)
        };

        match self.mode {
            Mode::Grid | Mode::Flicker => {
                for (i, &s) in shown.iter().enumerate() {
                    let rect = panels[i];
                    painter.rect_filled(rect, 0.0, Color32::BLACK);
                    let file = result.sources[s].shots[k].file.clone();
                    if let Some(tex) = self.texture(&file, false) {
                        let img_rect = self.image_rect(rect, tex.size_vec2(), result);
                        painter.with_clip_rect(rect).image(tex.id(), img_rect, uv(), Color32::WHITE);
                    } else if let Some(tex) = self.texture(&file, true) {
                        let img_rect = self.image_rect(rect, tex.size_vec2(), result);
                        painter.with_clip_rect(rect).image(tex.id(), img_rect, uv(), Color32::WHITE);
                    }
                    label(painter, rect, shot_label(s), false);
                }
                // Preload the other sources and neighbouring frames.
                for s in 0..result.sources.len() {
                    let _ = self.texture(&result.sources[s].shots[k].file.clone(), false);
                }
            }
            Mode::Slider => {
                let rect = panels[0];
                painter.rect_filled(rect, 0.0, Color32::BLACK);
                let x = rect.min.x + self.split * rect.width();
                for (s, clip) in [
                    (self.left, Rect::from_min_max(rect.min, Pos2::new(x, rect.max.y))),
                    (self.right, Rect::from_min_max(Pos2::new(x, rect.min.y), rect.max)),
                ] {
                    let file = result.sources[s].shots[k].file.clone();
                    if let Some(tex) = self.texture(&file, false).or_else(|| self.texture(&file, true)) {
                        let img_rect = self.image_rect(rect, tex.size_vec2(), result);
                        // Each side clipped to its half, so differently shaped images never overlap.
                        painter.with_clip_rect(clip).image(tex.id(), img_rect, uv(), Color32::WHITE);
                    }
                }
                painter.line_segment([Pos2::new(x, rect.min.y), Pos2::new(x, rect.max.y)], egui::Stroke::new(2.0, Color32::WHITE));
                label(painter, rect, shot_label(self.left), false);
                label(painter, rect, shot_label(self.right), true);
            }
        }
        for (i, r) in star_labels.iter().enumerate() {
            ui.interact(*r, ui.id().with(("star", i)), Sense::hover()).on_hover_text(STAR_EXPLANATION);
        }
        for kk in [k.wrapping_sub(1), k + 1] {
            if kk < result.frames.len() {
                for s in 0..result.sources.len() {
                    let _ = self.texture(&result.sources[s].shots[kk].file.clone(), false);
                }
            }
        }
    }

    /// Where an image lands inside a panel: fitted (contain), then zoomed and panned. Images are
    /// fitted to the reference shot's aspect so different sources line up.
    fn image_rect(&self, panel: Rect, tex_size: Vec2, _result: &CompResult) -> Rect {
        let fit = fit_rect(panel, tex_size);
        let off = Vec2::new(self.tx * panel.width(), self.ty * panel.height());
        Rect::from_min_size(panel.min + (fit.min - panel.min) * self.scale + off, fit.size() * self.scale)
    }

    fn clamp_pan(&mut self) {
        let s = self.scale;
        self.tx = self.tx.clamp(1.0 - s, 0.0);
        self.ty = self.ty.clamp(1.0 - s, 0.0);
    }

    fn ui_fix(&mut self, ui: &mut egui::Ui, result: &CompResult) {
        let k = self.frame;
        let rf = result.frames[k];
        theme::section(ui, "Adjust frame");
        ui.label(RichText::new(format!("Frame {} · reference #{rf}", k + 1)).weak());
        ui.add_space(6.0);
        let busy = self.busy;
        for (s, src) in result.sources.iter().enumerate() {
            let cur = src.shots[k].source_frame;
            theme::tile().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&src.label).strong());
                    if s == 0 {
                        ui.label(RichText::new("reference").small().weak());
                    }
                });
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(!busy, |ui| {
                        if ui.button("−1").on_hover_text("Previous frame in this source").clicked() {
                            self.busy = true;
                            let _ = self.edit_tx.send(EditReq::Nudge { rf, s, d: -1 });
                        }
                        let key = (rf, s);
                        let mut f = self.frame_edits.get(&key).copied().unwrap_or(cur);
                        let r = ui.add(egui::DragValue::new(&mut f).speed(0.1).prefix("#"));
                        if r.changed() {
                            self.frame_edits.insert(key, f);
                        }
                        // Apply once the drag has ended / typing is finished (Enter or focus loss).
                        if !r.dragged() && !r.has_focus() {
                            if let Some(v) = self.frame_edits.remove(&key) {
                                if v != cur {
                                    self.busy = true;
                                    let _ = self.edit_tx.send(EditReq::Set { rf, s, f: v });
                                }
                            }
                        }
                        if ui.button("+1").on_hover_text("Next frame in this source").clicked() {
                            self.busy = true;
                            let _ = self.edit_tx.send(EditReq::Nudge { rf, s, d: 1 });
                        }
                    });
                });
                if s == 0 {
                    ui.label(RichText::new("The other sources don't follow when you move the reference.").small().weak());
                }
            });
        }
        ui.add_space(18.0);
        theme::section(ui, "Replace frame");
        ui.label(RichText::new("Pick a different moment. Every source gets a new screenshot, aligned with the same offsets.").small().weak());
        ui.add_space(4.0);
        let full = egui::vec2(ui.available_width(), 28.0);
        ui.add_enabled_ui(!busy, |ui| {
            if ui
                .add_sized(full, egui::Button::new("Random nearby frame"))
                .on_hover_text("Pick a random moment between this frame's neighbours")
                .clicked()
            {
                self.busy = true;
                let _ = self.edit_tx.send(EditReq::Replace { rf, new: None });
            }
            ui.horizontal(|ui| {
                if self.replace_for != Some(rf) {
                    self.replace_for = Some(rf);
                    self.replace_with = rf;
                }
                ui.label("Reference frame");
                ui.add(egui::DragValue::new(&mut self.replace_with).speed(1.0).prefix("#"))
                    .on_hover_text("Frame number in the reference source (the number in the file names)");
                let changed = self.replace_with != rf;
                if ui
                    .add_enabled(changed, egui::Button::new("Use it"))
                    .on_hover_text("Replace this frame with the reference frame number on the left")
                    .clicked()
                {
                    self.busy = true;
                    let _ = self.edit_tx.send(EditReq::Replace { rf, new: Some(self.replace_with) });
                }
            });
        });

        ui.add_space(18.0);
        theme::section(ui, "Comparison");
        ui.label(RichText::new("Applies to the whole comparison.").small().weak());
        ui.add_space(4.0);
        if let Some(d) = &self.dir {
            if ui.add_sized(full, egui::Button::new("Open HTML viewer")).clicked() {
                crate::app::open_external(&d.join("index.html"));
            }
            if ui.add_sized(full, egui::Button::new("Show folder")).clicked() {
                crate::app::open_external(d);
            }
            if ui
                .add_sized(full, egui::Button::new("Upload to slow.pics manually…"))
                .on_hover_text("Opens this folder and the slow.pics upload page. Drag all the PNGs in; their names keep the frames in order.")
                .clicked()
            {
                crate::app::open_external(d);
                crate::app::open_url("https://slow.pics/comparison");
            }
        }
    }
}

/// Name for the list of open comparisons. Season jobs put each episode in a folder named just
/// "E01", so those get the series folder in front ("To Heart · E01").
/// Folders with a comp.json at or below `root`, sorted by path (so E01, E02… stay in order). If `root`
/// itself is a comparison, only it. Skips hidden folders and symlinks; depth and entry count are
/// capped so picking a huge folder can't hang the app. Unreadable folders are skipped.
fn find_comparisons(root: &Path) -> Vec<PathBuf> {
    if root.join("comp.json").is_file() {
        return vec![root.to_owned()];
    }
    const MAX_DEPTH: usize = 6;
    const MAX_ENTRIES: usize = 50_000;
    let mut found = Vec::new();
    let mut seen = 0usize;
    let mut stack = vec![(root.to_owned(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            seen += 1;
            if seen > MAX_ENTRIES {
                break;
            }
            let Ok(ft) = entry.file_type() else { continue };
            if !ft.is_dir() || entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let path = entry.path();
            if path.join("comp.json").is_file() {
                found.push(path);
            } else if depth + 1 < MAX_DEPTH {
                stack.push((path, depth + 1));
            }
        }
    }
    found.sort();
    found
}

fn list_name(dir: &Path) -> String {
    let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    match dir.parent().and_then(|p| p.file_name()) {
        Some(parent) if comp_core::pairing::parse_episode(&name).is_some_and(|e| e.to_string() == name) => format!("{} · {name}", parent.to_string_lossy()),
        _ => name,
    }
}

/// Hover text for frame types marked with `*`.
pub const STAR_EXPLANATION: &str = "* = field-processed source (IVTC / deinterlaced). The screenshot is woven from two fields that can come from different decoded frames, so it has no single picture type; the letter is the type of the decoded frame it is mainly built from.";

fn uv() -> Rect {
    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0))
}

/// Largest rect with `size`'s aspect ratio centred in `outer`.
fn fit_rect(outer: Rect, size: Vec2) -> Rect {
    let s = (outer.width() / size.x).min(outer.height() / size.y);
    Rect::from_center_size(outer.center(), size * s)
}

#[cfg(test)]
mod tests {
    use super::find_comparisons;
    use std::fs;
    use std::path::Path;

    fn comp(dir: &Path) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("comp.json"), "{}").unwrap();
    }

    #[test]
    fn finds_comparisons_below_a_folder() {
        let root = std::env::temp_dir().join(format!("slopcomp-find-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        comp(&root.join("Show/E02"));
        comp(&root.join("Show/E01"));
        comp(&root.join("Movie"));
        comp(&root.join("deep/a/b/c/Nested"));
        comp(&root.join(".hidden/Skipped"));
        fs::create_dir_all(root.join("empty/stuff")).unwrap();

        // Relative paths with "/" on every platform (Windows prints "\").
        let rel = |v: Vec<std::path::PathBuf>| {
            v.iter()
                .map(|p| p.strip_prefix(&root).unwrap().components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/"))
                .collect::<Vec<_>>()
        };
        assert_eq!(rel(find_comparisons(&root)), ["Movie", "Show/E01", "Show/E02", "deep/a/b/c/Nested"]);
        // A comparison folder itself: just that one.
        assert_eq!(rel(find_comparisons(&root.join("Movie"))), ["Movie"]);
        // A folder with no comparisons.
        assert!(find_comparisons(&root.join("empty")).is_empty());
        let _ = fs::remove_dir_all(&root);
    }
}
