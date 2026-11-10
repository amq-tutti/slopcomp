//! Automatic offset suggestion between two sources.
//!
//! 1. Black borders are detected in each source (pillarbox/letterbox) and cropped away, so a
//!    pillarboxed BD can be matched against a 4:3 DVD.
//! 2. A window of the reference and a wider window of the other source (± the search range) are
//!    decoded into 16×16 luma thumbnails.
//! 3. Scene cuts are detected in both (sharp thumbnail changes that stand out from local motion).
//!    The time offset that lines up the most cuts wins.
//! 4. The winner is confirmed by the Pearson correlation of brightness-normalised thumbnails at that
//!    offset, which works across resolution, codec and colour differences.
//!
//! The result is a *time* offset (`offset_seconds` in [`crate::comp::SourceSpec`]) with a confidence
//! score. A low confidence means "check it in the sync view", not "apply it".

use crate::error::{Error, Result};
use crate::filter::AnySource;
use crate::source::Frame;

#[derive(Debug, Clone)]
pub struct SyncOptions {
    /// Start of the reference window in seconds (default: 10% into the reference).
    pub window_start: Option<f64>,
    /// Length of the reference window in seconds.
    pub window_len: f64,
    /// Largest offset searched, in seconds, either direction.
    pub search: f64,
}

impl Default for SyncOptions {
    fn default() -> Self {
        Self { window_start: None, window_len: 60.0, search: 15.0 }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SyncSuggestion {
    /// Other-source time = reference time + `offset_seconds`.
    pub offset_seconds: f64,
    /// 0..1. Above ~0.5 is a confident match; below ~0.3 it is probably wrong or the window has
    /// too little going on (check it in the sync view either way).
    pub confidence: f32,
    /// Mean thumbnail correlation at the suggested offset (−1..1).
    pub correlation: f32,
    /// Typical correlation at wrong offsets; confidence comes from how far `correlation` rises above it.
    pub baseline: f32,
    pub ref_cuts: usize,
    pub matched_cuts: usize,
    /// The offset is exact to ± this many frames (0 = frame-exact). Larger when neighbouring
    /// offsets match almost as well, e.g. static shots or blended/differently decimated frames.
    pub precision_frames: u32,
    /// How the offset was found ("scene cuts" or "correlation search").
    pub method: &'static str,
}

const GRID: usize = 16;

/// Fractional crop rectangle (x0, y0, x1, y1) in 0..1.
type Rect = (f32, f32, f32, f32);

struct Sig {
    t: f64,
    thumb: Vec<f32>,
}

fn sample(f: &Frame<'_>, x: usize, y: usize) -> f32 {
    let row = f.row(0, y);
    let v = if f.layout.bytes_per_sample == 2 { u16::from_le_bytes([row[2 * x], row[2 * x + 1]]) as f32 } else { row[x] as f32 };
    let scale = ((1u32 << f.layout.bit_depth) - 1) as f32 / 255.0;
    ((v / scale - 16.0) / 219.0).clamp(0.0, 1.0)
}

/// Box-averaged luma grid over `rect`.
fn luma_grid(f: &Frame<'_>, rect: Rect, gw: usize, gh: usize) -> Vec<f32> {
    let (w, h) = (f.width as usize, f.height as usize);
    let (x0, y0) = ((rect.0 * w as f32) as usize, (rect.1 * h as f32) as usize);
    let (x1, y1) = (((rect.2 * w as f32) as usize).max(x0 + gw).min(w), ((rect.3 * h as f32) as usize).max(y0 + gh).min(h));
    let (rw, rh) = (x1 - x0, y1 - y0);
    let step = ((rw / gw).min(rh / gh) / 6).max(1);
    let mut out = vec![0f32; gw * gh];
    for gy in 0..gh {
        let (ya, yb) = (y0 + gy * rh / gh, (y0 + (gy + 1) * rh / gh).max(y0 + gy * rh / gh + 1));
        for gx in 0..gw {
            let (xa, xb) = (x0 + gx * rw / gw, (x0 + (gx + 1) * rw / gw).max(x0 + gx * rw / gw + 1));
            let (mut s, mut n) = (0f32, 0u32);
            let mut y = ya;
            while y < yb {
                let mut x = xa;
                while x < xb {
                    s += sample(f, x, y);
                    n += 1;
                    x += step;
                }
                y += step;
            }
            out[gy * gw + gx] = s / n.max(1) as f32;
        }
    }
    out
}

/// Detects black borders from frames spread over `[a, b)`.
fn detect_crop(src: &mut AnySource, a: i32, b: i32) -> Result<Rect> {
    const GW: usize = 64;
    const GH: usize = 36;
    let mut col_max = [0f32; GW];
    let mut row_max = [0f32; GH];
    let n = 12;
    for i in 0..n {
        let k = a + ((b - a) as i64 * (2 * i + 1) / (2 * n)) as i32;
        let f = src.frame(k)?;
        let g = luma_grid(&f, (0.0, 0.0, 1.0, 1.0), GW, GH);
        for y in 0..GH {
            for x in 0..GW {
                col_max[x] = col_max[x].max(g[y * GW + x]);
                row_max[y] = row_max[y].max(g[y * GW + x]);
            }
        }
    }
    let active = |v: f32| v > 0.06;
    let span = |m: &[f32]| -> (usize, usize) {
        let first = m.iter().position(|&v| active(v)).unwrap_or(0);
        let last = m.iter().rposition(|&v| active(v)).map(|p| p + 1).unwrap_or(m.len());
        if last - first < m.len() / 3 { (0, m.len()) } else { (first, last) }
    };
    let (cx0, cx1) = span(&col_max);
    let (ry0, ry1) = span(&row_max);
    // Step one grid cell inwards from detected edges to avoid half-black boundary cells.
    let inset = |a: usize, b: usize, n: usize| -> (f32, f32) {
        let a2 = if a > 0 { a + 1 } else { a };
        let b2 = if b < n { b - 1 } else { b };
        (a2 as f32 / n as f32, b2 as f32 / n as f32)
    };
    let (x0, x1) = inset(cx0, cx1, GW);
    let (y0, y1) = inset(ry0, ry1, GH);
    Ok((x0, y0, x1, y1))
}

fn signatures<F: FnMut() -> bool>(src: &mut AnySource, a: i32, b: i32, rect: Rect, tick: &mut F) -> Result<Vec<Sig>> {
    let mut v = Vec::with_capacity((b - a).max(0) as usize);
    for k in a..b {
        if k % 32 == 0 && !tick() {
            return Err(Error::Cancelled);
        }
        let t = src.times()[k as usize];
        let f = src.frame(k)?;
        v.push(Sig { t, thumb: luma_grid(&f, rect, GRID, GRID) });
    }
    Ok(v)
}

fn mad(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum::<f32>() / a.len() as f32
}

/// Pearson correlation of two thumbnails (brightness/contrast invariant).
fn corr(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len() as f32;
    let (ma, mb) = (a.iter().sum::<f32>() / n, b.iter().sum::<f32>() / n);
    let (mut sab, mut saa, mut sbb) = (0f32, 0f32, 0f32);
    for (x, y) in a.iter().zip(b) {
        let (dx, dy) = (x - ma, y - mb);
        sab += dx * dy;
        saa += dx * dx;
        sbb += dy * dy;
    }
    if saa < 1e-6 || sbb < 1e-6 {
        // Flat frames: equal if both flat at similar brightness.
        return if (ma - mb).abs() < 0.05 { 1.0 } else { 0.0 };
    }
    sab / (saa * sbb).sqrt()
}

/// Times of scene cuts: frames whose change from the previous frame is large and stands out from
/// the surrounding motion.
fn cuts(sigs: &[Sig]) -> Vec<f64> {
    let d: Vec<f32> = std::iter::once(0.0).chain(sigs.windows(2).map(|w| mad(&w[0].thumb, &w[1].thumb))).collect();
    let mut out = Vec::new();
    for i in 1..d.len() {
        let lo = i.saturating_sub(12);
        let hi = (i + 13).min(d.len());
        let mut local: Vec<f32> = d[lo..hi].iter().enumerate().filter(|(j, _)| lo + j != i).map(|(_, &v)| v).collect();
        local.sort_by(|a, b| a.total_cmp(b));
        let median = local.get(local.len() / 2).copied().unwrap_or(0.0);
        let is_peak = (i.saturating_sub(2)..(i + 3).min(d.len())).all(|j| j == i || d[j] < d[i]);
        if d[i] > 0.06 && d[i] > 3.0 * median + 0.02 && is_peak {
            out.push(sigs[i].t);
        }
    }
    out
}

fn median_step(sigs: &[Sig]) -> f64 {
    let mut d: Vec<f64> = sigs.windows(2).map(|w| w[1].t - w[0].t).collect();
    d.sort_by(|a, b| a.total_cmp(b));
    d.get(d.len() / 2).copied().unwrap_or(1.0 / 24.0)
}

/// Mean correlation between reference signatures and the other source's signature nearest to
/// `t + offset` (within `tol`). Uses up to 80 evenly spaced reference samples.
fn mean_corr(rs: &[Sig], os: &[Sig], offset: f64, tol: f64) -> (f32, usize) {
    mean_corr_n(rs, os, offset, tol, 80)
}

fn mean_corr_n(rs: &[Sig], os: &[Sig], offset: f64, tol: f64, samples: usize) -> (f32, usize) {
    let step = (rs.len() / samples.max(1)).max(1);
    let (mut sum, mut n) = (0f32, 0usize);
    for r in rs.iter().step_by(step) {
        let t = r.t + offset;
        let i = os.partition_point(|o| o.t < t);
        let best = [i.wrapping_sub(1), i]
            .into_iter()
            .filter_map(|j| os.get(j))
            .min_by(|a, b| (a.t - t).abs().total_cmp(&(b.t - t).abs()));
        if let Some(o) = best.filter(|o| (o.t - t).abs() <= tol) {
            sum += corr(&r.thumb, &o.thumb);
            n += 1;
        }
    }
    (if n > 0 { sum / n as f32 } else { 0.0 }, n)
}

/// Suggests `offset_seconds` for `other` relative to `reference`.
/// `progress(fraction)` is called while decoding. Return `false` to cancel.
pub fn suggest_offset<P: FnMut(f32) -> bool>(
    reference: &mut AnySource,
    other: &mut AnySource,
    opts: &SyncOptions,
    mut progress: P,
) -> Result<SyncSuggestion> {
    let (rt, ot) = (reference.times().to_vec(), other.times().to_vec());
    let (r_dur, o_dur) = (*rt.last().unwrap_or(&0.0), *ot.last().unwrap_or(&0.0));
    let len = opts.window_len.min(r_dur * 0.8).max(5.0);
    let start = opts.window_start.unwrap_or(r_dur * 0.1).clamp(0.0, (r_dur - len).max(0.0));
    let ra = rt.partition_point(|&t| t < start) as i32;
    let rb = rt.partition_point(|&t| t < start + len) as i32;
    let oa = ot.partition_point(|&t| t < (start - opts.search).max(0.0)) as i32;
    let ob = ot.partition_point(|&t| t < (start + len + opts.search).min(o_dur + 1.0)) as i32;
    if rb - ra < 24 || ob - oa < 24 {
        return Err(Error::Spec("sources are too short to suggest an offset".into()));
    }

    let r_crop = detect_crop(reference, ra, rb)?;
    let o_crop = detect_crop(other, oa, ob)?;
    let total = ((rb - ra) + (ob - oa)) as f32;
    let mut done = 0f32;
    let rs = signatures(reference, ra, rb, r_crop, &mut || {
        done += 32.0;
        progress(done / total)
    })?;
    let os = signatures(other, oa, ob, o_crop, &mut || {
        done += 32.0;
        progress(done / total)
    })?;

    let tol = 0.75 * median_step(&rs).max(median_step(&os));
    let (rc, oc) = (cuts(&rs), cuts(&os));

    // Candidate offsets from all cut pairs; score = reference cuts with a partner within tol.
    let mut best: Option<(usize, f64, f64)> = None; // (matches, -mean residual, offset)
    for &r in &rc {
        for &o in &oc {
            let d = o - r;
            if d.abs() > opts.search {
                continue;
            }
            let mut matched = 0;
            let mut resid = 0.0;
            for &r2 in &rc {
                let t = r2 + d;
                let i = oc.partition_point(|&x| x < t - tol);
                if let Some(&o2) = oc.get(i).filter(|&&o2| (o2 - t).abs() <= tol) {
                    matched += 1;
                    resid += (o2 - t).abs();
                }
            }
            let mean_resid = if matched > 0 { resid / matched as f64 } else { f64::MAX };
            let better = match best {
                None => true,
                Some((m, nr, bd)) => matched > m || (matched == m && (-mean_resid > nr || (-mean_resid == nr && d.abs() < bd.abs()))),
            };
            if better {
                best = Some((matched, -mean_resid, d));
            }
        }
    }

    let enough_cuts = rc.len() >= 3;
    let (offset, matched, method) = match best {
        Some((m, _, d)) if enough_cuts && m >= 2 => {
            // Refine: mean residual of the matched pairs.
            let mut adj = Vec::new();
            for &r2 in &rc {
                let t = r2 + d;
                let i = oc.partition_point(|&x| x < t - tol);
                if let Some(&o2) = oc.get(i).filter(|&&o2| (o2 - t).abs() <= tol) {
                    adj.push(o2 - r2);
                }
            }
            adj.sort_by(|a, b| a.total_cmp(b));
            (adj[adj.len() / 2], m, "scene cuts")
        }
        _ => {
            // Few cuts: brute-force correlation over the other source's frame grid.
            let step = median_step(&os);
            let n = (opts.search / step).ceil() as i64;
            let mut best = (f32::MIN, 0.0);
            for k in -n..=n {
                let d = k as f64 * step;
                let (c, cnt) = mean_corr(&rs, &os, d, tol);
                if cnt >= 10 && c > best.0 {
                    best = (c, d);
                }
            }
            (best.1, 0, "correlation search")
        }
    };
    // Fine search: the cut vote is only accurate to about a frame (more where frame grids differ,
    // e.g. VFR 29.97 sections against a decimated 23.976 encode). Score every offset within ±3
    // frames of the voted one, and of zero, on all frames. During motion the exact alignment wins.
    // Ties go to the offset closer to zero.
    let ostep = median_step(&os);
    let mut cands: Vec<f64> = Vec::new();
    for base in [offset, 0.0] {
        for k in -3..=3 {
            let d = base + k as f64 * ostep;
            if d.abs() <= opts.search && !cands.iter().any(|c| (c - d).abs() < ostep * 0.25) {
                cands.push(d);
            }
        }
    }
    let mut fine = (f32::MIN, offset);
    let mut scored: Vec<(f64, f32)> = Vec::new();
    for d in cands {
        let (c, n) = mean_corr_n(&rs, &os, d, tol, usize::MAX);
        if n < 10 {
            continue;
        }
        scored.push((d, c));
        if c > fine.0 + 1e-4 || ((c - fine.0).abs() <= 1e-4 && d.abs() < fine.1.abs()) {
            fine = (c, d);
        }
    }
    let offset = fine.1;
    // Precision: neighbouring offsets that score almost as well (static shots, blended frames,
    // differently decimated releases) mean the last frame or two can't be pinned down.
    let near: Vec<f64> = scored.iter().filter(|(_, c)| *c >= fine.0 - 0.004).map(|(d, _)| *d).collect();
    let spread = near.iter().map(|d| ((d - offset) / ostep).abs().round() as u32).max().unwrap_or(0);
    let (correlation, _) = mean_corr(&rs, &os, offset, tol);
    // Baseline: correlation at deliberately wrong offsets (several seconds away, outside the same
    // shot). Matching content stands well above it; unrelated content doesn't.
    let mut wrong: Vec<f32> = (2..=9)
        .flat_map(|j| [offset + j as f64 * 1.37, offset - j as f64 * 1.37])
        .filter_map(|d| {
            let (c, n) = mean_corr(&rs, &os, d, tol);
            (n >= 10).then_some(c)
        })
        .collect();
    wrong.sort_by(|a, b| a.total_cmp(b));
    let baseline = wrong.get(wrong.len() / 2).copied().unwrap_or(0.0).max(0.0);
    let contrast = ((correlation - baseline) / (1.0 - baseline).max(0.05)).clamp(0.0, 1.0);
    // Scene cuts that agree (≥ 3, or at least half the reference cuts) confirm the match. Without
    // that, the correlation contrast alone is discounted.
    let cuts_agree = enough_cuts && (matched >= 3 || matched * 2 >= rc.len());
    let confidence = (if cuts_agree { contrast } else { contrast * 0.6 }).clamp(0.0, 1.0);
    // Snap tiny offsets (timestamp rounding) to zero.
    let offset = if offset.abs() < 0.002 { 0.0 } else { (offset * 1000.0).round() / 1000.0 };
    Ok(SyncSuggestion { offset_seconds: offset, confidence, correlation, baseline, ref_cuts: rc.len(), matched_cuts: matched, precision_frames: spread, method })
}
