//! M0 correctness test: seeking returns *exactly* the requested frame.
//!
//! For every clip in the corpus (`testdata/make_corpus.sh`, or `$COMP_CORPUS`):
//! 1. A reference per-frame MD5 list comes from the system `ffmpeg -f framemd5`, an independent
//!    build and decoder.
//! 2. Our ffms2 source is read sequentially, and every frame must match the reference.
//! 3. A fresh source is read in seeded random order, and every frame must still match.
//!
//! If the corpus or system ffmpeg is missing, the test is skipped with a message.

use std::path::{Path, PathBuf};
use std::process::Command;

use comp_core::convert::{self, ConvertOptions};
use comp_core::source::{Frame, OpenOptions, VideoSource};
use md5::{Digest, Md5};
use rand::seq::SliceRandom;
use rand::SeedableRng;

fn corpus_dir() -> PathBuf {
    std::env::var_os("COMP_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/corpus"))
}

fn corpus_files() -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(corpus_dir()) else { return vec![] };
    let mut v: Vec<_> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && !p.extension().is_some_and(|e| e == "ffindex"))
        .collect();
    v.sort();
    v
}

/// Per-frame MD5s from the system ffmpeg, in presentation order.
fn reference_md5s(path: &Path) -> Option<Vec<String>> {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(path)
        .args(["-map", "0:v:0", "-fps_mode", "passthrough", "-f", "framemd5", "-"])
        .output()
        .ok()?;
    if !out.status.success() {
        panic!("ffmpeg framemd5 failed for {}: {}", path.display(), String::from_utf8_lossy(&out.stderr));
    }
    Some(
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .filter(|l| !l.starts_with('#'))
            .map(|l| l.rsplit(',').next().unwrap().trim().to_string())
            .collect(),
    )
}

/// MD5 of the frame packed like ffmpeg's rawvideo output: planes concatenated, rows unpadded.
fn frame_md5(f: &Frame<'_>) -> String {
    let mut h = Md5::new();
    for p in 0..f.layout.planes {
        let (_, ph) = f.layout.plane_dims(p, f.width as usize, f.height as usize);
        for y in 0..ph {
            h.update(f.row(p, y));
        }
    }
    format!("{:x}", h.finalize())
}

#[test]
fn seeking_is_frame_exact() {
    let files = corpus_files();
    if files.is_empty() {
        eprintln!("SKIP: no corpus at {} (run testdata/make_corpus.sh)", corpus_dir().display());
        return;
    }
    let cache = std::env::temp_dir().join("anime-comp-test-index");
    let opts = OpenOptions { index_cache: Some(cache), ..Default::default() };
    let mut failures = Vec::new();

    for path in &files {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let Some(reference) = reference_md5s(path) else {
            eprintln!("SKIP: system ffmpeg not available");
            return;
        };

        let mut src = VideoSource::open(path, &opts).unwrap_or_else(|e| panic!("{name}: open failed: {e}"));
        let info = src.info().clone();
        let n = info.num_frames as usize;
        if n != reference.len() {
            failures.push(format!("{name}: frame count {n} != reference {}", reference.len()));
            continue;
        }

        // Sequential pass.
        let mut seq_bad = Vec::new();
        for i in 0..n {
            let f = src.frame(i as i32).unwrap();
            if frame_md5(&f) != reference[i] {
                seq_bad.push(i);
            }
        }

        // Random-access pass on a fresh source (seeded, all frames in shuffled order).
        let mut src = VideoSource::open(path, &opts).unwrap();
        let mut order: Vec<usize> = (0..n).collect();
        order.shuffle(&mut rand_chacha::ChaCha8Rng::seed_from_u64(0xC0FFEE));
        let mut rnd_bad = Vec::new();
        for &i in &order {
            let f = src.frame(i as i32).unwrap();
            if frame_md5(&f) != reference[i] {
                rnd_bad.push(i);
            }
        }

        let types: String = {
            let mut s = VideoSource::open(path, &opts).unwrap();
            (0..n.min(24)).map(|i| s.pict_type(i as i32).unwrap()).collect()
        };
        eprintln!(
            "{name:22} [{}/{:?}] {n:4} frames {}x{} {:>3}-bit sar {}:{} types[{types}] seq_bad={} rnd_bad={}",
            info.container,
            info.seek_mode,
            info.width,
            info.height,
            info.layout.bit_depth,
            info.sar.0,
            info.sar.1,
            seq_bad.len(),
            rnd_bad.len()
        );
        if !seq_bad.is_empty() {
            failures.push(format!("{name}: sequential mismatches at {:?}", &seq_bad[..seq_bad.len().min(10)]));
        }
        if !rnd_bad.is_empty() {
            failures.push(format!("{name}: random-access mismatches at {:?}", &rnd_bad[..rnd_bad.len().min(10)]));
        }
    }
    assert!(failures.is_empty(), "frame accuracy failures:\n{}", failures.join("\n"));
}

#[test]
fn converts_every_corpus_clip_to_png() {
    let files = corpus_files();
    if files.is_empty() {
        eprintln!("SKIP: no corpus");
        return;
    }
    let out_dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("png");
    std::fs::create_dir_all(&out_dir).unwrap();
    let opts = OpenOptions::default();
    let conv = ConvertOptions { apply_sar: true, ..Default::default() };
    for path in &files {
        let mut src = VideoSource::open(path, &opts).unwrap();
        let info = src.info().clone();
        let n = info.num_frames / 2;
        let frame = src.frame(n).unwrap();
        let img = convert::to_rgb(&frame, info.sar, &conv).unwrap();
        let stem = path.file_name().unwrap().to_string_lossy().replace('.', "_");
        let png = out_dir.join(format!("{stem}_{n}.png"));
        convert::write_png(&img, &png, Default::default()).unwrap();
        eprintln!(
            "{stem:20} -> {}x{} matrix={}{} full_range={} {}",
            img.width,
            img.height,
            img.color.matrix,
            if img.color.matrix_guessed { "(guessed)" } else { "" },
            img.color.full_range,
            png.display()
        );
        // DVD clip is 720x480 at 16:9 → SAR 32:27 → stretched to 853 wide.
        if stem == "dvd_mpeg2_mpg" {
            assert_eq!((img.width, img.height), (853, 480), "anamorphic stretch");
        }
    }
}
