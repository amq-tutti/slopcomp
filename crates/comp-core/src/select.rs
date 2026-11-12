//! Frame selection on the reference timeline.
//!
//! The runner maps every reference frame to a frame in each source (by timestamp plus offsets).
//! Only reference frames that map to a valid frame in *every* source can be picked.

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

/// Half-open range `[start, end)` spanning the first to last reference frame that is valid in
/// all sources (`valid[r]`), or `None` if there are none.
pub fn usable_range(valid: &[bool]) -> Option<(i32, i32)> {
    let start = valid.iter().position(|&v| v)?;
    let end = valid.iter().rposition(|&v| v)? + 1;
    Some((start as i32, end as i32))
}

/// Picks `count` reference frames from `[start, end)`.
///
/// The range is split into `count` equal bins, with one uniformly random frame per bin, so picks
/// cover the whole episode. A given seed always gives the same frames. Returns ascending, unique
/// frame numbers. Fewer are returned if the range is shorter than `count`.
pub fn stratified(range: (i32, i32), count: usize, seed: u64) -> Vec<i32> {
    let (start, end) = range;
    let len = (end - start).max(0) as i64;
    if len == 0 || count == 0 {
        return vec![];
    }
    let count = (count as i64).min(len);
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut out: Vec<i32> = (0..count)
        .map(|i| {
            let lo = start as i64 + len * i / count;
            let hi = start as i64 + len * (i + 1) / count; // exclusive, > lo since count <= len
            rng.gen_range(lo..hi) as i32
        })
        .collect();
    out.dedup();
    out
}

/// Sorted, disjoint half-open frame ranges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segments(pub Vec<(i32, i32)>);

impl Segments {
    /// `range` minus every `excluded` range.
    pub fn from_range_minus(range: (i32, i32), excluded: &[(i32, i32)]) -> Self {
        let mut ex: Vec<(i32, i32)> = excluded.iter().copied().filter(|(a, b)| b > a).collect();
        ex.sort_unstable();
        let mut out = Vec::new();
        let mut cur = range.0;
        for (a, b) in ex {
            if b <= cur {
                continue;
            }
            if a >= range.1 {
                break;
            }
            if a > cur {
                out.push((cur, a.min(range.1)));
            }
            cur = cur.max(b);
        }
        if cur < range.1 {
            out.push((cur, range.1));
        }
        Segments(out)
    }

    pub fn len(&self) -> i64 {
        self.0.iter().map(|(a, b)| (b - a) as i64).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Position of frame `f` in the concatenated segments, if allowed.
    pub fn index_of(&self, f: i32) -> Option<i64> {
        let mut base = 0i64;
        for &(a, b) in &self.0 {
            if f >= a && f < b {
                return Some(base + (f - a) as i64);
            }
            base += (b - a) as i64;
        }
        None
    }

    /// The `i`-th allowed frame (0-based over the concatenated segments).
    pub fn nth(&self, mut i: i64) -> i32 {
        for &(a, b) in &self.0 {
            let n = (b - a) as i64;
            if i < n {
                return a + i as i32;
            }
            i -= n;
        }
        panic!("Segments::nth out of range");
    }
}

/// Outcome of judging a candidate frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Accept,
    /// Rejected, with a reason and a severity. When every try in a bin is rejected, the
    /// candidate with the lowest severity is used, but only if it's below [`HARD`]. Hard
    /// rejections (black screens, scene changes) are never used. Their bins are refilled from the
    /// largest gaps between picks instead.
    Reject { reason: &'static str, severity: u8 },
}

/// Rejections at or above this severity are never used as fallbacks.
pub const HARD: u8 = 3;

#[derive(Debug, Clone, Default)]
pub struct PickReport {
    pub frames: Vec<i32>,
    /// Every rejected candidate with its reason.
    pub rejected: Vec<(i32, &'static str)>,
    /// Frames used even though they were rejected (no acceptable candidate in their bin).
    pub fallbacks: Vec<(i32, &'static str)>,
}

/// Picks `count` frames from `allowed`, one per equal-sized bin, re-rolling rejected candidates
/// up to `max_tries` times per bin. `judge(candidate, picked_so_far)` decides. Deterministic for a
/// given seed and judge. Returns ascending frames.
pub fn pick<E, F>(allowed: &Segments, count: usize, seed: u64, max_tries: usize, mut judge: F) -> Result<PickReport, E>
where
    F: FnMut(i32, &[i32]) -> Result<Verdict, E>,
{
    let len = allowed.len();
    let mut rep = PickReport::default();
    if len == 0 || count == 0 {
        return Ok(rep);
    }
    let count = (count as i64).min(len);
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    // One attempt over the allowed-index interval [lo, hi). Returns the accepted frame, or the
    // best soft fallback.
    let mut attempt = |lo: i64, hi: i64, rep: &mut PickReport| -> Result<Option<(i32, Option<&'static str>)>, E> {
        let mut best: Option<(u8, i32, &'static str)> = None;
        let mut tried = Vec::new();
        // `max_tries` distinct candidates (or the whole bin, if smaller): drawing a frame that was
        // already judged doesn't use up a try, so small bins are searched completely.
        while tried.len() < max_tries.max(1) && (tried.len() as i64) < hi - lo {
            let cand = allowed.nth(rng.gen_range(lo..hi));
            if tried.contains(&cand) {
                continue;
            }
            tried.push(cand);
            match judge(cand, &rep.frames)? {
                Verdict::Accept => return Ok(Some((cand, None))),
                Verdict::Reject { reason, severity } => {
                    rep.rejected.push((cand, reason));
                    if severity < HARD && best.is_none_or(|(s, _, _)| severity < s) {
                        best = Some((severity, cand, reason));
                    }
                }
            }
        }
        Ok(best.map(|(_, c, r)| (c, Some(r))))
    };

    for i in 0..count {
        let lo = len * i / count;
        let hi = len * (i + 1) / count;
        if let Some((c, fallback)) = attempt(lo, hi, &mut rep)? {
            rep.frames.push(c);
            if let Some(r) = fallback {
                rep.fallbacks.push((c, r));
            }
        }
    }

    // Fill pass: bins that only had hard rejections (e.g. entirely inside a black section) get a
    // replacement from the largest remaining gap between picks.
    let mut dead_gaps: Vec<(i64, i64)> = Vec::new();
    while (rep.frames.len() as i64) < count {
        let mut idx: Vec<i64> = rep.frames.iter().filter_map(|&f| allowed.index_of(f)).collect();
        idx.sort_unstable();
        let mut gaps: Vec<(i64, i64)> = Vec::new();
        let mut prev = -1i64;
        for &x in idx.iter().chain(std::iter::once(&len)) {
            if x - prev > 1 {
                gaps.push((prev + 1, x));
            }
            prev = x;
        }
        gaps.retain(|g| !dead_gaps.contains(g));
        let Some(&(lo, hi)) = gaps.iter().max_by_key(|(a, b)| b - a) else { break };
        // Prefer the middle half of the gap so the new pick stays apart from its neighbours.
        let q = (hi - lo) / 4;
        let (mlo, mhi) = if hi - lo >= 8 { (lo + q, hi - q) } else { (lo, hi) };
        match attempt(mlo, mhi, &mut rep)? {
            Some((c, fallback)) => {
                rep.frames.push(c);
                if let Some(r) = fallback {
                    rep.fallbacks.push((c, r));
                }
            }
            None => dead_gaps.push((lo, hi)),
        }
    }
    rep.frames.sort_unstable();
    rep.frames.dedup();
    Ok(rep)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usable_range_spans_valid_frames() {
        assert_eq!(usable_range(&[false, true, true, false, true, false]), Some((1, 5)));
        assert_eq!(usable_range(&[false, false]), None);
        assert_eq!(usable_range(&[]), None);
    }

    #[test]
    fn stratified_is_deterministic_spread_and_in_range() {
        let a = stratified((100, 34_000), 20, 42);
        let b = stratified((100, 34_000), 20, 42);
        assert_eq!(a, b);
        assert_eq!(a.len(), 20);
        assert!(a.windows(2).all(|w| w[0] < w[1]));
        let bin = (34_000 - 100) / 20;
        for (i, &f) in a.iter().enumerate() {
            assert!(f >= 100 + bin * i as i32 && f < 100 + bin * (i as i32 + 1) + 1, "frame {f} outside bin {i}");
        }
        assert_ne!(a, stratified((100, 34_000), 20, 43));
    }

    #[test]
    fn segments_exclude_ranges() {
        let s = Segments::from_range_minus((0, 100), &[(10, 20), (15, 30), (90, 200), (-5, 2)]);
        assert_eq!(s.0, vec![(2, 10), (30, 90)]);
        assert_eq!(s.len(), 68);
        assert_eq!(s.nth(0), 2);
        assert_eq!(s.nth(7), 9);
        assert_eq!(s.nth(8), 30);
        assert_eq!(s.nth(67), 89);
    }

    #[test]
    fn pick_rerolls_and_falls_back() {
        let allowed = Segments(vec![(0, 1000)]);
        // Reject odd frames: all picks must be even.
        let rep = pick::<(), _>(&allowed, 10, 1, 20, |c, _| {
            Ok(if c % 2 == 1 { Verdict::Reject { reason: "odd", severity: 1 } } else { Verdict::Accept })
        })
        .unwrap();
        assert_eq!(rep.frames.len(), 10);
        assert!(rep.frames.iter().all(|f| f % 2 == 0));
        assert!(rep.fallbacks.is_empty());
        // Soft rejections everywhere: still one frame per bin, all fallbacks.
        let rep = pick::<(), _>(&allowed, 5, 1, 4, |_, _| Ok(Verdict::Reject { reason: "type", severity: 1 })).unwrap();
        assert_eq!(rep.frames.len(), 5);
        assert_eq!(rep.fallbacks.len(), 5);
        // Hard rejections never become fallbacks: a "black" stretch over 0..400 gets no picks, and
        // its bins are refilled elsewhere.
        let rep = pick::<(), _>(&allowed, 10, 1, 8, |c, _| {
            Ok(if c < 400 { Verdict::Reject { reason: "dark", severity: 4 } } else { Verdict::Accept })
        })
        .unwrap();
        assert!(rep.frames.iter().all(|&f| f >= 400), "{:?}", rep.frames);
        assert_eq!(rep.frames.len(), 10, "{:?}", rep.frames);
        assert!(rep.fallbacks.is_empty());
        // Everything hard-rejected: nothing picked, and no infinite loop.
        let rep = pick::<(), _>(&allowed, 5, 1, 3, |_, _| Ok(Verdict::Reject { reason: "dark", severity: 4 })).unwrap();
        assert!(rep.frames.is_empty());
        // Deterministic.
        let a = pick::<(), _>(&allowed, 8, 9, 5, |_, _| Ok(Verdict::Accept)).unwrap().frames;
        let b = pick::<(), _>(&allowed, 8, 9, 5, |_, _| Ok(Verdict::Accept)).unwrap().frames;
        assert_eq!(a, b);
        // Picks are spread one per bin over the allowed frames.
        for (i, f) in a.iter().enumerate() {
            assert!(*f >= 125 * i as i32 && *f < 125 * (i as i32 + 1));
        }
    }

    #[test]
    fn stratified_short_ranges() {
        assert_eq!(stratified((5, 8), 10, 1), vec![5, 6, 7]);
        assert!(stratified((5, 5), 10, 1).is_empty());
    }
}
