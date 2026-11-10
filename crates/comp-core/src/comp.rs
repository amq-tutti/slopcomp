//! End-to-end comparison runner: open sources → pick frames → decode → RGB → PNG → `comp.json`.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::convert::{self, ConvertOptions, PngCompression};
use crate::error::{Error, Result};
use crate::select;
use crate::source::{OpenOptions, VideoInfo, VideoSource};

/// One side of the comparison (e.g. "BD Remux", "WEB-DL").
#[derive(Debug, Clone)]
pub struct SourceSpec {
    pub label: String,
    pub path: PathBuf,
    /// Time offset in seconds: the reference frame at time `t` maps to the frame on screen at
    /// `t + offset_seconds` in this source.
    pub offset_seconds: f64,
    /// Frame offset applied after the time mapping ("this source is N frames behind").
    pub offset_frames: i32,
    pub matrix_override: Option<i32>,
    pub full_range_override: Option<bool>,
    pub sar_override: Option<(i32, i32)>,
}

impl SourceSpec {
    pub fn new(label: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self {
            label: label.into(),
            path: path.into(),
            offset_seconds: 0.0,
            offset_frames: 0,
            matrix_override: None,
            full_range_override: None,
            sar_override: None,
        }
    }
}

/// How the frames are picked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FramePick {
    /// `count` seeded, stratified random frames.
    Random { count: usize, seed: u64 },
    /// Explicit reference frame numbers.
    Explicit(Vec<i32>),
}

#[derive(Debug, Clone)]
pub struct CompSpec {
    pub name: String,
    /// The first source is the reference timeline.
    pub sources: Vec<SourceSpec>,
    pub pick: FramePick,
    pub out_dir: PathBuf,
    /// Global conversion settings. Per-source overrides come from [`SourceSpec`].
    pub convert: ConvertOptions,
    pub png: PngCompression,
    pub open: OpenOptions,
}

/// Progress events, emitted from worker threads.
#[derive(Debug, Clone)]
pub enum Event {
    Indexing { source: usize, done: i64, total: i64 },
    Opened { source: usize, info: VideoInfo },
    FramesPicked { frames: Vec<i32>, usable: (i32, i32) },
    Shot { source: usize, index: usize, path: PathBuf },
}

#[derive(Debug, Clone, Serialize)]
pub struct Shot {
    pub reference_frame: i32,
    pub source_frame: i32,
    /// Seconds from the start of the reference / of this source.
    pub reference_time: f64,
    pub source_time: f64,
    pub pict_type: String,
    pub file: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceResult {
    pub label: String,
    pub path: String,
    pub offset_seconds: f64,
    pub offset_frames: i32,
    pub vfr: bool,
    pub container: String,
    pub seek_mode: String,
    pub width: u32,
    pub height: u32,
    pub bit_depth: u32,
    pub num_frames: i32,
    pub fps: String,
    pub sar: (i32, i32),
    pub matrix: i32,
    pub matrix_guessed: bool,
    pub full_range: bool,
    pub shots: Vec<Shot>,
}

/// Everything about a finished comparison. Also written to `<out_dir>/comp.json`.
#[derive(Debug, Clone, Serialize)]
pub struct CompResult {
    pub name: String,
    pub tool_version: String,
    pub seed: Option<u64>,
    pub usable_range: (i32, i32),
    pub frames: Vec<i32>,
    pub sources: Vec<SourceResult>,
    #[serde(skip)]
    pub out_dir: PathBuf,
}

/// Makes a label safe for a file name, keeping it readable ("BD Remux" → "BD_Remux").
pub fn file_label(label: &str) -> String {
    let s: String = label
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '.' { c } else { '_' })
        .collect();
    let s = s.trim_matches('_').to_string();
    if s.is_empty() { "source".into() } else { s }
}

fn validate(spec: &CompSpec) -> Result<()> {
    if spec.sources.len() < 2 {
        return Err(Error::Spec("need at least two sources".into()));
    }
    let mut labels: Vec<_> = spec.sources.iter().map(|s| file_label(&s.label)).collect();
    labels.sort();
    if labels.windows(2).any(|w| w[0] == w[1]) {
        return Err(Error::Spec("source labels must be unique (after sanitising for file names)".into()));
    }
    let r = &spec.sources[0];
    if r.offset_frames != 0 || r.offset_seconds != 0.0 {
        return Err(Error::Spec(format!("'{}' is the reference timeline and can't have an offset", r.label)));
    }
    Ok(())
}

/// Runs a comparison. Sources are opened and decoded in parallel, one thread per source.
pub fn run(spec: &CompSpec, on_event: &(dyn Fn(Event) + Sync)) -> Result<CompResult> {
    validate(spec)?;
    std::fs::create_dir_all(&spec.out_dir)?;

    // Phase 1: open/index all sources in parallel.
    let opened: Vec<Result<VideoSource>> = std::thread::scope(|s| {
        let handles: Vec<_> = spec
            .sources
            .iter()
            .enumerate()
            .map(|(i, src)| {
                s.spawn(move || {
                    let vs = VideoSource::open_with_progress(&src.path, &spec.open, |done, total| {
                        on_event(Event::Indexing { source: i, done, total });
                        true
                    })?;
                    on_event(Event::Opened { source: i, info: vs.info().clone() });
                    Ok(vs)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().expect("source thread panicked")).collect()
    });
    let mut sources = Vec::with_capacity(opened.len());
    for (i, r) in opened.into_iter().enumerate() {
        sources.push(r.map_err(|e| Error::Spec(format!("{}: {e}", spec.sources[i].label)))?);
    }

    // Phase 2: map every reference frame into each source by timestamp, then pick frames inside
    // the range that every source covers.
    let ref_times = sources[0].times().to_vec();
    let maps: Vec<Vec<Option<i32>>> = sources
        .iter()
        .zip(&spec.sources)
        .enumerate()
        .map(|(i, (vs, s))| {
            if i == 0 {
                return (0..ref_times.len() as i32).map(Some).collect();
            }
            let n = vs.info().num_frames;
            ref_times
                .iter()
                .map(|&t| {
                    vs.frame_at_time(t + s.offset_seconds)
                        .map(|f| f + s.offset_frames)
                        .filter(|f| (0..n).contains(f))
                })
                .collect()
        })
        .collect();
    let valid: Vec<bool> = (0..ref_times.len()).map(|r| maps.iter().all(|m| m[r].is_some())).collect();
    let usable = select::usable_range(&valid)
        .ok_or_else(|| Error::Spec("sources don't overlap with the given offsets".into()))?;
    let (frames, seed) = match &spec.pick {
        FramePick::Random { count, seed } => {
            let picked = select::stratified(usable, *count, *seed);
            (picked.into_iter().filter(|&r| valid[r as usize]).collect(), Some(*seed))
        }
        FramePick::Explicit(list) => {
            let mut v = list.clone();
            v.sort_unstable();
            v.dedup();
            if let Some(bad) = v.iter().find(|&&f| f < 0 || f as usize >= valid.len() || !valid[f as usize]) {
                return Err(Error::Spec(format!(
                    "frame {bad} doesn't exist in every source (usable reference range [{}, {}))",
                    usable.0, usable.1
                )));
            }
            (v, None)
        }
    };
    on_event(Event::FramesPicked { frames: frames.clone(), usable });

    // Phase 3: decode + convert + write, one thread per source, frames in ascending order.
    let results: Vec<Result<SourceResult>> = std::thread::scope(|s| {
        let handles: Vec<_> = sources
            .into_iter()
            .zip(&spec.sources)
            .enumerate()
            .map(|(i, (mut vs, src))| {
                let targets: Vec<(i32, i32, f64)> =
                    frames.iter().map(|&r| (r, maps[i][r as usize].unwrap(), ref_times[r as usize])).collect();
                s.spawn(move || shoot_source(spec, i, src, &mut vs, &targets, on_event))
            })
            .collect();
        handles.into_iter().map(|h| h.join().expect("source thread panicked")).collect()
    });
    let sources = results.into_iter().collect::<Result<Vec<_>>>()?;

    let result = CompResult {
        name: spec.name.clone(),
        tool_version: env!("CARGO_PKG_VERSION").into(),
        seed,
        usable_range: usable,
        frames,
        sources,
        out_dir: spec.out_dir.clone(),
    };
    let json = serde_json::to_string_pretty(&result).expect("serialise comp.json");
    std::fs::write(spec.out_dir.join("comp.json"), json)?;
    Ok(result)
}

fn shoot_source(
    spec: &CompSpec,
    index: usize,
    src: &SourceSpec,
    vs: &mut VideoSource,
    targets: &[(i32, i32, f64)],
    on_event: &(dyn Fn(Event) + Sync),
) -> Result<SourceResult> {
    let info = vs.info().clone();
    let conv = ConvertOptions {
        matrix_override: src.matrix_override.or(spec.convert.matrix_override),
        full_range_override: src.full_range_override.or(spec.convert.full_range_override),
        sar_override: src.sar_override.or(spec.convert.sar_override),
        ..spec.convert.clone()
    };
    let label = file_label(&src.label);
    let mut shots = Vec::with_capacity(targets.len());
    let mut color = None;
    for (k, &(rf, sf, rt)) in targets.iter().enumerate() {
        let st = vs.times()[sf as usize];
        let frame = vs.frame(sf)?;
        let pict = frame.pict_type;
        let img = convert::to_rgb(&frame, info.sar, &conv)?;
        color.get_or_insert(img.color);
        let file = format!("{rf:06}_{label}.png");
        let path = spec.out_dir.join(&file);
        convert::write_png(&img, &path, spec.png)?;
        on_event(Event::Shot { source: index, index: k, path });
        shots.push(Shot {
            reference_frame: rf,
            source_frame: sf,
            reference_time: rt,
            source_time: st,
            pict_type: pict.to_string(),
            file,
            width: img.width,
            height: img.height,
        });
    }
    let color = color.unwrap_or_else(|| convert::resolve_color(&info.color, info.width, info.height, info.layout.is_jpeg_range, &conv));
    Ok(SourceResult {
        label: src.label.clone(),
        path: src.path.display().to_string(),
        offset_seconds: src.offset_seconds,
        offset_frames: src.offset_frames,
        vfr: is_vfr(vs.times()),
        container: info.container.clone(),
        seek_mode: format!("{:?}", info.seek_mode),
        width: info.width,
        height: info.height,
        bit_depth: info.layout.bit_depth,
        num_frames: info.num_frames,
        fps: format!("{}/{}", info.fps_num, info.fps_den),
        sar: info.sar,
        matrix: color.matrix,
        matrix_guessed: color.matrix_guessed,
        full_range: color.full_range,
        shots,
    })
}

/// Default output directory for a comparison name: `<base>/<sanitised name>`.
pub fn default_out_dir(base: &Path, name: &str) -> PathBuf {
    base.join(file_label(name))
}

/// True if frame durations vary by more than 2 ms (beyond timestamp rounding).
pub fn is_vfr(times: &[f64]) -> bool {
    let d: Vec<f64> = times.windows(2).map(|w| w[1] - w[0]).collect();
    match (d.iter().cloned().reduce(f64::min), d.iter().cloned().reduce(f64::max)) {
        (Some(lo), Some(hi)) => hi - lo > 0.002,
        _ => false,
    }
}
