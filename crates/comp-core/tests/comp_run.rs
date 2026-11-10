//! End-to-end: `comp::run` with offsets lines up the same content across different encodes.
//!
//! `h264_opengop.mkv` frame r shows the same picture as `late24_hevc.mkv` frame r-24. With the
//! right offset (in frames or seconds) the PNG pairs must be near-identical. With no offset they
//! must clearly differ (testsrc2 moves every frame).

use std::path::{Path, PathBuf};

use comp_core::comp::{self, CompSpec, FramePick, SourceSpec};
use comp_core::source::OpenOptions;

fn corpus(name: &str) -> PathBuf {
    std::env::var_os("COMP_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/corpus"))
        .join(name)
}

fn load_rgb(path: &Path) -> Vec<u8> {
    let dec = png::Decoder::new(std::fs::File::open(path).unwrap());
    let mut r = dec.read_info().unwrap();
    let mut buf = vec![0; r.output_buffer_size()];
    let info = r.next_frame(&mut buf).unwrap();
    buf.truncate(info.buffer_size());
    buf
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    let mse = a.iter().zip(b).map(|(&x, &y)| (x as f64 - y as f64).powi(2)).sum::<f64>() / a.len() as f64;
    if mse == 0.0 { f64::INFINITY } else { 10.0 * (255.0f64.powi(2) / mse).log10() }
}

/// Runs a 6-frame comparison and returns the mean PSNR between the two sources' PNGs.
fn mean_psnr(tag: &str, offset_frames: i32, offset_seconds: f64) -> f64 {
    let mut late = SourceSpec::new("LATE", corpus("late24_hevc.mkv"));
    late.offset_frames = offset_frames;
    late.offset_seconds = offset_seconds;
    let spec = CompSpec {
        name: tag.into(),
        sources: vec![SourceSpec::new("REF", corpus("h264_opengop.mkv")), late],
        pick: FramePick::Random { count: 6, seed: 7 },
        out_dir: Path::new(env!("CARGO_TARGET_TMPDIR")).join(tag),
        convert: Default::default(),
        png: Default::default(),
        open: OpenOptions::default(),
    };
    let res = comp::run(&spec, &|_| {}).unwrap();
    assert_eq!(res.frames.len(), 6);
    let (a, b) = (&res.sources[0].shots, &res.sources[1].shots);
    let total: f64 = a
        .iter()
        .zip(b)
        .map(|(x, y)| psnr(&load_rgb(&res.out_dir.join(&x.file)), &load_rgb(&res.out_dir.join(&y.file))))
        .sum();
    total / a.len() as f64
}

#[test]
fn offsets_align_content() {
    if !corpus("late24_hevc.mkv").exists() {
        eprintln!("SKIP: corpus missing (run testdata/make_corpus.sh)");
        return;
    }
    let aligned_frames = mean_psnr("offset_frames", -24, 0.0);
    let aligned_secs = mean_psnr("offset_seconds", 0, -1.001);
    let misaligned = mean_psnr("offset_none", 0, 0.0);
    eprintln!("PSNR aligned(frames)={aligned_frames:.1} aligned(seconds)={aligned_secs:.1} misaligned={misaligned:.1}");
    assert!(aligned_frames > 30.0, "frame offset should align content");
    assert!(aligned_secs > 30.0, "time offset should align content");
    assert!(misaligned < aligned_frames - 10.0, "no offset should be clearly misaligned");
}

#[test]
fn rejects_offset_on_reference_and_single_source() {
    let mut r = SourceSpec::new("REF", corpus("h264_opengop.mkv"));
    let spec = |sources| CompSpec {
        name: "x".into(),
        sources,
        pick: FramePick::Random { count: 1, seed: 1 },
        out_dir: Path::new(env!("CARGO_TARGET_TMPDIR")).join("reject"),
        convert: Default::default(),
        png: Default::default(),
        open: OpenOptions::default(),
    };
    assert!(comp::run(&spec(vec![r.clone()]), &|_| {}).is_err());
    r.offset_frames = 3;
    let other = SourceSpec::new("B", corpus("hevc_10bit.mkv"));
    let err = comp::run(&spec(vec![r, other]), &|_| {}).unwrap_err().to_string();
    assert!(err.contains("reference"), "{err}");
}
