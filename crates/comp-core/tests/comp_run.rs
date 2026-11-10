//! End-to-end: `comp::run` with offsets lines up the same content across different encodes.
//!
//! `h264_opengop.mkv` frame r shows the same picture as `late24_hevc.mkv` frame r-24. With the
//! right offset (in frames or seconds) the PNG pairs must be near-identical. With no offset they
//! must clearly differ (testsrc2 moves every frame).

use std::path::{Path, PathBuf};

use comp_core::comp::{self, CompSpec, FramePick, SourceSpec};
use comp_core::filter::{FieldMode, FieldOptions, FieldOrder};
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
        select: Default::default(),
        cancel: None,
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
        select: Default::default(),
        cancel: None,
    };
    assert!(comp::run(&spec(vec![r.clone()]), &|_| {}).is_err());
    r.offset_frames = 3;
    let other = SourceSpec::new("B", corpus("hevc_10bit.mkv"));
    let err = comp::run(&spec(vec![r, other]), &|_| {}).unwrap_err().to_string();
    assert!(err.contains("reference"), "{err}");
}

#[test]
fn ivtc_source_aligns_with_progressive_truth() {
    if !corpus("ivtc_telecined_dvd.mpg").exists() {
        eprintln!("SKIP: corpus missing");
        return;
    }
    let ivtc = Some(FieldOptions { mode: FieldMode::Ivtc, order: FieldOrder::Auto });
    let mut dvd = SourceSpec::new("DVD", corpus("ivtc_telecined_dvd.mpg"));
    dvd.field = ivtc;
    let mut bd = SourceSpec::new("BD", corpus("ivtc_telecined_h264.m2ts"));
    bd.field = ivtc;
    let spec = CompSpec {
        name: "ivtc_e2e".into(),
        sources: vec![SourceSpec::new("TRUTH", corpus("ivtc_truth_ffv1.mkv")), dvd, bd],
        pick: FramePick::Random { count: 12, seed: 11 },
        out_dir: Path::new(env!("CARGO_TARGET_TMPDIR")).join("ivtc_e2e"),
        convert: Default::default(),
        png: Default::default(),
        open: OpenOptions::default(),
        select: Default::default(),
        cancel: None,
    };
    let res = comp::run(&spec, &|_| {}).unwrap();
    assert_eq!(res.frames.len(), 12);
    for s in &res.sources[1..] {
        assert_eq!(s.field_processing.as_deref(), Some("ivtc-tff"));
        let mut worst = f64::INFINITY;
        for (t, x) in res.sources[0].shots.iter().zip(&s.shots) {
            assert_eq!(t.reference_frame, x.source_frame, "{}: IVTC output index should equal truth index", s.label);
            let p = psnr(&load_rgb(&res.out_dir.join(&t.file)), &load_rgb(&res.out_dir.join(&x.file)));
            worst = worst.min(p);
        }
        eprintln!("{}: worst RGB PSNR vs truth {worst:.1} dB", s.label);
        assert!(worst > 35.0, "{}: {worst:.1}", s.label);
    }
}

#[test]
fn ivtc_refuses_progressive_film_rate_sources() {
    if !corpus("ivtc_truth_ffv1.mkv").exists() {
        return;
    }
    let mut a = SourceSpec::new("A", corpus("h264_opengop.mkv"));
    a.field = Some(FieldOptions { mode: FieldMode::Ivtc, order: FieldOrder::Auto });
    let spec = CompSpec {
        name: "ivtc_refuse".into(),
        sources: vec![SourceSpec::new("REF", corpus("ivtc_truth_ffv1.mkv")), a],
        pick: FramePick::Random { count: 2, seed: 1 },
        out_dir: Path::new(env!("CARGO_TARGET_TMPDIR")).join("ivtc_refuse"),
        convert: Default::default(),
        png: Default::default(),
        open: OpenOptions::default(),
        select: Default::default(),
        cancel: None,
    };
    let err = comp::run(&spec, &|_| {}).unwrap_err().to_string();
    assert!(err.contains("already 23.976"), "{err}");
}

fn spec_with(name: &str, sources: Vec<SourceSpec>, count: usize, select: comp_core::comp::SelectOptions) -> CompSpec {
    CompSpec {
        name: name.into(),
        sources,
        pick: FramePick::Random { count, seed: 21 },
        out_dir: Path::new(env!("CARGO_TARGET_TMPDIR")).join(name),
        convert: Default::default(),
        png: Default::default(),
        open: OpenOptions::default(),
        select,
        cancel: None,
    }
}

#[test]
fn picking_skips_black_screens_and_scene_cuts() {
    if !corpus("cuts_black.mkv").exists() {
        eprintln!("SKIP: corpus missing");
        return;
    }
    let src = || SourceSpec::new("A", corpus("cuts_black.mkv"));
    let mut b = src();
    b.label = "B".into();
    // 30 bins over 240 frames: several bins fall entirely inside the black section. Black
    // frames are never used; those bins are refilled elsewhere, so we still get 30 frames.
    let res = comp::run(&spec_with("rules_on", vec![src(), b], 30, Default::default()), &|_| {}).unwrap();
    let cut_adjacent = |f: i32| [95, 96, 143, 144].contains(&f);
    let black = |f: i32| (96..144).contains(&f);
    assert_eq!(res.frames.len(), 30);
    assert!(res.fallbacks.is_empty(), "{:?}", res.fallbacks);
    for &f in &res.frames {
        assert!(!black(f), "picked black frame {f}");
        assert!(!cut_adjacent(f), "picked frame {f} next to a scene cut");
    }
    let reasons: Vec<&str> = res.rejected.iter().map(|(_, r)| r.as_str()).collect();
    assert!(reasons.contains(&"dark"), "black frames were considered and rejected: {reasons:?}");
    eprintln!("picked {:?}\nrejected {:?}\nfallbacks {:?}", res.frames, res.rejected, res.fallbacks);

    // With the rules off, the black section is fair game.
    let off = comp_core::comp::SelectOptions {
        dark_threshold: None,
        reject_transitions: false,
        ..Default::default()
    };
    let mut b = src();
    b.label = "B".into();
    let res = comp::run(&spec_with("rules_off", vec![src(), b], 30, off), &|_| {}).unwrap();
    assert!(res.frames.iter().any(|&f| black(f)), "without rules some picks land in the black section");
    assert!(res.rejected.is_empty());
}

#[test]
fn frame_type_rule_picks_only_requested_types() {
    let enc = || SourceSpec::new("ENC", corpus("h264_opengop.mkv"));
    let select = comp_core::comp::SelectOptions { frame_type: Some(("ENC".into(), vec!['B'])), ..Default::default() };
    let res = comp::run(&spec_with("ftype", vec![SourceSpec::new("REF", corpus("hevc_10bit.mkv")), enc()], 10, select), &|_| {}).unwrap();
    let types: String = res.sources[1].shots.iter().map(|s| s.pict_type.as_str()).collect();
    eprintln!("ENC types: {types}");
    assert!(res.fallbacks.is_empty(), "{:?}", res.fallbacks);
    assert!(types.chars().all(|c| c == 'B'), "{types}");
}
