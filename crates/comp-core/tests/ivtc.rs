//! IVTC / deinterlacing against exact ground truth.
//!
//! `ivtc_truth_ffv1.mkv` holds the original progressive 23.976 frames (lossless).
//! `ivtc_telecined_*` hold the same frames 3:2-telecined to 29.97i: lossless FFV1, lossy
//! interlaced H.264 in M2TS (BD-like) and lossy interlaced MPEG-2 in a program stream (DVD-like).
//!
//! * Lossless: IVTC must reproduce every original frame bit-exactly.
//! * Lossy: IVTC must return the right original frame for every output (best Y-PSNR match).
//! * Random access in shuffled order must succeed for every frame. `FilteredSource::frame`
//!   verifies each result against the analysis-pass hash and errors if it can't reproduce it.

use std::path::{Path, PathBuf};

use comp_core::filter::{frame_hash, FieldMode, FieldOptions, FieldOrder, FilteredSource};
use comp_core::source::{Frame, OpenOptions, VideoSource};
use rand::seq::SliceRandom;
use rand::SeedableRng;

fn corpus(name: &str) -> PathBuf {
    std::env::var_os("COMP_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/corpus"))
        .join(name)
}

fn have_corpus() -> bool {
    let ok = corpus("ivtc_truth_ffv1.mkv").exists();
    if !ok {
        eprintln!("SKIP: IVTC corpus missing (run testdata/make_corpus.sh)");
    }
    ok
}

fn ivtc(name: &str) -> FilteredSource {
    let vs = VideoSource::open(&corpus(name), &OpenOptions::default()).unwrap();
    FilteredSource::new(vs, FieldOptions { mode: FieldMode::Ivtc, order: FieldOrder::Auto }, None, |_, _| true).unwrap()
}

fn luma(f: &Frame<'_>) -> Vec<u8> {
    (0..f.height as usize).flat_map(|y| f.row(0, y).to_vec()).collect()
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let mse = a.iter().zip(b).map(|(&x, &y)| (x as f64 - y as f64).powi(2)).sum::<f64>() / a.len() as f64;
    if mse == 0.0 { f64::INFINITY } else { 10.0 * (255.0f64.powi(2) / mse).log10() }
}

#[test]
fn lossless_ivtc_is_bit_exact() {
    if !have_corpus() {
        return;
    }
    let mut truth = VideoSource::open(&corpus("ivtc_truth_ffv1.mkv"), &OpenOptions::default()).unwrap();
    let truth_hashes: Vec<u64> = (0..truth.info().num_frames).map(|i| frame_hash(&truth.frame(i).unwrap())).collect();

    let mut src = ivtc("ivtc_telecined_ffv1.mkv");
    let n = src.info().num_frames;
    eprintln!("IVTC: {} -> {n} frames @ {}/{}", src.inner().info().num_frames, src.info().fps_num, src.info().fps_den);
    assert_eq!(n as usize, truth_hashes.len(), "output frame count");
    assert_eq!((src.info().fps_num, src.info().fps_den), (24000, 1001));

    // Sequential analysis result vs truth.
    let bad: Vec<usize> = (0..n as usize).filter(|&k| src.map().hash[k] != truth_hashes[k]).collect();
    assert!(bad.is_empty(), "frames differing from truth after IVTC: {bad:?}");

    // Timestamps: CFR 23.976 from 0.
    let t = src.times();
    for (k, &tk) in t.iter().enumerate() {
        assert!((tk - k as f64 * 1001.0 / 24000.0).abs() < 0.002, "time of output {k}: {tk}");
    }

    // Random access in shuffled order.
    let mut order: Vec<i32> = (0..n).collect();
    order.shuffle(&mut rand_chacha::ChaCha8Rng::seed_from_u64(99));
    for &k in &order {
        let h = frame_hash(&src.frame(k).unwrap());
        assert_eq!(h, truth_hashes[k as usize], "random access frame {k}");
    }
    eprintln!("random access: {} window retries over {n} requests", src.window_retries);
}

#[test]
fn lossy_ivtc_returns_the_right_frames() {
    if !have_corpus() {
        return;
    }
    let mut truth = VideoSource::open(&corpus("ivtc_truth_ffv1.mkv"), &OpenOptions::default()).unwrap();
    let truth_luma: Vec<Vec<u8>> = (0..truth.info().num_frames).map(|i| luma(&truth.frame(i).unwrap())).collect();

    for name in ["ivtc_telecined_h264.m2ts", "ivtc_telecined_dvd.mpg"] {
        let mut src = ivtc(name);
        let n = src.info().num_frames;
        assert_eq!(n as usize, truth_luma.len(), "{name}: output frame count");
        let mut order: Vec<i32> = (0..n).collect();
        order.shuffle(&mut rand_chacha::ChaCha8Rng::seed_from_u64(5));
        let mut worst = f64::INFINITY;
        let mut wrong = Vec::new();
        for &k in &order {
            let y = luma(&src.frame(k).unwrap());
            let k = k as usize;
            let p_self = psnr(&y, &truth_luma[k]);
            let best = (k.saturating_sub(2)..(k + 3).min(truth_luma.len()))
                .max_by(|&a, &b| psnr(&y, &truth_luma[a]).total_cmp(&psnr(&y, &truth_luma[b])))
                .unwrap();
            if best != k {
                wrong.push((k, best));
            }
            worst = worst.min(p_self);
        }
        eprintln!("{name}: {n} frames, worst Y-PSNR vs truth {worst:.1} dB, retries {}", src.window_retries);
        assert!(wrong.is_empty(), "{name}: outputs matching a different original frame (got, best): {wrong:?}");
        assert!(worst > 30.0, "{name}: worst PSNR {worst:.1}");
    }
}

#[test]
fn deinterlace_mode_keeps_every_frame() {
    if !have_corpus() {
        return;
    }
    let vs = VideoSource::open(&corpus("ivtc_telecined_h264.m2ts"), &OpenOptions::default()).unwrap();
    let n_in = vs.info().num_frames;
    let mut src =
        FilteredSource::new(vs, FieldOptions { mode: FieldMode::Deinterlace, order: FieldOrder::Tff }, None, |_, _| true)
            .unwrap();
    assert_eq!(src.info().num_frames, n_in);
    let mut order: Vec<i32> = (0..n_in).collect();
    order.shuffle(&mut rand_chacha::ChaCha8Rng::seed_from_u64(3));
    for &k in &order {
        src.frame(k).unwrap();
    }
    eprintln!("deinterlace: {n_in} frames, retries {}", src.window_retries);
}

#[test]
fn field_map_cache_roundtrip() {
    if !have_corpus() {
        return;
    }
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("fieldmap-cache");
    let _ = std::fs::remove_dir_all(&dir);
    let opts = FieldOptions { mode: FieldMode::Ivtc, order: FieldOrder::Auto };
    let open = || VideoSource::open(&corpus("ivtc_telecined_ffv1.mkv"), &OpenOptions::default()).unwrap();
    let mut calls = 0;
    let a = FilteredSource::new(open(), opts, Some(&dir), |_, _| {
        calls += 1;
        true
    })
    .unwrap();
    assert!(calls > 0, "first open runs the analysis");
    let mut calls2 = 0;
    let b = FilteredSource::new(open(), opts, Some(&dir), |_, _| {
        calls2 += 1;
        true
    })
    .unwrap();
    assert_eq!(calls2, 0, "second open loads the cached map");
    assert_eq!(a.map(), b.map());
}
