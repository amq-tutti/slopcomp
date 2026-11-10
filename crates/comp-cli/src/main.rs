//! `comp`: anime-comp command-line interface.
//!
//! * `info` — stream properties
//! * `shot` — frame-exact lossless PNG screenshots of one file
//! * `run` — a comparison across several sources

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Instant, SystemTime};

use comp_core::comp::{self, CompSpec, Event, FramePick, SelectOptions, SourceSpec};
use comp_core::convert::{self, ConvertOptions};
use comp_core::filter::{FieldMode, FieldOptions, FieldOrder};
use comp_core::pairing;
use comp_core::viewer;
use comp_core::project::{FrameTypeRule, Project, ProjectSource, Rules};
use comp_core::source::{OpenOptions, VideoSource};

#[derive(Parser)]
#[command(name = "comp", version, about = "Frame-exact anime source comparisons")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print stream properties of one or more files.
    Info { files: Vec<PathBuf> },
    /// Write lossless PNG screenshots of the given frames.
    Shot {
        file: PathBuf,
        /// Frame numbers (0-based).
        #[arg(required = true, num_args = 1..)]
        frames: Vec<i32>,
        /// Output directory.
        #[arg(short, long, default_value = ".")]
        out: PathBuf,
        /// Keep storage resolution for anamorphic sources instead of stretching to display aspect.
        #[arg(long)]
        no_sar: bool,
    },
    /// Run a comparison from a saved TOML project file.
    Project { file: PathBuf },
    /// Show how episodes in several folders pair up (no decoding).
    Pair {
        /// LABEL=FOLDER (or file), repeatable.
        #[arg(short = 's', long = "source", required = true, value_parser = parse_kv)]
        sources: Vec<(String, String)>,
    },
    /// Run a comparison: same frames from every source, lossless PNGs plus comp.json.
    ///
    /// Example:
    ///   comp run -s "BD=/bd/ep01.mkv" -s "DVD=/dvd/ep01.mkv" --offset DVD=-12 --frames 20
    Run {
        /// Source as LABEL=PATH, where PATH is a file or a folder of episodes (paired by episode
        /// number). Repeat for each source; the first one is the reference timeline.
        #[arg(short = 's', long = "source", required = true, num_args = 1, value_parser = parse_kv)]
        sources: Vec<(String, String)>,
        /// Offset as LABEL=N (frames) or LABEL=Xs (seconds). Repeatable; frame and time offsets add up.
        /// Sources are aligned by timestamp first, so VFR and CFR sources line up; then the frame
        /// offset is applied ("DVD is 3 frames behind" → DVD=3).
        #[arg(long = "offset", value_parser = parse_kv)]
        offsets: Vec<(String, String)>,
        /// Number of random frames (ignored if --frame is given).
        #[arg(short = 'n', long, default_value_t = 20)]
        frames: usize,
        /// RNG seed for frame picking (random if omitted; printed so the run can be repeated).
        #[arg(long)]
        seed: Option<u64>,
        /// Explicit reference frame numbers instead of random picks.
        #[arg(long = "frame", num_args = 1..)]
        explicit: Vec<i32>,
        /// Inverse-telecine a source: LABEL, LABEL:tff or LABEL:bff (e.g. 1080i BD remux, NTSC DVD).
        /// Field matching + decimation to 23.976p; frames still combed after matching are deinterlaced.
        #[arg(long = "ivtc")]
        ivtc: Vec<String>,
        /// Deinterlace every frame of a source (truly interlaced content): LABEL[:tff|bff].
        #[arg(long = "deint")]
        deint: Vec<String>,
        /// Only pick frames whose picture type in LABEL is one of TYPES, e.g. --frame-type ENC=B.
        #[arg(long = "frame-type", value_parser = parse_kv)]
        frame_type: Option<(String, String)>,
        /// Leave out a reference-timeline range: START-END as seconds or [hh:]mm:ss. Repeatable.
        #[arg(long = "exclude")]
        exclude: Vec<String>,
        /// Leave out OP/ED, found from chapter names in any source (off by default: episodes are
        /// compared in full).
        #[arg(long)]
        exclude_op_ed: bool,
        /// Also skip near-duplicate shots (same static shot picked twice).
        #[arg(long)]
        skip_duplicates: bool,
        /// Also skip flat frames (solid colours, title cards).
        #[arg(long)]
        skip_flat: bool,
        /// Black-screen threshold: mean luma 0..1 (default 0.03; raise to also skip dark scenes).
        #[arg(long)]
        dark: Option<f32>,
        /// Turn off all rejection rules, including black screens and scene changes.
        #[arg(long)]
        no_reject: bool,
        /// Open the HTML viewer in your browser when done.
        #[arg(long)]
        open: bool,
        /// Open the output folder and slow.pics' upload page, for a manual drag-and-drop upload.
        #[arg(long)]
        open_slowpics: bool,
        /// Save these settings as a TOML project file (then run as usual).
        #[arg(long)]
        save_project: Option<PathBuf>,
        /// Matrix override as LABEL=709|601|470bg|2020. Repeatable.
        #[arg(long = "matrix", value_parser = parse_kv)]
        matrices: Vec<(String, String)>,
        /// Comparison name (used for the output folder).
        #[arg(long)]
        name: Option<String>,
        /// Base output directory.
        #[arg(short, long, default_value = "comps")]
        out: PathBuf,
        /// Keep storage resolution for anamorphic sources.
        #[arg(long)]
        no_sar: bool,
    },
}

fn parse_kv(s: &str) -> std::result::Result<(String, String), String> {
    s.split_once('=')
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .filter(|(k, v)| !k.is_empty() && !v.is_empty())
        .ok_or_else(|| format!("expected LABEL=VALUE, got '{s}'"))
}

/// "90-150", "1:30-2:30", "0:01:30.5-0:02:30" → seconds.
fn parse_time_range(s: &str) -> Result<(f64, f64)> {
    let secs = |t: &str| -> Result<f64> {
        let mut total = 0.0;
        for part in t.trim().split(':') {
            total = total * 60.0 + part.parse::<f64>().with_context(|| format!("bad time '{t}'"))?;
        }
        Ok(total)
    };
    let (a, b) = s.split_once('-').with_context(|| format!("expected START-END, got '{s}'"))?;
    Ok((secs(a)?, secs(b)?))
}

fn fmt_time(t: f64) -> String {
    format!("{}:{:05.2}", (t / 60.0).floor() as i64, t % 60.0)
}

fn parse_matrix(s: &str) -> Result<i32> {
    Ok(match s.to_ascii_lowercase().as_str() {
        "709" | "bt709" => 1,
        "601" | "170m" | "smpte170m" => 6,
        "470bg" | "bt470bg" | "pal" => 5,
        "2020" | "bt2020" | "2020ncl" => 9,
        "240m" => 7,
        "fcc" => 4,
        n => n.parse().with_context(|| format!("unknown matrix '{s}'"))?,
    })
}

fn index_cache() -> Option<PathBuf> {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .or_else(|| std::env::var_os("LOCALAPPDATA").map(PathBuf::from))
        .map(|d| d.join("anime-comp/index"))
}

fn open(path: &PathBuf) -> Result<VideoSource> {
    let opts = OpenOptions { index_cache: index_cache(), ..Default::default() };
    VideoSource::open(path, &opts).with_context(|| format!("opening {}", path.display()))
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Info { files } => {
            for f in &files {
                let src = open(f)?;
                let i = src.info();
                println!(
                    "{}\n  container {} (seek: {:?})\n  {}x{} {}-bit 4:{}:{} frames {} fps {}/{} sar {}:{}\n  color matrix={} transfer={} primaries={} range={} chroma_loc={}",
                    f.display(),
                    i.container,
                    i.seek_mode,
                    i.width,
                    i.height,
                    i.layout.bit_depth,
                    if i.layout.log2_chroma_w == 1 { 2 } else { 4 },
                    if i.layout.log2_chroma_h == 1 { 0 } else if i.layout.log2_chroma_w == 1 { 2 } else { 4 },
                    i.num_frames,
                    i.fps_num,
                    i.fps_den,
                    i.sar.0,
                    i.sar.1,
                    i.color.matrix,
                    i.color.transfer,
                    i.color.primaries,
                    i.color.range,
                    i.color.chroma_location
                );
            }
        }
        Cmd::Shot { file, mut frames, out, no_sar } => {
            std::fs::create_dir_all(&out)?;
            let mut src = open(&file)?;
            let sar = src.info().sar;
            let conv = ConvertOptions { apply_sar: !no_sar, ..Default::default() };
            let stem = file.file_stem().unwrap_or_default().to_string_lossy().into_owned();
            // Ascending order keeps linear-seek sources (DVD VOB/MPEG-PS) to one pass.
            frames.sort_unstable();
            frames.dedup();
            for n in frames {
                let frame = src.frame(n)?;
                let pict = frame.pict_type;
                let img = convert::to_rgb(&frame, sar, &conv)?;
                let path = out.join(format!("{stem}_{n:06}.png"));
                convert::write_png(&img, &path, Default::default())?;
                println!(
                    "{} ({}x{}, {pict}-frame, matrix {}{})",
                    path.display(),
                    img.width,
                    img.height,
                    img.color.matrix,
                    if img.color.matrix_guessed { " guessed" } else { "" }
                );
            }
        }
        Cmd::Project { file } => {
            let p = Project::load(&file)?;
            let mut select = SelectOptions { exclude_chapter_op_ed: p.exclude_op_ed, ..Default::default() };
            select.dark_threshold = p.rules.black_threshold.map(|t| t as f32);
            select.reject_transitions = p.rules.scene_changes;
            select.duplicate_distance = p.rules.duplicates.then_some(6);
            select.flat_threshold = p.rules.flat.then_some(0.02);
            select.frame_type = p.frame_type.as_ref().map(|r| (r.source.clone(), r.types.to_ascii_uppercase().chars().collect()));
            for r in &p.exclude {
                select.exclude.push(parse_time_range(r)?);
            }
            let sources = p.sources.iter().map(|s| (s.label.clone(), s.path.to_string_lossy().into_owned())).collect();
            let offsets = p
                .sources
                .iter()
                .flat_map(|s| {
                    s.offset.iter().flat_map(|o| o.split(',')).map(|o| (s.label.clone(), o.trim().to_string())).collect::<Vec<_>>()
                })
                .collect();
            let field = |pick: fn(&comp_core::project::ProjectSource) -> &Option<String>| -> Vec<String> {
                p.sources
                    .iter()
                    .filter_map(|s| pick(s).as_ref().map(|o| if o == "auto" { s.label.clone() } else { format!("{}:{o}", s.label) }))
                    .collect()
            };
            let ivtc = field(|s| &s.ivtc);
            let deint = field(|s| &s.deint);
            let matrices = p.sources.iter().filter_map(|s| s.matrix.clone().map(|m| (s.label.clone(), m))).collect();
            run_comp(sources, offsets, p.frames, p.seed, p.frame.clone(), ivtc, deint, matrices, p.name.clone(), p.out.clone(), p.no_sar, select, false, false)?;
        }
        Cmd::Pair { sources } => {
            let lists: Vec<Vec<PathBuf>> =
                sources.iter().map(|(_, p)| pairing::expand_input(Path::new(p))).collect::<std::io::Result<_>>()?;
            let pairs = pairing::pair(&lists);
            println!("{:<8} {}", "episode", sources.iter().map(|(l, _)| format!("{l:<40}")).collect::<String>());
            for row in &pairs.rows {
                let cells: String = row
                    .files
                    .iter()
                    .map(|f| {
                        let n = f.as_ref().map(|f| f.file_name().unwrap_or_default().to_string_lossy().into_owned());
                        let n = n.unwrap_or_else(|| "— missing —".into());
                        format!("{:<40}", n.chars().take(38).collect::<String>())
                    })
                    .collect();
                println!("{:<8} {cells}", row.episode.to_string());
            }
            for (si, files) in pairs.unparsed.iter().enumerate() {
                for f in files {
                    println!("unparsed ({}): {}", sources[si].0, f.display());
                }
            }
            println!("{} of {} episodes present in every source", pairs.complete().count(), pairs.rows.len());
        }
        Cmd::Run {
            sources,
            offsets,
            frames,
            seed,
            explicit,
            ivtc,
            deint,
            frame_type,
            exclude,
            exclude_op_ed,
            skip_duplicates,
            skip_flat,
            dark,
            no_reject,
            save_project,
            open,
            open_slowpics,
            matrices,
            name,
            out,
            no_sar,
        } => {
            let mut select = SelectOptions { exclude_chapter_op_ed: exclude_op_ed, ..Default::default() };
            if skip_duplicates {
                select.duplicate_distance = Some(6);
            }
            if skip_flat {
                select.flat_threshold = Some(0.02);
            }
            if let Some(d) = dark {
                select.dark_threshold = Some(d);
            }
            if no_reject {
                select.dark_threshold = None;
                select.flat_threshold = None;
                select.duplicate_distance = None;
                select.reject_transitions = false;
            }
            if let Some((label, types)) = frame_type {
                select.frame_type = Some((label, types.to_ascii_uppercase().chars().collect()));
            }
            for r in &exclude {
                select.exclude.push(parse_time_range(r)?);
            }
            if let Some(file) = &save_project {
                let project = Project {
                    name: name.clone(),
                    out: out.clone(),
                    frames,
                    seed,
                    frame: explicit.clone(),
                    exclude_op_ed,
                    exclude: exclude.clone(),
                    frame_type: select.frame_type.as_ref().map(|(l, t)| FrameTypeRule { source: l.clone(), types: t.iter().collect() }),
                    rules: Rules {
                        black_threshold: select.dark_threshold.map(|t| (t as f64 * 1000.0).round() / 1000.0),
                        scene_changes: select.reject_transitions,
                        duplicates: select.duplicate_distance.is_some(),
                        flat: select.flat_threshold.is_some(),
                    },
                    no_sar,
                    sources: sources
                        .iter()
                        .map(|(l, p)| {
                            let field = |list: &Vec<String>| {
                                list.iter().find_map(|i| {
                                    let (lab, ord) = i.rsplit_once(':').unwrap_or((i, "auto"));
                                    (lab == l).then(|| ord.to_string())
                                })
                            };
                            let offs: Vec<&str> = offsets.iter().filter(|(ol, _)| ol == l).map(|(_, v)| v.as_str()).collect();
                            ProjectSource {
                                label: l.clone(),
                                path: std::fs::canonicalize(p).unwrap_or_else(|_| PathBuf::from(p)),
                                offset: (!offs.is_empty()).then(|| offs.join(",")),
                                ivtc: field(&ivtc),
                                deint: field(&deint),
                                matrix: matrices.iter().find(|(ml, _)| ml == l).map(|(_, v)| v.clone()),
                            }
                        })
                        .collect(),
                };
                project.save(file)?;
                eprintln!("saved project to {}", file.display());
            }
            run_comp(sources, offsets, frames, seed, explicit, ivtc, deint, matrices, name, out, no_sar, select, open, open_slowpics)?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_comp(
    sources: Vec<(String, String)>,
    offsets: Vec<(String, String)>,
    frames: usize,
    seed: Option<u64>,
    explicit: Vec<i32>,
    ivtc: Vec<String>,
    deint: Vec<String>,
    matrices: Vec<(String, String)>,
    name: Option<String>,
    out: PathBuf,
    no_sar: bool,
    select: SelectOptions,
    open: bool,
    open_slowpics: bool,
) -> Result<()> {
    let mut specs: Vec<SourceSpec> = sources.iter().map(|(l, p)| SourceSpec::new(l, p)).collect();
    let find = |specs: &mut Vec<SourceSpec>, label: &str| -> Result<usize> {
        specs
            .iter()
            .position(|s| s.label == label)
            .with_context(|| format!("no source labelled '{label}'"))
    };
    for (label, v) in &offsets {
        let i = find(&mut specs, label)?;
        if let Some(secs) = v.strip_suffix('s') {
            specs[i].offset_seconds += secs.parse::<f64>().with_context(|| format!("bad time offset '{v}'"))?;
        } else {
            specs[i].offset_frames += v.parse::<i32>().with_context(|| format!("bad frame offset '{v}'"))?;
        }
    }
    for (list, mode) in [(&ivtc, FieldMode::Ivtc), (&deint, FieldMode::Deinterlace)] {
        for item in list {
            let (label, order) = match item.rsplit_once(':') {
                Some((l, "tff")) => (l, FieldOrder::Tff),
                Some((l, "bff")) => (l, FieldOrder::Bff),
                _ => (item.as_str(), FieldOrder::Auto),
            };
            let i = find(&mut specs, label)?;
            specs[i].field = Some(FieldOptions { mode, order });
        }
    }
    for (label, v) in &matrices {
        let i = find(&mut specs, label)?;
        specs[i].matrix_override = Some(parse_matrix(v)?);
    }

    let pick = if explicit.is_empty() {
        let seed = seed.unwrap_or_else(|| {
            SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_nanos() as u64 % 1_000_000).unwrap_or(1)
        });
        FramePick::Random { count: frames, seed }
    } else {
        FramePick::Explicit(explicit)
    };
    // Sources may be folders: expand to files and pair episodes by number.
    let lists: Vec<Vec<PathBuf>> = sources
        .iter()
        .map(|(l, p)| {
            let v = pairing::expand_input(Path::new(p)).with_context(|| format!("reading {p}"))?;
            anyhow::ensure!(!v.is_empty(), "no video files in '{p}' ({l})");
            Ok(v)
        })
        .collect::<Result<_>>()?;
    let pairs = pairing::pair(&lists);
    let multi = lists.iter().any(|l| l.len() > 1);
    let vs = specs.iter().map(|s| s.label.as_str()).collect::<Vec<_>>().join(" vs ");
    let name = name.unwrap_or_else(|| {
        let p = Path::new(&sources[0].1);
        let base = if multi {
            p.file_name()
        } else {
            p.file_stem()
        };
        format!("{} - {vs}", base.unwrap_or_default().to_string_lossy())
    });
    let mut spec = CompSpec {
        out_dir: comp::default_out_dir(&out, &name),
        name,
        sources: specs,
        pick,
        convert: ConvertOptions { apply_sar: !no_sar, ..Default::default() },
        png: Default::default(),
        open: OpenOptions { index_cache: index_cache(), ..Default::default() },
        select,
    };

    if !multi {
        for (i, s) in spec.sources.iter_mut().enumerate() {
            s.path = lists[i][0].clone();
        }
        let res = run_one(&spec)?;
        after_run(&res.out_dir, open, open_slowpics);
        return Ok(());
    }

    // Season mode: print the pairing table, then run every complete episode.
    println!("{:<8} {}", "episode", spec.sources.iter().map(|s| format!("{:<10}", s.label)).collect::<String>());
    for row in &pairs.rows {
        let cells: String = row.files.iter().map(|f| format!("{:<10}", if f.is_some() { "✓" } else { "missing" })).collect();
        println!("{:<8} {cells}", row.episode.to_string());
    }
    for (si, files) in pairs.unparsed.iter().enumerate() {
        for f in files {
            println!("warning: no episode number in {} ({})", f.display(), spec.sources[si].label);
        }
    }
    for (si, dropped, kept) in &pairs.duplicates {
        println!("warning: {} has two files for one episode; using {} over {}", spec.sources[*si].label, kept.display(), dropped.display());
    }
    let rows: Vec<_> = pairs.complete().cloned().collect();
    anyhow::ensure!(!rows.is_empty(), "no episode is present in every source");
    let base_dir = spec.out_dir.clone();
    let base_name = spec.name.clone();
    let mut failed = Vec::new();
    let mut entries = Vec::new();
    for row in rows {
        let ep = row.episode.to_string();
        println!("\n=== {ep} ===");
        for (i, s) in spec.sources.iter_mut().enumerate() {
            s.path = row.files[i].clone().unwrap();
        }
        spec.name = format!("{base_name} - {ep}");
        spec.out_dir = base_dir.join(&ep);
        match run_one(&spec) {
            Ok(res) => entries.push(viewer::SeasonEntry {
                thumb: res.sources[0].shots.first().map(|s| format!("{ep}/{}", s.file)),
                dir: ep.clone(),
                frames: res.frames.len(),
                episode: ep,
            }),
            Err(e) => {
                eprintln!("error in {ep}: {e:#}");
                failed.push(ep);
            }
        }
    }
    if !entries.is_empty() {
        let idx = viewer::write_season_index(&base_dir, &base_name, &entries)?;
        println!("\nseason index: {}", idx.display());
        if open {
            open_external(&idx.to_string_lossy());
        }
        if open_slowpics {
            after_run(&base_dir, false, true);
        }
    }
    anyhow::ensure!(failed.is_empty(), "failed episodes: {}", failed.join(", "));
    Ok(())
}

fn run_one(spec: &CompSpec) -> Result<comp::CompResult> {
    let labels: Vec<String> = spec.sources.iter().map(|s| s.label.clone()).collect();
    let total_shots = Mutex::new((0usize, 0usize));
    let last_pct: Mutex<HashMap<usize, i64>> = Mutex::new(HashMap::new());
    let started = Instant::now();
    let on_event = |e: Event| match e {
        Event::Indexing { source, done, total } if total > 0 => {
            let pct = done * 100 / total;
            let mut lp = last_pct.lock().unwrap();
            if lp.get(&source).is_none_or(|&p| pct >= p + 10) {
                lp.insert(source, pct);
                eprintln!("  indexing {:<12} {pct:>3}%", labels[source]);
            }
        }
        Event::Analyzing { source, done, total } if total > 0 => {
            let pct = done * 100 / total;
            let mut lp = last_pct.lock().unwrap();
            let key = source + 1000;
            if lp.get(&key).is_none_or(|&p| pct >= p + 10) {
                lp.insert(key, pct);
                eprintln!("  field analysis {:<12} {pct:>3}%", labels[source]);
            }
        }
        Event::Opened { source, info } => eprintln!(
            "  opened   {:<12} {}x{} {}-bit, {} frames @ {}/{}, sar {}:{}, {} ({:?} seek)",
            labels[source],
            info.width,
            info.height,
            info.layout.bit_depth,
            info.num_frames,
            info.fps_num,
            info.fps_den,
            info.sar.0,
            info.sar.1,
            info.container,
            info.seek_mode
        ),
        Event::Excluded { ranges } => {
            for (a, b, why) in ranges {
                eprintln!("  excluding {}–{} ({why})", fmt_time(a), fmt_time(b));
            }
        }
        Event::FramesPicked { frames, usable } => {
            *total_shots.lock().unwrap() = (0, frames.len() * labels.len());
            eprintln!("  picked {} frames in [{}, {}): {:?}", frames.len(), usable.0, usable.1, frames);
        }
        Event::Shot { .. } => {
            let mut t = total_shots.lock().unwrap();
            t.0 += 1;
            if t.0 == t.1 || t.0 % 10 == 0 {
                eprintln!("  shots {}/{}", t.0, t.1);
            }
        }
        _ => {}
    };

    let res = comp::run(spec, &on_event)?;
    println!("\n{} -> {}", res.name, res.out_dir.display());
    if let Some(seed) = res.seed {
        println!("seed {seed}");
    }
    if !res.rejected.is_empty() {
        let mut counts: std::collections::BTreeMap<&str, usize> = Default::default();
        for (_, r) in &res.rejected {
            *counts.entry(r.as_str()).or_default() += 1;
        }
        println!("rejected candidates: {counts:?}");
    }
    for (f, why) in &res.fallbacks {
        println!("note: frame {f} kept despite '{why}' (nothing better in its bin)");
    }
    for s in &res.sources {
        let types: String = s.shots.iter().map(|x| x.pict_type.as_str()).collect();
        println!(
            "  {:<12} offset {:+.3}s{:+}f{}{} matrix {}{} {} -> {}x{} types [{types}]",
            s.label,
            s.offset_seconds,
            s.offset_frames,
            if s.vfr { " VFR" } else { "" },
            s.field_processing.as_deref().map(|f| format!(" {f}")).unwrap_or_default(),
            s.matrix,
            if s.matrix_guessed { " (guessed)" } else { "" },
            if s.full_range { "full" } else { "limited" },
            s.shots.first().map(|x| x.width).unwrap_or(0),
            s.shots.first().map(|x| x.height).unwrap_or(0)
        );
    }
    println!("viewer: {}", res.out_dir.join("index.html").display());
    println!("done in {:.1}s", started.elapsed().as_secs_f64());
    Ok(res)
}

/// Opens a file, folder or URL with the OS default handler.
fn open_external(target: &str) {
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(target).status();
    #[cfg(target_os = "windows")]
    let r = std::process::Command::new("cmd").args(["/C", "start", "", target]).status();
    #[cfg(all(unix, not(target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(target).status();
    if let Err(e) = r {
        eprintln!("couldn't open {target}: {e}");
    }
}

/// After a run: open the viewer, and/or the folder plus slow.pics' upload page for a manual upload.
fn after_run(dir: &Path, open: bool, open_slowpics: bool) {
    if open {
        open_external(&dir.join("index.html").to_string_lossy());
    }
    if open_slowpics {
        open_external(&dir.to_string_lossy());
        open_external("https://slow.pics/comparison");
        println!(
            "Drag the PNGs from {} into the slow.pics page. Names sort frame by frame, sources in -s order.",
            dir.display()
        );
    }
}
