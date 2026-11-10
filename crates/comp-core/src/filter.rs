//! Field processing for interlaced and telecined sources (DVD, 1080i BD): IVTC and deinterlacing.
//!
//! # How random access works on top of a frame-dropping filter chain
//!
//! IVTC (`fieldmatch` → `bwdif` for leftover combed frames → `decimate`) turns 5 telecined frames
//! into 4 progressive ones, so "output frame k" only has meaning after the whole stream has been
//! through the filter chain. We handle that in two steps:
//!
//! 1. **Analysis pass** (once per file, cached): every source frame is fed through the chain in
//!    order. Each input frame is tagged with its index in `AVFrame.opaque`, which every filter in the
//!    chain carries over. That gives a [`FieldMap`] recording, for every output frame, which source
//!    frame it came from and an xxh3 hash of its pixels.
//! 2. **Random access**: to get output frame `k`, rebuild the chain and feed a short window of source
//!    frames, starting a few decimation cycles earlier and aligned to a cycle boundary. Then take the
//!    output tagged with the wanted source index. Its hash must equal the analysis-pass hash, so the
//!    result is *verified identical* to sequential processing. If it isn't, the window grows,
//!    falling back to processing from frame 0.
//!
//! Ascending requests reuse the running chain, so a full comparison costs roughly one pass over the
//! picked range.

use std::collections::VecDeque;
use std::ffi::CString;
use std::io::{Read, Write};
use std::os::raw::c_int;
use std::path::{Path, PathBuf};

use comp_sys as sys;

use crate::error::{Error, Result};
use crate::source::{self, ColorInfo, Frame, PixelLayout, SeekMode, VideoInfo, VideoSource};

/// What to do with fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldMode {
    /// Inverse telecine: field matching + decimation (29.97i telecined film → 23.976p).
    /// Frames that stay combed after matching are deinterlaced.
    Ivtc,
    /// Deinterlace every frame (bwdif, one output per input frame).
    Deinterlace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FieldOrder {
    /// From the stream (falls back to top field first).
    #[default]
    Auto,
    Tff,
    Bff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldOptions {
    pub mode: FieldMode,
    pub order: FieldOrder,
}

impl FieldOptions {
    fn cycle(&self) -> i32 {
        match self.mode {
            FieldMode::Ivtc => 5,
            FieldMode::Deinterlace => 1,
        }
    }

    /// Cycles fed before the target cycle in the first random-access attempt.
    fn preroll_cycles(&self) -> i32 {
        match self.mode {
            FieldMode::Ivtc => 2,
            FieldMode::Deinterlace => 4,
        }
    }

    fn tag(&self, tff: bool) -> String {
        format!(
            "{}-{}",
            match self.mode {
                FieldMode::Ivtc => "ivtc",
                FieldMode::Deinterlace => "deint",
            },
            if tff { "tff" } else { "bff" }
        )
    }
}

// ------------------------------------------------------------------------------------------------
// Owned frames

/// A decoded frame that owns its pixels (planes tightly packed).
#[derive(Debug, Clone)]
pub struct OwnedFrame {
    pub width: u32,
    pub height: u32,
    pub layout: PixelLayout,
    pub color: ColorInfo,
    pub interlaced: bool,
    planes: Vec<Vec<u8>>,
}

impl OwnedFrame {
    /// Borrowed view for conversion / hashing.
    pub fn as_frame(&self, number: i32) -> Frame<'_> {
        let mut planes: [(&[u8], usize); 3] = [(&[], 0); 3];
        for (p, data) in self.planes.iter().enumerate() {
            let (pw, _) = self.layout.plane_dims(p, self.width as usize, self.height as usize);
            planes[p] = (data.as_slice(), pw * self.layout.bytes_per_sample);
        }
        Frame {
            number,
            width: self.width,
            height: self.height,
            layout: self.layout,
            color: self.color,
            pict_type: '?',
            key_frame: false,
            interlaced: self.interlaced,
            top_field_first: false,
            planes,
        }
    }
}

/// xxh3 of the frame's pixels (rows without padding).
pub fn frame_hash(f: &Frame<'_>) -> u64 {
    let mut h = xxhash_rust::xxh3::Xxh3::new();
    for p in 0..f.layout.planes {
        let (_, ph) = f.layout.plane_dims(p, f.width as usize, f.height as usize);
        for y in 0..ph {
            h.update(f.row(p, y));
        }
    }
    h.digest()
}

// ------------------------------------------------------------------------------------------------
// libavfilter chain

const AVERROR_EOF: c_int = -(0x45 | (0x4F << 8) | (0x46 << 16) | (0x20 << 24));
#[cfg(target_os = "macos")]
const AVERROR_EAGAIN: c_int = -35;
#[cfg(not(target_os = "macos"))]
const AVERROR_EAGAIN: c_int = -11;

fn av_err(ctx: &str, code: c_int) -> Error {
    let mut buf = [0 as std::os::raw::c_char; 256];
    unsafe { sys::av_strerror(code, buf.as_mut_ptr(), buf.len()) };
    let msg = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned();
    Error::Filter(format!("{ctx}: {msg} ({code})"))
}

/// A configured filter chain: buffer → … → buffersink.
struct Chain {
    graph: *mut sys::AVFilterGraph,
    src: *mut sys::AVFilterContext,
    sink: *mut sys::AVFilterContext,
    layout: PixelLayout,
    color: ColorInfo,
    tff: bool,
    eof_sent: bool,
}

/// One output of the chain: the source frame index it was derived from, and its pixels.
struct ChainOut {
    src_index: i32,
    frame: OwnedFrame,
}

impl Chain {
    fn new(opts: &FieldOptions, tff: bool, info: &VideoInfo) -> Result<Self> {
        if info.layout.bytes_per_sample != 1 || info.layout.planes != 3 {
            return Err(Error::Filter(format!(
                "field processing needs 8-bit YUV input (source is {}-bit)",
                info.layout.bit_depth
            )));
        }
        let parity = if tff { "tff" } else { "bff" };
        let stages: Vec<(&str, String)> = match opts.mode {
            FieldMode::Ivtc => vec![
                // combmatch=sc (default, as VFM): re-matching every "combed" frame turns false positives on
                // sharp detail into wrong matches.
                ("fieldmatch", format!("order={parity}")),
                ("bwdif", format!("mode=send_frame:parity={parity}:deint=interlaced")),
                ("decimate", String::new()),
            ],
            FieldMode::Deinterlace => vec![("bwdif", format!("mode=send_frame:parity={parity}:deint=all"))],
        };
        unsafe {
            let graph = sys::avfilter_graph_alloc();
            if graph.is_null() {
                return Err(Error::Filter("avfilter_graph_alloc failed".into()));
            }
            let mut chain = Chain {
                graph,
                src: std::ptr::null_mut(),
                sink: std::ptr::null_mut(),
                layout: info.layout,
                color: info.color,
                tff,
                eof_sent: false,
            };
            let (fn_, fd) = if info.fps_num > 0 && info.fps_den > 0 { (info.fps_num, info.fps_den) } else { (30000, 1001) };
            let src_args = format!(
                "video_size={}x{}:pix_fmt={}:time_base={fd}/{fn_}:frame_rate={fn_}/{fd}:pixel_aspect=1/1",
                info.width, info.height, info.layout.av_format
            );
            chain.src = chain.create("buffer", "in", &src_args)?;
            let mut prev = chain.src;
            for (i, (name, args)) in stages.iter().enumerate() {
                let ctx = chain.create(name, &format!("f{i}"), args)?;
                chain.link(prev, ctx)?;
                prev = ctx;
            }
            chain.sink = chain.create("buffersink", "out", "")?;
            chain.link(prev, chain.sink)?;
            let rc = sys::avfilter_graph_config(graph, std::ptr::null_mut());
            if rc < 0 {
                return Err(av_err("configure filter graph", rc));
            }
            Ok(chain)
        }
    }

    unsafe fn create(&mut self, filter: &str, name: &str, args: &str) -> Result<*mut sys::AVFilterContext> {
        let cfilter = CString::new(filter).unwrap();
        let f = sys::avfilter_get_by_name(cfilter.as_ptr());
        if f.is_null() {
            return Err(Error::Filter(format!("filter '{filter}' not built into ffmpeg")));
        }
        let cname = CString::new(name).unwrap();
        let cargs = CString::new(args).unwrap();
        let mut ctx = std::ptr::null_mut();
        let rc = sys::avfilter_graph_create_filter(
            &mut ctx,
            f,
            cname.as_ptr(),
            if args.is_empty() { std::ptr::null() } else { cargs.as_ptr() },
            std::ptr::null_mut(),
            self.graph,
        );
        if rc < 0 {
            return Err(av_err(&format!("create {filter}({args})"), rc));
        }
        Ok(ctx)
    }

    unsafe fn link(&mut self, a: *mut sys::AVFilterContext, b: *mut sys::AVFilterContext) -> Result<()> {
        let rc = sys::avfilter_link(a, 0, b, 0);
        if rc < 0 {
            return Err(av_err("link filters", rc));
        }
        Ok(())
    }

    /// Feeds source frame `index`.
    fn push(&mut self, f: &Frame<'_>, index: i32) -> Result<()> {
        unsafe {
            let mut av = sys::av_frame_alloc();
            if av.is_null() {
                return Err(Error::Filter("av_frame_alloc failed".into()));
            }
            (*av).width = f.width as c_int;
            (*av).height = f.height as c_int;
            (*av).format = f.layout.av_format;
            let rc = sys::av_frame_get_buffer(av, 0);
            if rc < 0 {
                sys::av_frame_free(&mut av);
                return Err(av_err("av_frame_get_buffer", rc));
            }
            for p in 0..f.layout.planes {
                let (_, ph) = f.layout.plane_dims(p, f.width as usize, f.height as usize);
                let stride = (*av).linesize[p] as usize;
                for y in 0..ph {
                    let row = f.row(p, y);
                    std::ptr::copy_nonoverlapping(row.as_ptr(), (*av).data[p].add(y * stride), row.len());
                }
            }
            (*av).pts = index as i64;
            (*av).opaque = (index as usize + 1) as *mut _;
            let mut flags = sys::AV_FRAME_FLAG_INTERLACED as c_int;
            if self.tff {
                flags |= sys::AV_FRAME_FLAG_TOP_FIELD_FIRST as c_int;
            }
            (*av).flags = flags;
            let rc = sys::av_buffersrc_add_frame_flags(self.src, av, 0);
            sys::av_frame_free(&mut av);
            if rc < 0 {
                return Err(av_err("push frame", rc));
            }
        }
        Ok(())
    }

    /// Signals end of stream so delayed frames come out.
    fn flush(&mut self) -> Result<()> {
        if !self.eof_sent {
            self.eof_sent = true;
            let rc = unsafe { sys::av_buffersrc_add_frame_flags(self.src, std::ptr::null_mut(), 0) };
            if rc < 0 {
                return Err(av_err("flush", rc));
            }
        }
        Ok(())
    }

    /// Next available output, if any.
    fn pull(&mut self) -> Result<Option<ChainOut>> {
        unsafe {
            let mut av = sys::av_frame_alloc();
            let rc = sys::av_buffersink_get_frame(self.sink, av);
            if rc == AVERROR_EAGAIN || rc == AVERROR_EOF {
                sys::av_frame_free(&mut av);
                return Ok(None);
            }
            if rc < 0 {
                sys::av_frame_free(&mut av);
                return Err(av_err("pull frame", rc));
            }
            let (w, h) = ((*av).width as usize, (*av).height as usize);
            let mut planes = Vec::with_capacity(self.layout.planes);
            for p in 0..self.layout.planes {
                let (pw, ph) = self.layout.plane_dims(p, w, h);
                let row_bytes = pw * self.layout.bytes_per_sample;
                let stride = (*av).linesize[p] as usize;
                let mut buf = vec![0u8; row_bytes * ph];
                for y in 0..ph {
                    std::ptr::copy_nonoverlapping((*av).data[p].add(y * stride), buf.as_mut_ptr().add(y * row_bytes), row_bytes);
                }
                planes.push(buf);
            }
            let tag = (*av).opaque as usize;
            let interlaced = (*av).flags & sys::AV_FRAME_FLAG_INTERLACED as c_int != 0;
            sys::av_frame_free(&mut av);
            if tag == 0 {
                return Err(Error::Filter("filter output lost its source tag".into()));
            }
            Ok(Some(ChainOut {
                src_index: (tag - 1) as i32,
                frame: OwnedFrame { width: w as u32, height: h as u32, layout: self.layout, color: self.color, interlaced, planes },
            }))
        }
    }
}

// A filter graph is owned exclusively by its Chain and only used from one thread at a time.
unsafe impl Send for Chain {}

impl Drop for Chain {
    fn drop(&mut self) {
        unsafe { sys::avfilter_graph_free(&mut self.graph) };
    }
}

// ------------------------------------------------------------------------------------------------
// Field map (analysis result)

/// Result of the analysis pass: for each output frame, its source frame and pixel hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldMap {
    pub src_of: Vec<i32>,
    pub hash: Vec<u64>,
}

const MAP_MAGIC: &[u8; 8] = b"ACFMAP01";

impl FieldMap {
    fn save(&self, path: &Path) -> std::io::Result<()> {
        let tmp = path.with_extension("tmp");
        let mut f = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
        f.write_all(MAP_MAGIC)?;
        f.write_all(&(self.src_of.len() as u64).to_le_bytes())?;
        for (s, h) in self.src_of.iter().zip(&self.hash) {
            f.write_all(&s.to_le_bytes())?;
            f.write_all(&h.to_le_bytes())?;
        }
        f.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        std::fs::rename(tmp, path)
    }

    fn load(path: &Path) -> Option<Self> {
        let mut data = Vec::new();
        std::fs::File::open(path).ok()?.read_to_end(&mut data).ok()?;
        if data.len() < 16 || &data[..8] != MAP_MAGIC {
            return None;
        }
        let n = u64::from_le_bytes(data[8..16].try_into().ok()?) as usize;
        if data.len() != 16 + n * 12 {
            return None;
        }
        let mut src_of = Vec::with_capacity(n);
        let mut hash = Vec::with_capacity(n);
        for rec in data[16..].chunks_exact(12) {
            src_of.push(i32::from_le_bytes(rec[..4].try_into().unwrap()));
            hash.push(u64::from_le_bytes(rec[4..].try_into().unwrap()));
        }
        Some(Self { src_of, hash })
    }
}

// ------------------------------------------------------------------------------------------------
// Filtered source

/// A [`VideoSource`] seen through IVTC or deinterlacing, with frame-exact random access.
pub struct FilteredSource {
    inner: VideoSource,
    opts: FieldOptions,
    tff: bool,
    map: FieldMap,
    info: VideoInfo,
    times: Vec<f64>,
    /// Running chain from the last request, for cheap ascending access.
    running: Option<Running>,
    current: Option<OwnedFrame>,
    /// Statistics: how many requests needed a wider window than the first attempt.
    pub window_retries: u32,
}

struct Running {
    chain: Chain,
    next_feed: i32,
    recent: VecDeque<ChainOut>,
}

impl FilteredSource {
    /// Wraps an opened source. Runs (or loads from `cache_dir`) the analysis pass.
    /// `progress(done, total)` is called during analysis. Return `false` to cancel.
    pub fn new<F>(inner: VideoSource, opts: FieldOptions, cache_dir: Option<&Path>, mut progress: F) -> Result<Self>
    where
        F: FnMut(i64, i64) -> bool,
    {
        let info = inner.info();
        if opts.mode == FieldMode::Ivtc && info.fps_den > 0 && (info.fps_num as f64 / info.fps_den as f64) < 25.5 {
            return Err(Error::Filter(format!(
                "{} is already {:.3} fps; IVTC expects 29.97i telecined input (decimating would drop real frames)",
                inner.path().display(),
                info.fps_num as f64 / info.fps_den as f64
            )));
        }
        let tff = match opts.order {
            FieldOrder::Tff => true,
            FieldOrder::Bff => false,
            FieldOrder::Auto => inner.top_field_first().unwrap_or(true),
        };
        let cache_file: Option<PathBuf> = match cache_dir {
            Some(dir) => {
                std::fs::create_dir_all(dir)?;
                Some(dir.join(format!("{}.{}.fieldmap", source::cache_key(inner.path())?, opts.tag(tff))))
            }
            None => None,
        };
        let mut this = Self {
            info: inner.info().clone(),
            inner,
            opts,
            tff,
            map: FieldMap { src_of: vec![], hash: vec![] },
            times: vec![],
            running: None,
            current: None,
            window_retries: 0,
        };
        let cached = cache_file.as_deref().and_then(FieldMap::load);
        this.map = match cached {
            Some(m) => m,
            None => {
                let m = this.analyze(&mut progress)?;
                if let Some(cf) = &cache_file {
                    let _ = m.save(cf); // cache write failures aren't fatal
                }
                m
            }
        };
        this.finish_setup()?;
        Ok(this)
    }

    fn analyze<F: FnMut(i64, i64) -> bool>(&mut self, progress: &mut F) -> Result<FieldMap> {
        let n = self.inner.info().num_frames;
        let mut chain = Chain::new(&self.opts, self.tff, self.inner.info())?;
        let mut map = FieldMap { src_of: Vec::new(), hash: Vec::new() };
        let take = |chain: &mut Chain, map: &mut FieldMap| -> Result<()> {
            while let Some(out) = chain.pull()? {
                map.hash.push(frame_hash(&out.frame.as_frame(0)));
                map.src_of.push(out.src_index);
            }
            Ok(())
        };
        for i in 0..n {
            let f = self.inner.frame(i)?;
            chain.push(&f, i)?;
            take(&mut chain, &mut map)?;
            if i % 64 == 0 && !progress(i as i64, n as i64) {
                return Err(Error::Cancelled);
            }
        }
        chain.flush()?;
        take(&mut chain, &mut map)?;
        progress(n as i64, n as i64);
        if map.src_of.is_empty() {
            return Err(Error::Filter("field processing produced no frames".into()));
        }
        if map.src_of.windows(2).any(|w| w[0] >= w[1]) {
            return Err(Error::Filter("filter output is not in source order".into()));
        }
        Ok(map)
    }

    /// Derives info and timestamps from the map.
    fn finish_setup(&mut self) -> Result<()> {
        let n_out = self.map.src_of.len();
        let src_times = self.inner.times();
        let cycle = self.opts.cycle();
        self.times = match self.opts.mode {
            FieldMode::Deinterlace => self.map.src_of.iter().map(|&s| src_times[s as usize]).collect(),
            FieldMode::Ivtc => {
                // Output frames are CFR at 4/5 of the source rate. Anchor each decimation cycle at
                // its first source frame's timestamp, then space its outputs evenly across the
                // cycle. Telecined field timestamps are off by up to half a film frame, which would
                // make time-based alignment with progressive sources pick the neighbouring frame.
                let mut t = Vec::with_capacity(n_out);
                let mut k = 0;
                while k < n_out {
                    let c = self.map.src_of[k] / cycle;
                    let mut end = k;
                    while end < n_out && self.map.src_of[end] / cycle == c {
                        end += 1;
                    }
                    let start_src = (c * cycle) as usize;
                    let next_src = ((c + 1) * cycle) as usize;
                    let t0 = src_times[start_src.min(src_times.len() - 1)];
                    let t1 = if next_src < src_times.len() {
                        src_times[next_src]
                    } else {
                        // Last (possibly partial) cycle: extrapolate with the average frame duration.
                        let avg = src_times.last().unwrap() / (src_times.len().max(2) - 1) as f64;
                        t0 + avg * cycle as f64
                    };
                    // The decimated cycle keeps cycle-1 frames regardless of how many come out in
                    // this window, so the spacing is (cycle length) / (cycle - 1).
                    let step = (t1 - t0) / (cycle - 1) as f64;
                    for j in 0..(end - k) {
                        t.push(t0 + step * j as f64);
                    }
                    k = end;
                }
                t
            }
        };
        self.info.num_frames = n_out as i32;
        if self.opts.mode == FieldMode::Ivtc {
            let (n, d) = (self.info.fps_num as i64 * 4, self.info.fps_den as i64 * 5);
            let g = gcd(n, d).max(1);
            self.info.fps_num = (n / g) as i32;
            self.info.fps_den = (d / g) as i32;
        }
        self.info.seek_mode = SeekMode::Normal;
        Ok(())
    }

    pub fn info(&self) -> &VideoInfo {
        &self.info
    }

    pub fn inner(&self) -> &VideoSource {
        &self.inner
    }

    pub fn map(&self) -> &FieldMap {
        &self.map
    }

    pub fn top_field_first(&self) -> bool {
        self.tff
    }

    /// Output frame timestamps in seconds from the first output frame's source time.
    pub fn times(&self) -> &[f64] {
        &self.times
    }

    pub fn frame_at_time(&self, t: f64) -> Option<i32> {
        source::frame_at_time(&self.times, t)
    }

    /// Output frame `k`, verified identical to what the sequential analysis pass produced.
    pub fn frame(&mut self, k: i32) -> Result<Frame<'_>> {
        let n = self.map.src_of.len() as i32;
        if k < 0 || k >= n {
            return Err(Error::FrameOutOfRange { frame: k as i64, count: n });
        }
        let want_src = self.map.src_of[k as usize];
        let want_hash = self.map.hash[k as usize];
        let cycle = self.opts.cycle();
        let target_cycle = want_src / cycle;

        // 1. Continue the running chain if the target is just ahead (or was just emitted).
        if let Some(out) = self.try_running(want_src, cycle)? {
            if frame_hash(&out.as_frame(k)) == want_hash {
                self.current = Some(out);
                return Ok(self.current.as_ref().unwrap().as_frame(k));
            }
        }

        // 2. Fresh windows, growing on hash mismatch, ending with a full pass from frame 0.
        let mut preroll = self.opts.preroll_cycles();
        loop {
            let start_cycle = (target_cycle - preroll).max(0);
            let start = start_cycle * cycle;
            let got = self.run_window(start, want_src, cycle)?;
            if let Some(out) = got {
                if frame_hash(&out.as_frame(k)) == want_hash {
                    self.current = Some(out);
                    return Ok(self.current.as_ref().unwrap().as_frame(k));
                }
            }
            if start == 0 {
                return Err(Error::Filter(format!(
                    "output frame {k} (source frame {want_src}) differs from the analysis pass even when processed from the start; \
                     the cached field map may be stale"
                )));
            }
            self.window_retries += 1;
            preroll = (preroll * 4).max(8);
        }
    }

    /// Looks for `want_src` in the running chain's recent outputs, or feeds forward to it if it's
    /// close ahead. Returns `None` if the running chain can't produce it.
    fn try_running(&mut self, want_src: i32, cycle: i32) -> Result<Option<OwnedFrame>> {
        const MAX_AHEAD: i32 = 600;
        let Some(run) = self.running.as_mut() else { return Ok(None) };
        if let Some(pos) = run.recent.iter().position(|o| o.src_index == want_src) {
            return Ok(Some(run.recent[pos].frame.clone()));
        }
        // The output for want_src must not have been emitted already (recent only keeps a few).
        let newest = run.recent.back().map(|o| o.src_index).unwrap_or(-1);
        if want_src <= newest || want_src - run.next_feed > MAX_AHEAD {
            return Ok(None);
        }
        let found = feed_until(&mut self.inner, run, want_src, cycle)?;
        Ok(found)
    }

    fn run_window(&mut self, start: i32, want_src: i32, cycle: i32) -> Result<Option<OwnedFrame>> {
        self.running = None;
        let chain = Chain::new(&self.opts, self.tff, self.inner.info())?;
        let mut run = Running { chain, next_feed: start, recent: VecDeque::new() };
        let found = feed_until(&mut self.inner, &mut run, want_src, cycle)?;
        self.running = Some(run);
        Ok(found)
    }
}

/// Feeds source frames into a running chain until the output for `want_src` appears, or until
/// it's clear that it won't (two cycles past it, or end of stream).
fn feed_until(inner: &mut VideoSource, run: &mut Running, want_src: i32, cycle: i32) -> Result<Option<OwnedFrame>> {
    const KEEP_RECENT: usize = 8;
    let n = inner.info().num_frames;
    let give_up_at = ((want_src / cycle) + 3) * cycle;
    loop {
        // Drain outputs first.
        while let Some(out) = run.chain.pull()? {
            let hit = out.src_index == want_src;
            let past = out.src_index > want_src;
            run.recent.push_back(out);
            if run.recent.len() > KEEP_RECENT {
                run.recent.pop_front();
            }
            if hit {
                return Ok(Some(run.recent.back().unwrap().frame.clone()));
            }
            if past {
                // The wanted source frame was decimated in this window: not reproducible here.
                return Ok(None);
            }
        }
        if run.next_feed >= n {
            if run.chain.eof_sent {
                return Ok(None);
            }
            run.chain.flush()?;
            continue;
        }
        if run.next_feed >= give_up_at {
            return Ok(None);
        }
        let i = run.next_feed;
        let f = inner.frame(i)?;
        run.chain.push(&f, i)?;
        run.next_feed += 1;
    }
}

fn gcd(a: i64, b: i64) -> i64 {
    if b == 0 { a.abs() } else { gcd(b, a % b) }
}

// ------------------------------------------------------------------------------------------------
// Either kind of source

/// A plain source or a field-processed one, with the same frame-access API.
pub enum AnySource {
    Plain(VideoSource),
    Filtered(Box<FilteredSource>),
}

impl AnySource {
    pub fn info(&self) -> &VideoInfo {
        match self {
            AnySource::Plain(s) => s.info(),
            AnySource::Filtered(s) => s.info(),
        }
    }
    pub fn times(&self) -> &[f64] {
        match self {
            AnySource::Plain(s) => s.times(),
            AnySource::Filtered(s) => s.times(),
        }
    }
    pub fn frame_at_time(&self, t: f64) -> Option<i32> {
        match self {
            AnySource::Plain(s) => s.frame_at_time(t),
            AnySource::Filtered(s) => s.frame_at_time(t),
        }
    }
    pub fn frame(&mut self, n: i32) -> Result<Frame<'_>> {
        match self {
            AnySource::Plain(s) => s.frame(n),
            AnySource::Filtered(s) => s.frame(n),
        }
    }
}
