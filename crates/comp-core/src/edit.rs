//! Corrections after a comparison has been generated.
//!
//! [`Editor`] reopens a finished comparison from its `comp.json` and changes single frames:
//! * [`Editor::set_source_frame`] / [`Editor::nudge`]: re-shoot one source's frame (for example
//!   when two releases drift a frame apart in one scene);
//! * [`Editor::replace`]: swap a picked frame for another one for every source.
//!
//! Each change rewrites the affected PNGs, `comp.json` and the HTML viewer immediately.

use std::path::{Path, PathBuf};

use rand::Rng;

use crate::comp::{CompResult, Shot};
use crate::convert::{self, ConvertOptions};
use crate::error::{Error, Result};
use crate::filter::{AnySource, FieldMode, FieldOptions, FieldOrder, FilteredSource};
use crate::source::{self, OpenOptions, VideoSource};

struct Opened {
    src: AnySource,
}

pub struct Editor {
    pub result: CompResult,
    dir: PathBuf,
    sources: Vec<Option<Opened>>,
}

fn parse_field(s: Option<&str>) -> Option<FieldOptions> {
    let s = s?;
    let (mode, order) = s.split_once('-').unwrap_or((s, "auto"));
    let mode = match mode {
        "ivtc" => FieldMode::Ivtc,
        "deint" => FieldMode::Deinterlace,
        _ => return None,
    };
    let order = match order {
        "tff" => FieldOrder::Tff,
        "bff" => FieldOrder::Bff,
        _ => FieldOrder::Auto,
    };
    Some(FieldOptions { mode, order })
}

impl Editor {
    /// Loads `<dir>/comp.json`. Sources are opened lazily (indexes are normally cached).
    pub fn open(dir: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(dir.join("comp.json"))?;
        let mut result: CompResult =
            serde_json::from_str(&text).map_err(|e| Error::Spec(format!("{}: {e}", dir.join("comp.json").display())))?;
        result.out_dir = dir.to_owned();
        let n = result.sources.len();
        Ok(Self { result, dir: dir.to_owned(), sources: (0..n).map(|_| None).collect() })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn ensure(&mut self, s: usize) -> Result<()> {
        if self.sources.get(s).ok_or_else(|| Error::Spec(format!("no source #{s}")))?.is_some() {
            return Ok(());
        }
        let sr = &self.result.sources[s];
        let cache = source::default_cache_dir();
        let vs = VideoSource::open(Path::new(&sr.path), &OpenOptions { index_cache: cache.clone(), ..Default::default() })
            .map_err(|e| Error::Spec(format!("{}: {e}", sr.label)))?;
        let src = match parse_field(sr.field_processing.as_deref()) {
            None => AnySource::Plain(vs),
            Some(fo) => AnySource::Filtered(Box::new(FilteredSource::new(vs, fo, cache.as_deref(), |_, _| true)?)),
        };
        self.sources[s] = Some(Opened { src });
        Ok(())
    }

    fn convert_options(&self, s: usize) -> ConvertOptions {
        let sr = &self.result.sources[s];
        ConvertOptions {
            apply_sar: self.result.stretch_anamorphic,
            // Reuse exactly what the original run used, so re-shot frames match the others.
            matrix_override: Some(sr.matrix),
            full_range_override: Some(sr.full_range),
            crop: Some(sr.crop),
            ..Default::default()
        }
    }

    fn index_of(&self, reference_frame: i32) -> Result<usize> {
        self.result
            .frames
            .iter()
            .position(|&f| f == reference_frame)
            .ok_or_else(|| Error::Spec(format!("frame {reference_frame} isn't in this comparison")))
    }

    /// Decodes `frame` of source `s` and writes it as `file`. Returns the new shot metadata.
    fn shoot(&mut self, s: usize, frame: i32, reference_frame: i32, reference_time: f64, file: String) -> Result<Shot> {
        self.ensure(s)?;
        let conv = self.convert_options(s);
        let opened = self.sources[s].as_mut().unwrap();
        let n = opened.src.info().num_frames;
        if frame < 0 || frame >= n {
            return Err(Error::FrameOutOfRange { frame: frame as i64, count: n });
        }
        let source_time = opened.src.times()[frame as usize];
        let sar = opened.src.info().sar;
        let filtered = opened.src.is_filtered();
        let f = opened.src.frame(frame)?;
        let pict = f.pict_type;
        let img = convert::to_rgb(&f, sar, &conv)?;
        convert::write_png(&img, &self.dir.join(&file), Default::default())?;
        Ok(Shot {
            reference_frame,
            source_frame: frame,
            reference_time,
            source_time,
            pict_type: if filtered { format!("{pict}*") } else { pict.to_string() },
            file,
            width: img.width,
            height: img.height,
        })
    }

    /// Re-shoots source `s` at `source_frame` for the comparison frame `reference_frame`.
    pub fn set_source_frame(&mut self, reference_frame: i32, s: usize, source_frame: i32) -> Result<()> {
        let k = self.index_of(reference_frame)?;
        let old = self.result.sources[s].shots[k].clone();
        let shot = self.shoot(s, source_frame, old.reference_frame, old.reference_time, old.file.clone())?;
        self.result.sources[s].shots[k] = shot;
        self.save()
    }

    /// Moves source `s` by `delta` frames for one comparison frame.
    pub fn nudge(&mut self, reference_frame: i32, s: usize, delta: i32) -> Result<()> {
        let k = self.index_of(reference_frame)?;
        let cur = self.result.sources[s].shots[k].source_frame;
        self.set_source_frame(reference_frame, s, cur + delta)
    }

    /// Replaces comparison frame `reference_frame` with `new_reference_frame` for every source,
    /// mapping it into each source by timestamp and offsets as in the original run.
    pub fn replace(&mut self, reference_frame: i32, new_reference_frame: i32) -> Result<()> {
        let k = self.index_of(reference_frame)?;
        if self.result.frames.contains(&new_reference_frame) {
            return Err(Error::Spec(format!("frame {new_reference_frame} is already in this comparison")));
        }
        for s in 0..self.result.sources.len() {
            self.ensure(s)?;
        }
        let ref_src = &self.sources[0].as_ref().unwrap().src;
        let n_ref = ref_src.info().num_frames;
        if new_reference_frame < 0 || new_reference_frame >= n_ref {
            return Err(Error::FrameOutOfRange { frame: new_reference_frame as i64, count: n_ref });
        }
        let t = ref_src.times()[new_reference_frame as usize];
        // Map into every source first so nothing is written if any source lacks the frame.
        let mut targets = Vec::new();
        for (s, sr) in self.result.sources.iter().enumerate() {
            let src = &self.sources[s].as_ref().unwrap().src;
            let f = if s == 0 {
                Some(new_reference_frame)
            } else {
                let n = src.info().num_frames;
                src.frame_at_time(t + sr.offset_seconds).map(|f| f + sr.offset_frames).filter(|f| (0..n).contains(f))
            };
            targets.push(f.ok_or_else(|| Error::Spec(format!("{} has no frame at that time", sr.label)))?);
        }
        for (s, &f) in targets.iter().enumerate() {
            let old = self.result.sources[s].shots[k].clone();
            // File names start with the reference frame number; build the new one from the old
            // suffix (`_<n>-<label>.png`).
            let suffix = old.file.split_once('_').map(|(_, r)| r.to_string()).unwrap_or_else(|| old.file.clone());
            let file = format!("{new_reference_frame:06}_{suffix}");
            let shot = self.shoot(s, f, new_reference_frame, t, file)?;
            if shot.file != old.file {
                let _ = std::fs::remove_file(self.dir.join(&old.file));
            }
            self.result.sources[s].shots[k] = shot;
        }
        self.result.frames[k] = new_reference_frame;
        // Keep frames (and every source's shots) in ascending order.
        let mut order: Vec<usize> = (0..self.result.frames.len()).collect();
        order.sort_by_key(|&i| self.result.frames[i]);
        self.result.frames = order.iter().map(|&i| self.result.frames[i]).collect();
        for sr in &mut self.result.sources {
            sr.shots = order.iter().map(|&i| sr.shots[i].clone()).collect();
        }
        self.save()
    }

    /// A random replacement for `reference_frame`: somewhere between its neighbouring picks
    /// (exclusive), so the spread over the episode is kept.
    pub fn random_replacement(&self, reference_frame: i32) -> Result<i32> {
        let k = self.index_of(reference_frame)?;
        let frames = &self.result.frames;
        let lo = if k > 0 { (frames[k - 1] + frames[k]) / 2 + 1 } else { self.result.usable_range.0 };
        let hi = if k + 1 < frames.len() { (frames[k] + frames[k + 1]) / 2 } else { self.result.usable_range.1 };
        if hi - lo < 2 {
            return Err(Error::Spec("no room for another frame here".into()));
        }
        let mut rng = rand::thread_rng();
        for _ in 0..32 {
            let f = rng.gen_range(lo..hi);
            if f != reference_frame && !frames.contains(&f) {
                return Ok(f);
            }
        }
        Err(Error::Spec("couldn't find a replacement frame".into()))
    }

    /// Writes `comp.json` and the HTML viewer.
    pub fn save(&self) -> Result<()> {
        let json = serde_json::to_string_pretty(&self.result).expect("serialise comp.json");
        std::fs::write(self.dir.join("comp.json"), json)?;
        crate::viewer::write(&self.result)?;
        Ok(())
    }
}
