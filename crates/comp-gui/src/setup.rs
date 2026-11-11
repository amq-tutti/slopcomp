//! The editable comparison setup behind the GUI, with conversions to a runnable [`CompSpec`]
//! and to/from TOML project files.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use comp_core::comp::{CompSpec, FramePick, SelectOptions, SourceSpec};
use comp_core::convert::ConvertOptions;
use comp_core::filter::{FieldMode, FieldOptions, FieldOrder};
use comp_core::project::{FrameTypeRule, Project, ProjectSource, Rules};
use comp_core::source::{self, OpenOptions};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Field {
    Progressive,
    Ivtc,
    Deinterlace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Order {
    Auto,
    Tff,
    Bff,
}

/// Matrix choices offered in the UI (H.273 code, label).
pub const MATRICES: [(Option<i32>, &str); 5] =
    [(None, "Auto"), (Some(1), "BT.709"), (Some(6), "BT.601 (NTSC)"), (Some(5), "BT.470BG (PAL)"), (Some(9), "BT.2020")];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceRow {
    pub label: String,
    /// File or folder.
    pub path: String,
    pub field: Field,
    pub order: Order,
    pub offset_seconds: f64,
    pub offset_frames: i32,
    /// Per-episode offsets (seconds, frames) set in the Sync tab, keyed by episode ("E02").
    #[serde(default)]
    pub episode_offsets: std::collections::BTreeMap<String, (f64, i32)>,
    pub matrix: Option<i32>,
}

impl SourceRow {
    pub fn new(label: String, path: String) -> Self {
        Self {
            label,
            path,
            field: Field::Progressive,
            order: Order::Auto,
            offset_seconds: 0.0,
            offset_frames: 0,
            episode_offsets: Default::default(),
            matrix: None,
        }
    }

    /// Offset used for an episode: its own one if set in Sync, else the default.
    pub fn offset_for(&self, episode: Option<&str>) -> (f64, i32) {
        episode
            .and_then(|e| self.episode_offsets.get(e).copied())
            .unwrap_or((self.offset_seconds, self.offset_frames))
    }

    pub fn field_options(&self) -> Option<FieldOptions> {
        let order = match self.order {
            Order::Auto => FieldOrder::Auto,
            Order::Tff => FieldOrder::Tff,
            Order::Bff => FieldOrder::Bff,
        };
        match self.field {
            Field::Progressive => None,
            Field::Ivtc => Some(FieldOptions { mode: FieldMode::Ivtc, order }),
            Field::Deinterlace => Some(FieldOptions { mode: FieldMode::Deinterlace, order }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)] // fields added later get their default, so older saved setups still load
pub struct Setup {
    pub name: String,
    pub out_dir: String,
    pub sources: Vec<SourceRow>,
    pub frames: usize,
    pub seed: u64,
    pub skip_black: bool,
    pub black_threshold: f32,
    pub skip_scene_changes: bool,
    pub skip_duplicates: bool,
    pub skip_flat: bool,
    pub exclude_op_ed: bool,
    /// (source index, types such as "B").
    pub frame_type: Option<(usize, String)>,
    pub stretch_anamorphic: bool,
    /// Detect and crop black borders (pillarbox/letterbox).
    pub crop_borders: bool,
    /// Episodes unticked in the pairing table (e.g. "E05"); everything else runs.
    pub skip_episodes: Vec<String>,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            name: String::new(),
            out_dir: std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join("Pictures").join("SlopComp"))
                .unwrap_or_else(|| PathBuf::from("comps"))
                .to_string_lossy()
                .into_owned(),
            sources: Vec::new(),
            frames: 20,
            seed: random_seed(),
            skip_black: true,
            black_threshold: 0.03,
            skip_scene_changes: true,
            skip_duplicates: false,
            skip_flat: false,
            exclude_op_ed: false,
            frame_type: None,
            stretch_anamorphic: true,
            crop_borders: true,
            skip_episodes: Vec::new(),
        }
    }
}

pub fn random_seed() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64 % 1_000_000)
        .unwrap_or(1)
}

/// A label from a dropped path: the folder name or file stem, shortened.
pub fn label_for(path: &Path, taken: &[String]) -> String {
    let raw = if path.is_dir() { path.file_name() } else { path.file_stem() }
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Source".into());
    // Prefer a bracketed release group ("[Group] Show …") as the label.
    let base = raw
        .strip_prefix('[')
        .and_then(|r| r.split_once(']'))
        .map(|(g, _)| g.to_string())
        .unwrap_or_else(|| raw.chars().take(20).collect());
    let mut label = base.clone();
    let mut n = 2;
    while taken.contains(&label) {
        label = format!("{base} {n}");
        n += 1;
    }
    label
}

impl Setup {
    pub fn display_name(&self) -> String {
        if !self.name.trim().is_empty() {
            return self.name.trim().to_string();
        }
        let labels: Vec<&str> = self.sources.iter().map(|s| s.label.as_str()).collect();
        let first = self.sources.first().map(|s| {
            let p = Path::new(&s.path);
            if p.is_dir() { p.file_name() } else { p.file_stem() }.map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
        });
        format!("{} - {}", first.unwrap_or_default(), labels.join(" vs "))
    }

    /// Problems that prevent a run, in plain words.
    pub fn problems(&self) -> Vec<String> {
        let mut v = Vec::new();
        if self.sources.len() < 2 {
            v.push("Add at least two sources (drop files or folders onto the window).".into());
        }
        for s in &self.sources {
            if s.label.trim().is_empty() {
                v.push("Every source needs a label.".into());
                break;
            }
        }
        let mut labels: Vec<String> = self.sources.iter().map(|s| comp_core::comp::shot_label(&s.label).to_lowercase()).collect();
        labels.sort();
        if labels.windows(2).any(|w| w[0] == w[1]) {
            v.push("Source labels must be different.".into());
        }
        for s in &self.sources {
            if !Path::new(&s.path).exists() {
                v.push(format!("{}: path not found", s.label));
            }
        }
        v
    }

    pub fn to_spec(&self, cancel: Option<Arc<AtomicBool>>) -> CompSpec {
        let sources = self
            .sources
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let mut s = SourceSpec::new(r.label.trim(), PathBuf::from(&r.path));
                if i > 0 {
                    s.offset_seconds = r.offset_seconds;
                    s.offset_frames = r.offset_frames;
                    s.episode_offsets = r.episode_offsets.clone();
                }
                s.matrix_override = r.matrix;
                s.field = r.field_options();
                s.auto_crop = self.crop_borders;
                s
            })
            .collect();
        let select = SelectOptions {
            dark_threshold: self.skip_black.then_some(self.black_threshold),
            reject_transitions: self.skip_scene_changes,
            duplicate_distance: self.skip_duplicates.then_some(6),
            flat_threshold: self.skip_flat.then_some(0.02),
            frame_type: self.frame_type.as_ref().and_then(|(i, t)| {
                self.sources.get(*i).map(|s| (s.label.trim().to_string(), t.to_ascii_uppercase().chars().collect()))
            }),
            exclude_chapter_op_ed: self.exclude_op_ed,
            ..Default::default()
        };
        let name = self.display_name();
        CompSpec {
            out_dir: comp_core::comp::default_out_dir(Path::new(&self.out_dir), &name),
            name,
            sources,
            pick: FramePick::Random { count: self.frames, seed: self.seed },
            convert: ConvertOptions { apply_sar: self.stretch_anamorphic, ..Default::default() },
            png: Default::default(),
            open: OpenOptions { index_cache: source::default_cache_dir(), ..Default::default() },
            select,
            cancel,
        }
    }

    pub fn to_project(&self) -> Project {
        Project {
            name: (!self.name.trim().is_empty()).then(|| self.name.trim().to_string()),
            out: PathBuf::from(&self.out_dir),
            frames: self.frames,
            seed: Some(self.seed),
            frame: vec![],
            exclude_op_ed: self.exclude_op_ed,
            exclude: vec![],
            frame_type: self.frame_type.as_ref().and_then(|(i, t)| {
                self.sources.get(*i).map(|s| FrameTypeRule { source: s.label.clone(), types: t.clone() })
            }),
            rules: Rules {
                black_threshold: self.skip_black.then_some((self.black_threshold as f64 * 1000.0).round() / 1000.0),
                scene_changes: self.skip_scene_changes,
                duplicates: self.skip_duplicates,
                flat: self.skip_flat,
            },
            no_sar: !self.stretch_anamorphic,
            no_crop: !self.crop_borders,
            sources: self
                .sources
                .iter()
                .map(|r| {
                    let order = match r.order {
                        Order::Auto => "auto",
                        Order::Tff => "tff",
                        Order::Bff => "bff",
                    };
                    let mut offs = Vec::new();
                    if r.offset_seconds != 0.0 {
                        offs.push(format!("{}s", r.offset_seconds));
                    }
                    if r.offset_frames != 0 {
                        offs.push(r.offset_frames.to_string());
                    }
                    let fmt = |(s, f): (f64, i32)| {
                        let mut v = Vec::new();
                        if s != 0.0 {
                            v.push(format!("{s}s"));
                        }
                        if f != 0 || v.is_empty() {
                            v.push(f.to_string());
                        }
                        v.join(",")
                    };
                    ProjectSource {
                        episode_offsets: r.episode_offsets.iter().map(|(e, o)| (e.clone(), fmt(*o))).collect(),
                        label: r.label.clone(),
                        path: PathBuf::from(&r.path),
                        offset: (!offs.is_empty()).then(|| offs.join(",")),
                        ivtc: (r.field == Field::Ivtc).then(|| order.to_string()),
                        deint: (r.field == Field::Deinterlace).then(|| order.to_string()),
                        matrix: r.matrix.map(|m| m.to_string()),
                    }
                })
                .collect(),
        }
    }

    pub fn from_project(p: &Project) -> Result<Self, String> {
        let mut s = Setup {
            name: p.name.clone().unwrap_or_default(),
            out_dir: p.out.to_string_lossy().into_owned(),
            frames: p.frames,
            seed: p.seed.unwrap_or_else(random_seed),
            skip_black: p.rules.black_threshold.is_some(),
            black_threshold: p.rules.black_threshold.unwrap_or(0.03) as f32,
            skip_scene_changes: p.rules.scene_changes,
            skip_duplicates: p.rules.duplicates,
            skip_flat: p.rules.flat,
            exclude_op_ed: p.exclude_op_ed,
            stretch_anamorphic: !p.no_sar,
            crop_borders: !p.no_crop,
            ..Default::default()
        };
        for ps in &p.sources {
            let mut r = SourceRow::new(ps.label.clone(), ps.path.to_string_lossy().into_owned());
            let order_of = |o: &str| match o {
                "tff" => Order::Tff,
                "bff" => Order::Bff,
                _ => Order::Auto,
            };
            if let Some(o) = &ps.ivtc {
                r.field = Field::Ivtc;
                r.order = order_of(o);
            } else if let Some(o) = &ps.deint {
                r.field = Field::Deinterlace;
                r.order = order_of(o);
            }
            for part in ps.offset.iter().flat_map(|o| o.split(',')) {
                let part = part.trim();
                if let Some(sec) = part.strip_suffix('s') {
                    r.offset_seconds += sec.parse::<f64>().map_err(|_| format!("offset \"{part}\" isn't valid. Use seconds (\"1.5s\") or frames (\"12\")."))?;
                } else if !part.is_empty() {
                    r.offset_frames += part.parse::<i32>().map_err(|_| format!("offset \"{part}\" isn't valid. Use seconds (\"1.5s\") or frames (\"12\")."))?;
                }
            }
            for (ep, o) in &ps.episode_offsets {
                let (mut s, mut f) = (0.0, 0);
                for part in o.split(',') {
                    let part = part.trim();
                    if let Some(sec) = part.strip_suffix('s') {
                        s += sec.parse::<f64>().map_err(|_| format!("offset \"{part}\" isn't valid. Use seconds (\"1.5s\") or frames (\"12\")."))?;
                    } else if !part.is_empty() {
                        f += part.parse::<i32>().map_err(|_| format!("offset \"{part}\" isn't valid. Use seconds (\"1.5s\") or frames (\"12\")."))?;
                    }
                }
                r.episode_offsets.insert(ep.clone(), (s, f));
            }
            r.matrix = match ps.matrix.as_deref() {
                None => None,
                Some("709") => Some(1),
                Some("601") | Some("170m") => Some(6),
                Some("470bg") | Some("pal") => Some(5),
                Some("2020") => Some(9),
                Some(m) => Some(m.parse().map_err(|_| format!("matrix \"{m}\" isn't known. Use 709, 601, 470bg, or 2020."))?),
            };
            s.sources.push(r);
        }
        if let Some(ft) = &p.frame_type {
            if let Some(i) = s.sources.iter().position(|r| r.label == ft.source) {
                s.frame_type = Some((i, ft.types.clone()));
            }
        }
        Ok(s)
    }
}
