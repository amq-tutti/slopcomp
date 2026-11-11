//! Background worker that decodes aligned preview frames for the sync view.
//!
//! The UI sends [`Request`]s; the worker coalesces them (only the newest configure / frame
//! request matters while the user drags a slider) and answers with [`Reply`]s.
//!
//! Opening a file (indexing, IVTC field analysis) can take minutes for a new remux episode, so it
//! runs on its own thread per file. The worker itself never blocks on it: switching back to an
//! episode whose sources are already open (or cached) shows it immediately while other episodes
//! keep preparing in the background. Opened sources are kept in a small pool for quick switching.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::thread;

use comp_core::convert::{self, ConvertOptions};
use comp_core::filter::{AnySource, FieldOptions, FilteredSource};
use comp_core::source::{self, OpenOptions, VideoSource};
use comp_core::sync::{suggest_offset, SyncOptions, SyncSuggestion};

#[derive(Debug, Clone, PartialEq)]
pub struct SourceConfig {
    pub path: PathBuf,
    pub field: Option<FieldOptions>,
    pub matrix: Option<i32>,
}

impl SourceConfig {
    fn name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }
    /// Sources are shared between configs that only differ in matrix (conversion-time setting).
    fn same_source(&self, other: &SourceConfig) -> bool {
        self.path == other.path && self.field == other.field
    }
}

pub enum Request {
    /// Open these sources (the first is the reference). Replaces any previous set.
    Configure(Vec<SourceConfig>),
    /// Decode reference frame `ref_frame` and the matching frame of every other source.
    Frame { ref_frame: i32, offsets: Vec<(f64, i32)>, max_width: u32, stretch: bool, crop: bool },
    /// Suggest an offset for source `index` against the reference.
    Suggest { index: usize },
}

pub struct PreviewImage {
    pub width: usize,
    pub height: usize,
    pub rgb: Vec<u8>,
    /// Frame number in this source, and its picture type.
    pub frame: i32,
    pub pict_type: char,
    pub native: (u32, u32),
}

pub enum Reply {
    Status(String),
    Ready { ref_frames: i32, ref_times: Vec<f64> },
    Images { ref_frame: i32, images: Vec<Result<PreviewImage, String>> },
    Suggestion { index: usize, result: Result<SyncSuggestion, String> },
    Error(String),
}

/// Everything the worker thread receives: UI requests and results from opener threads.
enum Msg {
    Req(Request),
    Opened { cfg: SourceConfig, result: Result<AnySource, String> },
    Progress { cfg: SourceConfig, text: String },
}

pub struct Worker {
    tx: Sender<Msg>,
    pub rx: Receiver<Reply>,
}

impl Worker {
    pub fn spawn(repaint: impl Fn() + Send + Sync + 'static) -> Self {
        let (tx, msg_rx) = mpsc::channel::<Msg>();
        let (rep_tx, rx) = mpsc::channel::<Reply>();
        let repaint: Arc<dyn Fn() + Send + Sync> = Arc::new(repaint);
        let tx2 = tx.clone();
        thread::Builder::new()
            .name("preview".into())
            .spawn(move || run(msg_rx, tx2, rep_tx, repaint))
            .expect("spawn preview worker");
        Self { tx, rx }
    }

    pub fn send(&self, r: Request) {
        let _ = self.tx.send(Msg::Req(r));
    }
}

/// Opens (indexes, field-analyses) one source on its own thread.
fn spawn_opener(cfg: SourceConfig, tx: Sender<Msg>, repaint: Arc<dyn Fn() + Send + Sync>) {
    thread::Builder::new()
        .name("preview-open".into())
        .spawn(move || {
            let name = cfg.name();
            let progress = |text: String| {
                let _ = tx.send(Msg::Progress { cfg: cfg.clone(), text });
                repaint();
            };
            progress(format!("Opening {name}…"));
            let opts = OpenOptions { index_cache: source::default_cache_dir(), ..Default::default() };
            let result = VideoSource::open_with_progress(&cfg.path, &opts, |d, t| {
                if t > 0 && d % (t / 20).max(1) < (t / 200).max(1) {
                    progress(format!("Indexing {name}: {}%", d * 100 / t));
                }
                true
            })
            .map_err(|e| e.to_string())
            .and_then(|vs| match cfg.field {
                None => Ok(AnySource::Plain(vs)),
                Some(fo) => FilteredSource::new(vs, fo, source::default_cache_dir().as_deref(), |d, t| {
                    if t > 0 && d % (t / 20).max(1) < 64 {
                        progress(format!("Field analysis {name}: {}%", d * 100 / t));
                    }
                    true
                })
                .map(|f| AnySource::Filtered(Box::new(f)))
                .map_err(|e| e.to_string()),
            });
            let _ = tx.send(Msg::Opened { cfg, result });
            repaint();
        })
        .expect("spawn opener");
}

/// Opened sources kept for quick switching between episodes.
const POOL_SIZE: usize = 16;

fn run(rx: Receiver<Msg>, self_tx: Sender<Msg>, tx: Sender<Reply>, repaint: Arc<dyn Fn() + Send + Sync>) {
    let mut pool: Vec<(SourceConfig, AnySource)> = Vec::new();
    let mut opening: Vec<SourceConfig> = Vec::new();
    let mut progress: Vec<(SourceConfig, String)> = Vec::new();
    let mut current: Vec<SourceConfig> = Vec::new();
    let mut ready = false;
    // Detected black borders per source (computed on first use).
    let mut crops: Vec<(SourceConfig, comp_core::crop::Crop)> = Vec::new();
    let send = |r: Reply| {
        let _ = tx.send(r);
        repaint();
    };
    // Status line: what the current episode is waiting for, else what's preparing in the background.
    let status_line = |current: &[SourceConfig], progress: &[(SourceConfig, String)], ready: bool| -> String {
        let fg: Vec<&str> =
            progress.iter().filter(|(c, _)| current.iter().any(|x| x.same_source(c))).map(|(_, t)| t.as_str()).collect();
        if !ready && !fg.is_empty() {
            return fg.join(" · ");
        }
        let bg: Vec<&str> =
            progress.iter().filter(|(c, _)| !current.iter().any(|x| x.same_source(c))).map(|(_, t)| t.as_str()).collect();
        if bg.is_empty() { String::new() } else { format!("Preparing in background: {}", bg.join(" · ")) }
    };

    loop {
        // Block for one message, then drain the queue keeping only the newest of each request kind.
        let Ok(first) = rx.recv() else { return };
        let mut configure = None;
        let mut frame = None;
        let mut suggest: Vec<usize> = Vec::new();
        let mut events = Vec::new();
        let mut take = |m: Msg| match m {
            Msg::Req(Request::Configure(c)) => {
                configure = Some(c);
                frame = None; // a frame request for the old set is stale
            }
            Msg::Req(f @ Request::Frame { .. }) => frame = Some(f),
            Msg::Req(Request::Suggest { index }) => {
                if !suggest.contains(&index) {
                    suggest.push(index);
                }
            }
            other => events.push(other),
        };
        take(first);
        loop {
            match rx.try_recv() {
                Ok(m) => take(m),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }

        if let Some(cfgs) = configure {
            current = cfgs;
            ready = false;
            for c in &current {
                let have = pool.iter().any(|(p, _)| p.same_source(c)) || opening.iter().any(|o| o.same_source(c));
                if !have {
                    opening.push(c.clone());
                    spawn_opener(c.clone(), self_tx.clone(), repaint.clone());
                }
            }
        }

        for ev in events {
            match ev {
                Msg::Progress { cfg, text } => {
                    match progress.iter_mut().find(|(c, _)| c.same_source(&cfg)) {
                        Some(p) => p.1 = text,
                        None => progress.push((cfg, text)),
                    }
                }
                Msg::Opened { cfg, result } => {
                    opening.retain(|o| !o.same_source(&cfg));
                    progress.retain(|(c, _)| !c.same_source(&cfg));
                    match result {
                        Ok(src) => {
                            pool.push((cfg, src));
                            // Evict the least recently used sources that aren't on screen.
                            while pool.len() > POOL_SIZE {
                                match pool.iter().position(|(p, _)| !current.iter().any(|c| c.same_source(p))) {
                                    Some(i) => {
                                        pool.remove(i);
                                    }
                                    None => break,
                                }
                            }
                        }
                        Err(e) => {
                            if current.iter().any(|c| c.same_source(&cfg)) {
                                send(Reply::Error(format!("{}: {e}", cfg.name())));
                            }
                        }
                    }
                }
                Msg::Req(_) => {}
            }
        }

        // The current episode becomes ready as soon as all its sources are open.
        if !ready && !current.is_empty() && current.iter().all(|c| pool.iter().any(|(p, _)| p.same_source(c))) {
            ready = true;
            let r = &pool.iter().find(|(p, _)| p.same_source(&current[0])).unwrap().1;
            send(Reply::Ready { ref_frames: r.info().num_frames, ref_times: r.times().to_vec() });
        }
        send(Reply::Status(status_line(&current, &progress, ready)));

        if !ready || (frame.is_none() && suggest.is_empty()) {
            continue;
        }
        // Take the current episode's sources out of the pool (in order), work, put them back at the
        // end (most recently used).
        let mut sources: Vec<(SourceConfig, AnySource)> = Vec::with_capacity(current.len());
        for c in &current {
            let i = pool.iter().position(|(p, _)| p.same_source(c)).unwrap();
            let (_, src) = pool.remove(i);
            sources.push((c.clone(), src));
        }

        if let Some(Request::Frame { ref_frame, offsets, max_width, stretch, crop }) = frame {
            let t = sources[0].1.times().get(ref_frame.max(0) as usize).copied().unwrap_or(0.0);
            let mut images = Vec::with_capacity(sources.len());
            for (i, (cfg, src)) in sources.iter_mut().enumerate() {
                let target = if i == 0 {
                    Some(ref_frame)
                } else {
                    let (os, of) = offsets.get(i).copied().unwrap_or((0.0, 0));
                    let n = src.info().num_frames;
                    src.frame_at_time(t + os).map(|f| f + of).filter(|f| (0..n).contains(f))
                };
                let c = if crop {
                    match crops.iter().find(|(cc, _)| cc.same_source(cfg)) {
                        Some((_, c)) => Some(*c),
                        None => {
                            send(Reply::Status("Detecting black borders…".into()));
                            let n = src.info().num_frames;
                            let c = comp_core::crop::detect(src, 0, n, 24).unwrap_or_default();
                            crops.push((cfg.clone(), c));
                            send(Reply::Status(status_line(&current, &progress, ready)));
                            Some(c)
                        }
                    }
                } else {
                    None
                };
                images.push(match target {
                    None => Err("no frame at this time (outside the source)".to_string()),
                    Some(f) => decode(src, cfg, f, max_width, stretch, c),
                });
            }
            send(Reply::Images { ref_frame, images });
        }

        for index in suggest {
            if index == 0 || index >= sources.len() {
                continue;
            }
            send(Reply::Status("Finding offset (matching scene cuts)…".into()));
            let (head, tail) = sources.split_at_mut(index);
            let result = suggest_offset(&mut head[0].1, &mut tail[0].1, &SyncOptions::default(), |_| true).map_err(|e| e.to_string());
            send(Reply::Status(status_line(&current, &progress, ready)));
            send(Reply::Suggestion { index, result });
        }

        pool.extend(sources);
    }
}

fn decode(
    src: &mut AnySource,
    cfg: &SourceConfig,
    n: i32,
    max_width: u32,
    stretch: bool,
    crop: Option<comp_core::crop::Crop>,
) -> Result<PreviewImage, String> {
    let sar = src.info().sar;
    let frame = src.frame(n).map_err(|e| e.to_string())?;
    let pict_type = frame.pict_type;
    let conv = ConvertOptions { apply_sar: stretch, matrix_override: cfg.matrix, crop, ..Default::default() };
    let img = convert::to_rgb(&frame, sar, &conv).map_err(|e| e.to_string())?;
    let native = (img.width, img.height);
    let (w, h, rgb) = downscale(img.width as usize, img.height as usize, &img.data, max_width as usize);
    Ok(PreviewImage { width: w, height: h, rgb, frame: n, pict_type, native })
}

/// Area-average downscale by an integer factor so the width fits `max_w` (display only).
fn downscale(w: usize, h: usize, rgb: &[u8], max_w: usize) -> (usize, usize, Vec<u8>) {
    let f = w.div_ceil(max_w.max(1)).max(1);
    if f == 1 {
        return (w, h, rgb.to_vec());
    }
    let (ow, oh) = (w / f, h / f);
    let mut out = vec![0u8; ow * oh * 3];
    let area = (f * f) as u32;
    for oy in 0..oh {
        for ox in 0..ow {
            let mut acc = [0u32; 3];
            for dy in 0..f {
                let row = ((oy * f + dy) * w + ox * f) * 3;
                for dx in 0..f {
                    let p = row + dx * 3;
                    acc[0] += rgb[p] as u32;
                    acc[1] += rgb[p + 1] as u32;
                    acc[2] += rgb[p + 2] as u32;
                }
            }
            let o = (oy * ow + ox) * 3;
            out[o] = (acc[0] / area) as u8;
            out[o + 1] = (acc[1] / area) as u8;
            out[o + 2] = (acc[2] / area) as u8;
        }
    }
    (ow, oh, out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use comp_core::filter::{FieldMode, FieldOrder};
    use std::path::Path;
    use std::time::{Duration, Instant};

    fn corpus(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/corpus").join(name)
    }

    fn cfg(name: &str, ivtc: bool) -> SourceConfig {
        SourceConfig {
            path: corpus(name),
            field: ivtc.then_some(FieldOptions { mode: FieldMode::Ivtc, order: FieldOrder::Auto }),
            matrix: None,
        }
    }

    /// Waits for the next Ready (skipping status/progress) up to `limit`.
    fn wait_ready(w: &Worker, limit: Duration) -> Option<Duration> {
        let start = Instant::now();
        while start.elapsed() < limit {
            match w.rx.recv_timeout(Duration::from_millis(50)) {
                Ok(Reply::Ready { .. }) => return Some(start.elapsed()),
                Ok(Reply::Error(e)) => panic!("{e}"),
                _ => {}
            }
        }
        None
    }

    #[test]
    fn switching_back_to_an_open_episode_does_not_wait_for_background_analysis() {
        if !corpus("cuts_late_telecined.mkv").exists() {
            eprintln!("SKIP: corpus missing");
            return;
        }
        // Fresh cache so the IVTC analysis really runs (and the user's cache isn't touched).
        let cache = std::env::temp_dir().join(format!("anime-comp-preview-test-{}", std::process::id()));
        std::env::set_var("XDG_CACHE_HOME", &cache);

        let w = Worker::spawn(|| {});
        let ep1 = vec![cfg("h264_opengop.mkv", false), cfg("hevc_10bit.mkv", false)];
        w.send(Request::Configure(ep1.clone()));
        wait_ready(&w, Duration::from_secs(30)).expect("episode 1 ready");

        // Episode 2 needs a multi-second IVTC analysis (1900 telecined frames)…
        let ep2 = vec![cfg("cuts_ref.mkv", false), cfg("cuts_late_telecined.mkv", true)];
        w.send(Request::Configure(ep2.clone()));
        std::thread::sleep(Duration::from_millis(300));
        // …but switching back to episode 1 is immediate.
        w.send(Request::Configure(ep1));
        let back = wait_ready(&w, Duration::from_secs(5)).expect("episode 1 ready again");
        eprintln!("back to episode 1 in {back:?}");
        assert!(back < Duration::from_secs(1), "{back:?}");

        // Episode 2 finishes in the background; switching to it later is then quick too.
        let t = Instant::now();
        loop {
            w.send(Request::Configure(ep2.clone()));
            if let Some(d) = wait_ready(&w, Duration::from_millis(500)) {
                eprintln!("episode 2 ready {:?} after switching back (waited {d:?} on the last try)", t.elapsed());
                break;
            }
            assert!(t.elapsed() < Duration::from_secs(120), "episode 2 never finished");
            w.send(Request::Configure(vec![cfg("h264_opengop.mkv", false), cfg("hevc_10bit.mkv", false)]));
            let _ = wait_ready(&w, Duration::from_secs(2));
        }
        let _ = std::fs::remove_dir_all(cache);
    }
}
