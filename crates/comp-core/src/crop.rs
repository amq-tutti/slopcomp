//! Black border (pillarbox / letterbox) detection.
//!
//! Frames spread over the episode are scanned; for every column and row the brightest mean luma
//! seen in any sample is kept. Columns/rows that stay black in *every* sample are borders. Taking
//! the maximum over many frames means dark scenes never cause over-cropping.

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::filter::AnySource;
use crate::source::Frame;

/// Pixels to remove from each side, in source (decoded) pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Crop {
    pub left: u32,
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
}

impl Crop {
    pub fn is_none(&self) -> bool {
        *self == Crop::default()
    }
}

/// Luma above this (0..1 between black and white level) counts as picture.
const THRESHOLD: f32 = 0.06;

fn norm_luma(f: &Frame<'_>, x: usize, row: &[u8]) -> f32 {
    let v = if f.layout.bytes_per_sample == 2 { u16::from_le_bytes([row[2 * x], row[2 * x + 1]]) as f32 } else { row[x] as f32 };
    let scale = ((1u32 << f.layout.bit_depth) - 1) as f32 / 255.0;
    (v / scale - 16.0) / 219.0
}

/// Accumulates per-column and per-row maxima of mean luma over frames.
#[derive(Default)]
pub struct Detector {
    cols: Vec<f32>,
    rows: Vec<f32>,
    frames: usize,
}

impl Detector {
    pub fn add(&mut self, f: &Frame<'_>) {
        let (w, h) = (f.width as usize, f.height as usize);
        if self.cols.len() != w || self.rows.len() != h {
            self.cols = vec![f32::MIN; w];
            self.rows = vec![f32::MIN; h];
        }
        let mut col_sum = vec![0f32; w];
        let ystep = 2;
        let mut nrows = 0;
        for y in (0..h).step_by(ystep) {
            let row = f.row(0, y);
            let mut row_sum = 0f32;
            for (x, cs) in col_sum.iter_mut().enumerate() {
                let v = norm_luma(f, x, row);
                *cs += v;
                row_sum += v;
            }
            let rm = row_sum / w as f32;
            // Rows we skipped take the value of the row above.
            for yy in y..(y + ystep).min(h) {
                self.rows[yy] = self.rows[yy].max(rm);
            }
            nrows += 1;
        }
        for (c, s) in self.cols.iter_mut().zip(col_sum) {
            *c = c.max(s / nrows as f32);
        }
        self.frames += 1;
    }

    pub fn result(&self) -> Crop {
        let (w, h) = (self.cols.len(), self.rows.len());
        if self.frames == 0 || w == 0 || h == 0 {
            return Crop::default();
        }
        let span = |v: &[f32]| -> (usize, usize) {
            let first = v.iter().position(|&m| m > THRESHOLD);
            let last = v.iter().rposition(|&m| m > THRESHOLD);
            match (first, last) {
                (Some(a), Some(b)) => (a, v.len() - 1 - b),
                _ => (0, 0),
            }
        };
        let (l, r) = span(&self.cols);
        let (t, b) = span(&self.rows);
        // Refuse implausible crops (most of the frame black in every sample, e.g. an all-dark
        // clip): keep at least 40% of each dimension.
        if w - l - r < w * 2 / 5 || h - t - b < h * 2 / 5 {
            return Crop::default();
        }
        Crop { left: l as u32, top: t as u32, right: r as u32, bottom: b as u32 }
    }
}

/// Detects borders from `samples` frames spread evenly over `[a, b)` of `src`.
pub fn detect(src: &mut AnySource, a: i32, b: i32, samples: usize) -> Result<Crop> {
    let mut d = Detector::default();
    let n = samples.max(1) as i64;
    let len = (b - a).max(1) as i64;
    for i in 0..n {
        let k = a + (len * (2 * i + 1) / (2 * n)) as i32;
        let f = src.frame(k)?;
        d.add(&f);
    }
    Ok(d.result())
}
