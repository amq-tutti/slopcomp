//! Post-generation corrections: re-shooting one source's frame, nudging, and replacing a frame
//! keep the PNGs, comp.json and the viewer consistent.

use std::path::{Path, PathBuf};

use comp_core::comp::{self, CompSpec, FramePick, SourceSpec};
use comp_core::edit::Editor;
use comp_core::source::OpenOptions;

fn corpus(name: &str) -> PathBuf {
    std::env::var_os("COMP_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/corpus"))
        .join(name)
}

#[test]
fn manual_corrections() {
    if !corpus("cuts_late_dvd.mpg").exists() {
        eprintln!("SKIP: corpus missing");
        return;
    }
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("edit");
    let _ = std::fs::remove_dir_all(&dir);
    // The late copy is really at -2.044 s (49 frames); add +2 frames of error on purpose.
    let mut late = SourceSpec::new("LATE", corpus("cuts_late_dvd.mpg"));
    late.offset_seconds = -2.044;
    late.offset_frames = 2;
    let spec = CompSpec {
        name: "edit".into(),
        sources: vec![SourceSpec::new("REF", corpus("cuts_ref.mkv")), late],
        pick: FramePick::Explicit(vec![60, 120, 1000]),
        out_dir: dir.clone(),
        convert: Default::default(),
        png: Default::default(),
        open: OpenOptions::default(),
        select: Default::default(),
        cancel: None,
    };
    comp::run(&spec, &|_| {}).unwrap();
    let source_frame = |rf: i32| {
        let r = Editor::open(&dir).unwrap().result;
        let k = r.frames.iter().position(|&f| f == rf).unwrap();
        r.sources[1].shots[k].source_frame
    };
    assert_eq!(source_frame(120), 120 - 49 + 2);

    let mut ed = Editor::open(&dir).unwrap();
    // Nudge back by two, and check the PNG and comp.json follow.
    let png = dir.join(&ed.result.sources[1].shots[1].file);
    let before = std::fs::read(&png).unwrap();
    ed.nudge(120, 1, -2).unwrap();
    assert_eq!(source_frame(120), 120 - 49);
    assert_ne!(std::fs::read(&png).unwrap(), before, "PNG re-shot");
    // Set an exact frame.
    ed.set_source_frame(60, 1, 11).unwrap();
    assert_eq!(source_frame(60), 11);
    // Out-of-range frames are refused.
    assert!(ed.set_source_frame(60, 1, 999_999).is_err());

    // Replace frame 1000 with 1100: files renamed, order kept, old PNGs removed.
    let old_files: Vec<String> = ed.result.sources.iter().map(|s| s.shots[2].file.clone()).collect();
    ed.replace(1000, 1100).unwrap();
    let r = Editor::open(&dir).unwrap().result;
    assert_eq!(r.frames, vec![60, 120, 1100]);
    for s in &r.sources {
        assert_eq!(s.shots[2].reference_frame, 1100);
        assert!(s.shots[2].file.starts_with("001100_"), "{}", s.shots[2].file);
        assert!(dir.join(&s.shots[2].file).exists());
    }
    assert_eq!(r.sources[1].shots[2].source_frame, 1100 - 49 + 2, "replace maps with the run's offsets");
    for f in old_files {
        assert!(!dir.join(f).exists(), "old PNG removed");
    }
    // Replacing with an earlier frame re-sorts; a frame missing from a source is refused.
    let mut ed = Editor::open(&dir).unwrap();
    ed.replace(1100, 90).unwrap();
    assert_eq!(Editor::open(&dir).unwrap().result.frames, vec![60, 90, 120]);
    assert!(ed.replace(90, 10).is_err(), "LATE starts 49 frames later");
    assert_eq!(Editor::open(&dir).unwrap().result.frames, vec![60, 90, 120]);
    // A random replacement stays between the neighbouring picks.
    let f = ed.random_replacement(90).unwrap();
    assert!(f > 75 && f < 105 && f != 90, "{f}");
    assert!(dir.join("index.html").exists());
}
