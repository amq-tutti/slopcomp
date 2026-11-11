//! Offset suggestion against known offsets. The `cuts_late_*` clips start 49 frames
//! (49 × 1001/24000 = 2.044 s) after `cuts_ref.mkv`, at a different resolution, codec, colour and
//! aspect ratio (pillarboxed 4:3 DVD), or telecined (IVTC).

use std::path::{Path, PathBuf};

use comp_core::filter::{AnySource, FieldMode, FieldOptions, FieldOrder, FilteredSource};
use comp_core::source::{OpenOptions, VideoSource};
use comp_core::sync::{suggest_offset, SyncOptions};

fn corpus(name: &str) -> PathBuf {
    std::env::var_os("COMP_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/corpus"))
        .join(name)
}

fn plain(name: &str) -> AnySource {
    AnySource::Plain(VideoSource::open(&corpus(name), &OpenOptions::default()).unwrap())
}

const EXPECTED: f64 = -49.0 * 1001.0 / 24000.0;

#[test]
fn finds_known_offsets() {
    if !corpus("cuts_late_telecined.mkv").exists() {
        eprintln!("SKIP: corpus missing");
        return;
    }
    // The drift check decodes extra windows, which is slow on CI runners (especially through IVTC),
    // so only the DVD case runs it; the clips don't drift, so it must report none.
    let opts = SyncOptions { window_start: Some(8.0), window_len: 40.0, search: 10.0, drift_checks: 0 };
    let with_drift = SyncOptions { drift_checks: 2, ..opts.clone() };

    let mut r = plain("cuts_ref.mkv");
    let mut o = plain("cuts_late_dvd.mpg");
    let s = suggest_offset(&mut r, &mut o, &with_drift, |_| true).unwrap();
    eprintln!("DVD: {s:?}");
    assert!((s.offset_seconds - EXPECTED).abs() < 0.021, "{s:?}");
    assert!(s.precision_frames <= 1, "{s:?}");
    assert!(s.confidence > 0.5, "{s:?}");
    assert_eq!(s.drift_frames, 0, "no drift in the test clips: {s:?}");

    let vs = VideoSource::open(&corpus("cuts_late_telecined.mkv"), &OpenOptions::default()).unwrap();
    let mut o = AnySource::Filtered(Box::new(
        FilteredSource::new(vs, FieldOptions { mode: FieldMode::Ivtc, order: FieldOrder::Auto }, None, |_, _| true).unwrap(),
    ));
    let s = suggest_offset(&mut r, &mut o, &opts, |_| true).unwrap();
    eprintln!("IVTC: {s:?}");
    assert!((s.offset_seconds - EXPECTED).abs() < 0.021, "{s:?}");
    assert!(s.confidence > 0.5, "{s:?}");

    // Reverse direction: the late clip as reference.
    let mut r2 = plain("cuts_late_dvd.mpg");
    let mut o2 = plain("cuts_ref.mkv");
    let s = suggest_offset(&mut r2, &mut o2, &opts, |_| true).unwrap();
    eprintln!("reverse: {s:?}");
    assert!((s.offset_seconds + EXPECTED).abs() < 0.021, "{s:?}");
    assert!(s.confidence > 0.5, "{s:?}");

    // Same file against itself: zero.
    let mut a = plain("cuts_ref.mkv");
    let mut b = plain("cuts_ref.mkv");
    let s = suggest_offset(&mut a, &mut b, &opts, |_| true).unwrap();
    assert_eq!(s.offset_seconds, 0.0, "{s:?}");
    assert!(s.confidence > 0.9, "{s:?}");
}

#[test]
fn unrelated_content_has_low_confidence() {
    if !corpus("cuts_ref.mkv").exists() {
        return;
    }
    // Blurred test pattern vs a Mandelbrot zoom (cuts_black.mkv frames 144+, from 6 s): no relation.
    let mut r = plain("ivtc_truth_ffv1.mkv");
    let mut o = plain("cuts_black.mkv");
    let opts = SyncOptions { window_start: Some(6.5), window_len: 3.0, search: 0.4, drift_checks: 0 };
    let s = suggest_offset(&mut r, &mut o, &opts, |_| true).unwrap();
    eprintln!("unrelated: {s:?}");
    assert!(s.confidence < 0.25, "{s:?}");
}
