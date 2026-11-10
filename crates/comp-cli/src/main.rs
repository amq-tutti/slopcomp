//! `comp`: anime-comp command-line interface.
//!
//! * `info` — stream properties
//! * `shot` — frame-exact lossless PNG screenshots of one file
//! * `run` — a comparison across several sources

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Instant, SystemTime};

use comp_core::comp::{self, CompSpec, Event, FramePick, SourceSpec};
use comp_core::convert::{self, ConvertOptions};
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
    /// Run a comparison: same frames from every source, lossless PNGs plus comp.json.
    ///
    /// Example:
    ///   comp run -s "BD=/bd/ep01.mkv" -s "DVD=/dvd/ep01.mkv" --offset DVD=-12 --frames 20
    Run {
        /// Source as LABEL=PATH. Repeat for each source; the first one is the reference timeline.
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
        Cmd::Run { sources, offsets, frames, seed, explicit, matrices, name, out, no_sar } => {
            run_comp(sources, offsets, frames, seed, explicit, matrices, name, out, no_sar)?;
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
    matrices: Vec<(String, String)>,
    name: Option<String>,
    out: PathBuf,
    no_sar: bool,
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
    let name = name.unwrap_or_else(|| {
        let stem = PathBuf::from(&sources[0].1).file_stem().unwrap_or_default().to_string_lossy().into_owned();
        format!("{stem} - {}", specs.iter().map(|s| s.label.as_str()).collect::<Vec<_>>().join(" vs "))
    });
    let spec = CompSpec {
        out_dir: comp::default_out_dir(&out, &name),
        name,
        sources: specs,
        pick,
        convert: ConvertOptions { apply_sar: !no_sar, ..Default::default() },
        png: Default::default(),
        open: OpenOptions { index_cache: index_cache(), ..Default::default() },
    };

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

    let res = comp::run(&spec, &on_event)?;
    println!("\n{} -> {}", res.name, res.out_dir.display());
    if let Some(seed) = res.seed {
        println!("seed {seed}");
    }
    for s in &res.sources {
        let types: String = s.shots.iter().map(|x| x.pict_type.as_str()).collect();
        println!(
            "  {:<12} offset {:+.3}s{:+}f{} matrix {}{} {} -> {}x{} types [{types}]",
            s.label,
            s.offset_seconds,
            s.offset_frames,
            if s.vfr { " VFR" } else { "" },
            s.matrix,
            if s.matrix_guessed { " (guessed)" } else { "" },
            if s.full_range { "full" } else { "limited" },
            s.shots.first().map(|x| x.width).unwrap_or(0),
            s.shots.first().map(|x| x.height).unwrap_or(0)
        );
    }
    println!("done in {:.1}s", started.elapsed().as_secs_f64());
    Ok(())
}
