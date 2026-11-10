//! `comp`: anime-comp command-line interface.
//!
//! M0 scope: `info` (stream properties) and `shot` (frame-exact lossless PNG screenshots).

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
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
    }
    Ok(())
}
