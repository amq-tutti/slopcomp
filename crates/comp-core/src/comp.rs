//! End-to-end comparison runner: open sources → pick frames → decode → RGB → PNG → `comp.json`.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::convert::{self, ConvertOptions, PngCompression};
use crate::error::{Error, Result};
use crate::select;
use crate::filter::{AnySource, FieldOptions, FilteredSource};
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
    /// IVTC / deinterlacing for interlaced or telecined sources.
    pub field: Option<FieldOptions>,
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
            field: None,
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
    /// Rules for random frame picking.
    pub select: SelectOptions,
}

/// Rules for random frame picking. Explicit frame lists bypass all of these.
#[derive(Debug, Clone)]
pub struct SelectOptions {
    /// Reject frames whose mean luma (0..1 between black and white level) is below this.
    /// The default (0.03) only catches black screens, not dark scenes.
    pub dark_threshold: Option<f32>,
    /// Reject frames next to a scene cut or inside a fade.
    pub reject_transitions: bool,
    /// Reject frames whose dHash is within this Hamming distance of an already picked frame.
    pub duplicate_distance: Option<u32>,
    /// Reject nearly flat frames (solid colours, title cards) below this thumbnail std-dev.
    pub flat_threshold: Option<f32>,
    /// Only accept frames whose picture type in the given source (by label) is one of these
    /// (e.g. `['B']` to compare encoder behaviour on B-frames).
    pub frame_type: Option<(String, Vec<char>)>,
    /// Leave out OP/ED found in chapter names (from whichever source has them).
    pub exclude_chapter_op_ed: bool,
    /// Extra excluded ranges on the reference timeline, in seconds.
    pub exclude: Vec<(f64, f64)>,
    /// Candidates tried per bin before falling back to the least-bad one.
    pub max_tries: usize,
}

impl Default for SelectOptions {
    /// Defaults keep all episode content (OP/ED included) and only skip black screens and
    /// scene changes (cuts and fades). The other rules are opt-in.
    fn default() -> Self {
        Self {
            dark_threshold: Some(0.03),
            reject_transitions: true,
            duplicate_distance: None,
            flat_threshold: None,
            frame_type: None,
            exclude_chapter_op_ed: false,
            exclude: Vec::new(),
            max_tries: 16,
        }
    }
}

/// Progress events, emitted from worker threads.
#[derive(Debug, Clone)]
pub enum Event {
    Indexing { source: usize, done: i64, total: i64 },
    /// Field-processing analysis pass (IVTC / deinterlace), once per file and then cached.
    Analyzing { source: usize, done: i64, total: i64 },
    Opened { source: usize, info: VideoInfo },
    FramesPicked { frames: Vec<i32>, usable: (i32, i32) },
    /// Ranges left out of picking (reference seconds), with the reason ("OP (chapter)", …).
    Excluded { ranges: Vec<(f64, f64, String)> },
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
    /// "ivtc-tff", "deint-bff", … or null.
    pub field_processing: Option<String>,
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
    /// Reference-timeline ranges (seconds) left out of picking, with reasons.
    pub excluded: Vec<(f64, f64, String)>,
    /// Candidates rejected during picking, with reasons (reference frame numbers).
    pub rejected: Vec<(i32, String)>,
    /// Frames used despite failing a rule because their bin had nothing better.
    pub fallbacks: Vec<(i32, String)>,
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
    let opened: Vec<Result<AnySource>> = std::thread::scope(|s| {
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
                    let vs = match src.field {
                        None => AnySource::Plain(vs),
                        Some(fo) => {
                            let fs = FilteredSource::new(vs, fo, spec.open.index_cache.as_deref(), |done, total| {
                                on_event(Event::Analyzing { source: i, done, total });
                                true
                            })?;
                            AnySource::Filtered(Box::new(fs))
                        }
                    };
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
    let excluded = excluded_ranges(spec, &sources)?;
    if !excluded.is_empty() {
        on_event(Event::Excluded { ranges: excluded.clone() });
    }
    let mut rejected = Vec::new();
    let mut fallbacks = Vec::new();
    let (frames, seed) = match &spec.pick {
        FramePick::Random { count, seed } => {
            // Excluded seconds → reference frame ranges; invalid (unmapped) frames too.
            let mut ex: Vec<(i32, i32)> = excluded
                .iter()
                .map(|&(a, b, _)| {
                    let fa = ref_times.partition_point(|&t| t < a) as i32;
                    let fb = ref_times.partition_point(|&t| t < b) as i32;
                    (fa, fb)
                })
                .collect();
            let mut r = usable.0;
            while r < usable.1 {
                if !valid[r as usize] {
                    let s0 = r;
                    while r < usable.1 && !valid[r as usize] {
                        r += 1;
                    }
                    ex.push((s0, r));
                }
                r += 1;
            }
            let allowed = select::Segments::from_range_minus(usable, &ex);
            let rep = pick_random(spec, &mut sources, &maps, &allowed, *count, *seed)?;
            rejected = rep.rejected.iter().map(|&(f, r)| (f, r.to_string())).collect();
            fallbacks = rep.fallbacks.iter().map(|&(f, r)| (f, r.to_string())).collect();
            (rep.frames, Some(*seed))
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
        excluded,
        rejected,
        fallbacks,
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
    vs: &mut AnySource,
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
        field_processing: src.field.map(|f| {
            let tff = match vs {
                AnySource::Filtered(fs) => fs.top_field_first(),
                _ => true,
            };
            format!("{}-{}", match f.mode { crate::filter::FieldMode::Ivtc => "ivtc", _ => "deint" }, if tff { "tff" } else { "bff" })
        }),
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

/// OP/ED (from chapters) and manual exclusions, as reference-timeline seconds.
fn excluded_ranges(spec: &CompSpec, sources: &[AnySource]) -> Result<Vec<(f64, f64, String)>> {
    let sel = &spec.select;
    let mut out: Vec<(f64, f64, String)> = sel.exclude.iter().map(|&(a, b)| (a, b, "manual".to_string())).collect();
    if sel.exclude_chapter_op_ed && matches!(spec.pick, FramePick::Random { .. }) {
        // Use the first source (reference first) that has named OP/ED chapters.
        for (src, vs) in spec.sources.iter().zip(sources) {
            let chapters = crate::chapters::read(vs.path()).unwrap_or_default();
            let ranges = crate::chapters::op_ed_ranges(&chapters);
            if ranges.is_empty() {
                continue;
            }
            for (a, b, kind) in ranges {
                // Chapter times are absolute container times in this source. Convert to its
                // normalised timeline, then to the reference timeline via its time offset.
                let to_ref = |t: f64| t - vs.start_time() - src.offset_seconds;
                let what = match kind {
                    crate::chapters::ChapterKind::Opening => "OP",
                    _ => "ED",
                };
                out.push((to_ref(a), to_ref(b), format!("{what} (chapter in {})", src.label)));
            }
            break;
        }
    }
    Ok(out)
}

/// Seeded picking with rejection rules judged on the reference (content) and, for frame types,
/// on the chosen source.
fn pick_random(
    spec: &CompSpec,
    sources: &mut [AnySource],
    maps: &[Vec<Option<i32>>],
    allowed: &select::Segments,
    count: usize,
    seed: u64,
) -> Result<select::PickReport> {
    use crate::stats::FrameStats;
    use select::Verdict;
    use std::collections::HashMap;

    let sel = &spec.select;
    let type_src = match &sel.frame_type {
        Some((label, types)) => {
            let i = spec
                .sources
                .iter()
                .position(|s| &s.label == label)
                .ok_or_else(|| Error::Spec(format!("frame-type source '{label}' not found")))?;
            if sources[i].is_filtered() {
                return Err(Error::Spec(format!(
                    "'{label}' is IVTC'd/deinterlaced; picture types don't exist after field processing"
                )));
            }
            Some((i, types.clone()))
        }
        None => None,
    };
    let n_ref = sources[0].info().num_frames;
    let mut cache: HashMap<i32, FrameStats> = HashMap::new();
    let mut stats = |sources: &mut [AnySource], r: i32| -> Result<Option<FrameStats>> {
        if r < 0 || r >= n_ref {
            return Ok(None);
        }
        if let Some(s) = cache.get(&r) {
            return Ok(Some(s.clone()));
        }
        let st = FrameStats::of(&sources[0].frame(r)?);
        cache.insert(r, st.clone());
        Ok(Some(st))
    };
    let mut picked_hashes: Vec<(i32, u64)> = Vec::new();

    let rep = select::pick(allowed, count, seed, sel.max_tries, |r, picked| -> Result<Verdict> {
        picked_hashes.retain(|(f, _)| picked.contains(f));
        // 1. Frame type (cheapest to reject early on, one decode).
        if let Some((ti, types)) = &type_src {
            let f = maps[*ti][r as usize].unwrap();
            let pt = sources[*ti].frame(f)?.pict_type;
            if !types.contains(&pt) {
                return Ok(Verdict::Reject { reason: "frame type", severity: 1 });
            }
        }
        // 2. Content rules, judged on the reference.
        let cur = stats(sources, r)?.unwrap();
        if sel.dark_threshold.is_some_and(|t| cur.mean < t) {
            return Ok(Verdict::Reject { reason: "dark", severity: 4 });
        }
        if sel.flat_threshold.is_some_and(|t| cur.detail < t) {
            return Ok(Verdict::Reject { reason: "flat", severity: 4 });
        }
        if let Some(d) = sel.duplicate_distance {
            if picked_hashes.iter().any(|&(_, h)| (h ^ cur.dhash).count_ones() <= d) {
                return Ok(Verdict::Reject { reason: "duplicate", severity: 2 });
            }
        }
        if sel.reject_transitions {
            const CUT: f32 = 0.15;
            let prev = stats(sources, r - 1)?;
            let next = stats(sources, r + 1)?;
            if prev.as_ref().is_some_and(|p| p.diff(&cur) > CUT) || next.as_ref().is_some_and(|n| n.diff(&cur) > CUT) {
                return Ok(Verdict::Reject { reason: "scene cut", severity: 3 });
            }
            // Fade: brightness changes steadily in one direction over ±2 frames.
            let window: Vec<f32> = (r - 2..=r + 2)
                .map(|f| stats(sources, f).map(|s| s.map(|s| s.mean)))
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .flatten()
                .collect();
            if window.len() == 5 {
                let steps: Vec<f32> = window.windows(2).map(|w| w[1] - w[0]).collect();
                let monotonic = steps.iter().all(|&d| d > 0.002) || steps.iter().all(|&d| d < -0.002);
                if monotonic && (window[4] - window[0]).abs() > 0.03 {
                    return Ok(Verdict::Reject { reason: "fade", severity: 3 });
                }
            }
        }
        picked_hashes.push((r, cur.dhash));
        Ok(Verdict::Accept)
    })?;
    Ok(rep)
}
