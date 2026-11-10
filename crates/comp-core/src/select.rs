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
    fn stratified_short_ranges() {
        assert_eq!(stratified((5, 8), 10, 1), vec![5, 6, 7]);
        assert!(stratified((5, 5), 10, 1).is_empty());
    }
}
