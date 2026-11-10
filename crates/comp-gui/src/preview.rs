//! Background worker that decodes aligned preview frames for the sync view.
//!
//! The UI sends [`Request`]s; the worker coalesces them (only the newest configure / frame
//! request matters while the user drags a slider) and answers with [`Reply`]s.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;

use comp_core::convert::{self, ConvertOptions};
use comp_core::filter::{AnySource, FieldOptions, FilteredSource};
use comp_core::source::{self, OpenOptions, VideoSource};

#[derive(Debug, Clone, PartialEq)]
pub struct SourceConfig {
    pub path: PathBuf,
    pub field: Option<FieldOptions>,
    pub matrix: Option<i32>,
}

pub enum Request {
    /// Open these sources (the first is the reference). Replaces any previous set.
    Configure(Vec<SourceConfig>),
    /// Decode reference frame `ref_frame` and the matching frame of every other source.
    Frame { ref_frame: i32, offsets: Vec<(f64, i32)>, max_width: u32, stretch: bool },
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
    Error(String),
}

pub struct Worker {
    tx: Sender<Request>,
    pub rx: Receiver<Reply>,
}

impl Worker {
    pub fn spawn(repaint: impl Fn() + Send + 'static) -> Self {
        let (tx, req_rx) = mpsc::channel::<Request>();
        let (rep_tx, rx) = mpsc::channel::<Reply>();
        thread::Builder::new()
            .name("preview".into())
            .spawn(move || run(req_rx, rep_tx, repaint))
            .expect("spawn preview worker");
        Self { tx, rx }
    }

    pub fn send(&self, r: Request) {
        let _ = self.tx.send(r);
    }
}

fn run(rx: Receiver<Request>, tx: Sender<Reply>, repaint: impl Fn()) {
    let mut sources: Vec<(SourceConfig, AnySource)> = Vec::new();
    let send = |r: Reply| {
        let _ = tx.send(r);
        repaint();
    };
    loop {
        // Block for one request, then drain the queue keeping only the newest of each kind.
        let Ok(first) = rx.recv() else { return };
        let mut configure = None;
        let mut frame = None;
        let mut take = |r: Request| match r {
            Request::Configure(c) => {
                configure = Some(c);
                frame = None; // a frame request for the old set is stale
            }
            f @ Request::Frame { .. } => frame = Some(f),
        };
        take(first);
        loop {
            match rx.try_recv() {
                Ok(r) => take(r),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }

        if let Some(cfgs) = configure {
            // Keep sources whose config didn't change; open the rest.
            let mut old = std::mem::take(&mut sources);
            let mut failed = None;
            for (i, c) in cfgs.into_iter().enumerate() {
                if let Some(pos) = old.iter().position(|(oc, _)| *oc == c) {
                    sources.push(old.remove(pos));
                    continue;
                }
                let name = c.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                send(Reply::Status(format!("Opening {name}…")));
                let opts = OpenOptions { index_cache: source::default_cache_dir(), ..Default::default() };
                let opened = VideoSource::open_with_progress(&c.path, &opts, |d, t| {
                    if t > 0 && d % (t / 20).max(1) < (t / 200).max(1) {
                        send(Reply::Status(format!("Indexing {name}: {}%", d * 100 / t)));
                    }
                    true
                })
                .map_err(|e| e.to_string())
                .and_then(|vs| match c.field {
                    None => Ok(AnySource::Plain(vs)),
                    Some(fo) => FilteredSource::new(vs, fo, source::default_cache_dir().as_deref(), |d, t| {
                        if t > 0 && d % (t / 20).max(1) < 64 {
                            send(Reply::Status(format!("Field analysis {name}: {}%", d * 100 / t)));
                        }
                        true
                    })
                    .map(|f| AnySource::Filtered(Box::new(f)))
                    .map_err(|e| e.to_string()),
                });
                match opened {
                    Ok(s) => sources.push((c, s)),
                    Err(e) => {
                        failed = Some(format!("source {}: {e}", i + 1));
                        break;
                    }
                }
            }
            match failed {
                Some(e) => {
                    sources.clear();
                    send(Reply::Error(e));
                }
                None => {
                    if let Some((_, r)) = sources.first() {
                        send(Reply::Ready { ref_frames: r.info().num_frames, ref_times: r.times().to_vec() });
                    }
                }
            }
        }

        if let Some(Request::Frame { ref_frame, offsets, max_width, stretch }) = frame {
            if sources.is_empty() {
                continue;
            }
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
                images.push(match target {
                    None => Err("no frame at this time (outside the source)".to_string()),
                    Some(f) => decode(src, cfg, f, max_width, stretch),
                });
            }
            send(Reply::Images { ref_frame, images });
        }
    }
}

fn decode(src: &mut AnySource, cfg: &SourceConfig, n: i32, max_width: u32, stretch: bool) -> Result<PreviewImage, String> {
    let sar = src.info().sar;
    let frame = src.frame(n).map_err(|e| e.to_string())?;
    let pict_type = frame.pict_type;
    let conv = ConvertOptions { apply_sar: stretch, matrix_override: cfg.matrix, ..Default::default() };
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
