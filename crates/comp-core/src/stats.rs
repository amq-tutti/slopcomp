//! Cheap per-frame statistics for frame selection: a small luma thumbnail, mean brightness,
//! detail, and a 64-bit difference hash for near-duplicate detection.

use crate::source::Frame;

/// Thumbnail edge length.
pub const THUMB: usize = 32;

#[derive(Debug, Clone)]
pub struct FrameStats {
    /// Mean luma normalised to 0..1 (0 = black level, 1 = white level for limited range).
    pub mean: f32,
    /// Standard deviation of the thumbnail (0..1). Near 0 = flat frame (solid colour, title card).
    pub detail: f32,
    /// 32×32 box-filtered luma, normalised 0..1, row-major.
    pub thumb: Vec<f32>,
    /// 64-bit dHash (9×8 horizontal gradient signs).
    pub dhash: u64,
}

impl FrameStats {
    pub fn of(f: &Frame<'_>) -> Self {
        let (w, h) = (f.width as usize, f.height as usize);
        let max = ((1u32 << f.layout.bit_depth) - 1) as f32;
        // Limited-range black/white levels scaled to bit depth. Full-range streams just end up
        // slightly stretched, which doesn't matter for these heuristics.
        let scale = max / 255.0;
        let (black, white) = (16.0 * scale, 235.0 * scale);
        let sample = |x: usize, y: usize| -> f32 {
            let row = f.row(0, y);
            if f.layout.bytes_per_sample == 2 {
                u16::from_le_bytes([row[2 * x], row[2 * x + 1]]) as f32
            } else {
                row[x] as f32
            }
        };
        // Box-average into an arbitrary grid, sampling every `step`-th pixel for speed.
        let grid = |gw: usize, gh: usize| -> Vec<f32> {
            let mut out = vec![0f32; gw * gh];
            let step = ((w / gw).min(h / gh) / 4).max(1);
            for gy in 0..gh {
                let (y0, y1) = (gy * h / gh, ((gy + 1) * h / gh).max(gy * h / gh + 1));
                for gx in 0..gw {
                    let (x0, x1) = (gx * w / gw, ((gx + 1) * w / gw).max(gx * w / gw + 1));
                    let (mut sum, mut n) = (0f32, 0u32);
                    let mut y = y0;
                    while y < y1 {
                        let mut x = x0;
                        while x < x1 {
                            sum += sample(x, y);
                            n += 1;
                            x += step;
                        }
                        y += step;
                    }
                    out[gy * gw + gx] = (((sum / n as f32) - black) / (white - black)).clamp(0.0, 1.0);
                }
            }
            out
        };
        let thumb = grid(THUMB, THUMB);
        let mean = thumb.iter().sum::<f32>() / thumb.len() as f32;
        let detail = (thumb.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / thumb.len() as f32).sqrt();
        let d = grid(9, 8);
        let mut dhash = 0u64;
        for y in 0..8 {
            for x in 0..8 {
                if d[y * 9 + x] < d[y * 9 + x + 1] {
                    dhash |= 1 << (y * 8 + x);
                }
            }
        }
        Self { mean, detail, thumb, dhash }
    }

    /// Mean absolute thumbnail difference (0..1).
    pub fn diff(&self, other: &FrameStats) -> f32 {
        self.thumb.iter().zip(&other.thumb).map(|(a, b)| (a - b).abs()).sum::<f32>() / self.thumb.len() as f32
    }

    pub fn hamming(&self, other: &FrameStats) -> u32 {
        (self.dhash ^ other.dhash).count_ones()
    }
}
