//! Lossless-as-possible conversion of decoded YUV frames to 8-bit RGB, plus PNG output.
//!
//! zimg does the conversion with the correct matrix, range and chroma siting. Missing metadata
//! falls back to resolution-based defaults (BT.601 for SD, BT.709 otherwise). Anamorphic sources can
//! optionally be stretched horizontally to their display aspect ratio in the same pass.

use std::ffi::CStr;
use std::fs::File;
use std::io::BufWriter;
use std::os::raw::c_char;
use std::path::Path;

use comp_sys as sys;

use crate::error::{Error, Result};
use crate::source::{ColorInfo, Frame};

/// Resampling kernel, used for chroma upsampling and anamorphic stretching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Kernel {
    Point,
    Bilinear,
    Bicubic,
    Spline16,
    #[default]
    Spline36,
    Spline64,
    Lanczos3,
}

impl Kernel {
    fn zimg(self) -> (sys::zimg_resample_filter_e, f64) {
        match self {
            Kernel::Point => (sys::ZIMG_RESIZE_POINT, f64::NAN),
            Kernel::Bilinear => (sys::ZIMG_RESIZE_BILINEAR, f64::NAN),
            Kernel::Bicubic => (sys::ZIMG_RESIZE_BICUBIC, f64::NAN),
            Kernel::Spline16 => (sys::ZIMG_RESIZE_SPLINE16, f64::NAN),
            Kernel::Spline36 => (sys::ZIMG_RESIZE_SPLINE36, f64::NAN),
            Kernel::Spline64 => (sys::ZIMG_RESIZE_SPLINE64, f64::NAN),
            Kernel::Lanczos3 => (sys::ZIMG_RESIZE_LANCZOS, 3.0),
        }
    }
}

/// Dithering applied when reducing to 8-bit RGB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Dither {
    /// Error diffusion for sources deeper than 8 bits, plain rounding for 8-bit sources.
    #[default]
    Auto,
    None,
    ErrorDiffusion,
}

#[derive(Debug, Clone, Default)]
pub struct ConvertOptions {
    pub chroma_kernel: Kernel,
    pub scale_kernel: Kernel,
    pub dither: Dither,
    /// Stretch non-square-pixel sources horizontally to their display aspect ratio.
    pub apply_sar: bool,
    /// Override the sample aspect ratio reported by the container.
    pub sar_override: Option<(i32, i32)>,
    /// Override matrix coefficients (H.273 code point).
    pub matrix_override: Option<i32>,
    /// Override range: `true` = full, `false` = limited.
    pub full_range_override: Option<bool>,
}

/// Colour parameters actually used for a conversion (for display / reporting).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedColor {
    pub matrix: i32,
    pub full_range: bool,
    /// zimg chroma location.
    pub chroma_location: i32,
    pub matrix_guessed: bool,
}

/// Interleaved 8-bit RGB image.
#[derive(Debug, Clone)]
pub struct RgbImage {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
    pub color: ResolvedColor,
}

const SUPPORTED_MATRICES: [i32; 8] = [1, 4, 5, 6, 7, 8, 9, 10];

/// Picks the matrix/range/chroma location for a frame, applying overrides and fallbacks.
pub fn resolve_color(c: &ColorInfo, width: u32, height: u32, jpeg_fmt: bool, opts: &ConvertOptions) -> ResolvedColor {
    let (matrix, matrix_guessed) = match opts.matrix_override {
        Some(m) => (m, false),
        None if SUPPORTED_MATRICES.contains(&c.matrix) => (c.matrix, false),
        None => {
            let sd = width <= 1024 && height <= 576;
            let m = if !sd {
                sys::ZIMG_MATRIX_BT709
            } else if height == 576 || height == 288 {
                sys::ZIMG_MATRIX_BT470_BG
            } else {
                sys::ZIMG_MATRIX_ST170_M
            };
            (m as i32, true)
        }
    };
    let full_range = opts.full_range_override.unwrap_or(match c.range {
        2 => true,
        1 => false,
        _ => jpeg_fmt,
    });
    let chroma_location = if (1..=6).contains(&c.chroma_location) { c.chroma_location - 1 } else { sys::ZIMG_CHROMA_LEFT as i32 };
    ResolvedColor { matrix, full_range, chroma_location, matrix_guessed }
}

/// Output width after the optional anamorphic stretch. Height is never changed.
pub fn output_width(width: u32, sar: (i32, i32), opts: &ConvertOptions) -> u32 {
    let (n, d) = opts.sar_override.unwrap_or(sar);
    if !opts.apply_sar || n <= 0 || d <= 0 || n == d {
        return width;
    }
    ((width as f64 * n as f64 / d as f64).round() as u32).max(1)
}

/// A 64-byte aligned plane buffer, as zimg's SIMD paths require.
struct AlignedPlane {
    buf: Vec<u8>,
    off: usize,
    stride: usize,
    rows: usize,
}

impl AlignedPlane {
    const ALIGN: usize = 64;
    fn new(row_bytes: usize, rows: usize) -> Self {
        let stride = row_bytes.div_ceil(Self::ALIGN) * Self::ALIGN;
        let buf = vec![0u8; stride * rows + Self::ALIGN];
        let off = (Self::ALIGN - (buf.as_ptr() as usize % Self::ALIGN)) % Self::ALIGN;
        Self { buf, off, stride, rows }
    }
    fn data(&self) -> &[u8] {
        &self.buf[self.off..self.off + self.stride * self.rows]
    }
    fn data_mut(&mut self) -> &mut [u8] {
        &mut self.buf[self.off..self.off + self.stride * self.rows]
    }
}

fn zimg_err(ctx: &str) -> Error {
    let mut buf = [0 as c_char; 1024];
    unsafe { sys::zimg_get_last_error(buf.as_mut_ptr(), buf.len()) };
    let msg = unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned();
    unsafe { sys::zimg_clear_last_error() };
    Error::Zimg(format!("{ctx}: {msg}"))
}

struct Graph(*mut sys::zimg_filter_graph);
impl Drop for Graph {
    fn drop(&mut self) {
        unsafe { sys::zimg_filter_graph_free(self.0) };
    }
}

/// Converts a decoded frame to interleaved 8-bit RGB.
pub fn to_rgb(frame: &Frame<'_>, sar: (i32, i32), opts: &ConvertOptions) -> Result<RgbImage> {
    let layout = frame.layout;
    let (w, h) = (frame.width as usize, frame.height as usize);
    let color = resolve_color(&frame.color, frame.width, frame.height, layout.is_jpeg_range, opts);
    let out_w = output_width(frame.width, sar, opts) as usize;
    let grey = layout.planes == 1;

    // Copy source planes into aligned buffers.
    let mut src_planes = Vec::with_capacity(layout.planes);
    for p in 0..layout.planes {
        let (pw, ph) = layout.plane_dims(p, w, h);
        let row_bytes = pw * layout.bytes_per_sample;
        let mut ap = AlignedPlane::new(row_bytes, ph);
        let stride = ap.stride;
        let dst = ap.data_mut();
        for y in 0..ph {
            dst[y * stride..y * stride + row_bytes].copy_from_slice(frame.row(p, y));
        }
        src_planes.push(ap);
    }

    let api = sys::ZIMG_API_VERSION;
    let mut src_fmt = sys::zimg_image_format::default();
    let mut dst_fmt = sys::zimg_image_format::default();
    let mut params = sys::zimg_graph_builder_params::default();
    unsafe {
        sys::zimg_image_format_default(&mut src_fmt, api);
        sys::zimg_image_format_default(&mut dst_fmt, api);
        sys::zimg_graph_builder_params_default(&mut params, api);
    }

    src_fmt.width = w as u32;
    src_fmt.height = h as u32;
    src_fmt.pixel_type = if layout.bytes_per_sample == 2 { sys::ZIMG_PIXEL_WORD } else { sys::ZIMG_PIXEL_BYTE };
    src_fmt.depth = layout.bit_depth;
    src_fmt.subsample_w = layout.log2_chroma_w;
    src_fmt.subsample_h = layout.log2_chroma_h;
    src_fmt.color_family = if grey { sys::ZIMG_COLOR_GREY } else { sys::ZIMG_COLOR_YUV };
    src_fmt.matrix_coefficients = if grey { sys::ZIMG_MATRIX_UNSPECIFIED } else { color.matrix };
    // Keep transfer/primaries identical on both sides so no gamut/gamma conversion happens.
    src_fmt.transfer_characteristics = sys::ZIMG_TRANSFER_UNSPECIFIED;
    src_fmt.color_primaries = sys::ZIMG_PRIMARIES_UNSPECIFIED;
    src_fmt.pixel_range = if color.full_range { sys::ZIMG_RANGE_FULL } else { sys::ZIMG_RANGE_LIMITED };
    src_fmt.chroma_location = color.chroma_location;

    dst_fmt.width = out_w as u32;
    dst_fmt.height = h as u32;
    dst_fmt.pixel_type = sys::ZIMG_PIXEL_BYTE;
    dst_fmt.depth = 8;
    dst_fmt.color_family = if grey { sys::ZIMG_COLOR_GREY } else { sys::ZIMG_COLOR_RGB };
    dst_fmt.matrix_coefficients = if grey { sys::ZIMG_MATRIX_UNSPECIFIED } else { sys::ZIMG_MATRIX_RGB };
    dst_fmt.transfer_characteristics = sys::ZIMG_TRANSFER_UNSPECIFIED;
    dst_fmt.color_primaries = sys::ZIMG_PRIMARIES_UNSPECIFIED;
    dst_fmt.pixel_range = sys::ZIMG_RANGE_FULL;

    let (k, ka) = opts.scale_kernel.zimg();
    params.resample_filter = k;
    params.filter_param_a = ka;
    let (ck, cka) = opts.chroma_kernel.zimg();
    params.resample_filter_uv = ck;
    params.filter_param_a_uv = cka;
    params.dither_type = match opts.dither {
        Dither::None => sys::ZIMG_DITHER_NONE,
        Dither::ErrorDiffusion => sys::ZIMG_DITHER_ERROR_DIFFUSION,
        Dither::Auto if layout.bit_depth > 8 => sys::ZIMG_DITHER_ERROR_DIFFUSION,
        Dither::Auto => sys::ZIMG_DITHER_NONE,
    };

    let graph = unsafe { sys::zimg_filter_graph_build(&src_fmt, &dst_fmt, &params) };
    if graph.is_null() {
        return Err(zimg_err("build graph"));
    }
    let graph = Graph(graph);

    let mut tmp_size = 0usize;
    if unsafe { sys::zimg_filter_graph_get_tmp_size(graph.0, &mut tmp_size) } != sys::ZIMG_ERROR_SUCCESS {
        return Err(zimg_err("tmp size"));
    }
    let mut tmp = AlignedPlane::new(tmp_size.max(1), 1);

    let out_planes_n = if grey { 1 } else { 3 };
    let mut dst_planes: Vec<AlignedPlane> = (0..out_planes_n).map(|_| AlignedPlane::new(out_w, h)).collect();

    let mut src_buf = sys::zimg_image_buffer_const { version: api, ..Default::default() };
    for (i, ap) in src_planes.iter().enumerate() {
        src_buf.plane[i].data = ap.data().as_ptr() as *const _;
        src_buf.plane[i].stride = ap.stride as isize;
        src_buf.plane[i].mask = sys::ZIMG_BUFFER_MAX;
    }
    let mut dst_buf = sys::zimg_image_buffer { version: api, ..Default::default() };
    for (i, ap) in dst_planes.iter_mut().enumerate() {
        dst_buf.plane[i].stride = ap.stride as isize;
        dst_buf.plane[i].data = ap.data_mut().as_mut_ptr() as *mut _;
        dst_buf.plane[i].mask = sys::ZIMG_BUFFER_MAX;
    }

    let rc = unsafe {
        sys::zimg_filter_graph_process(
            graph.0,
            &src_buf,
            &dst_buf,
            tmp.data_mut().as_mut_ptr() as *mut _,
            None,
            std::ptr::null_mut(),
            None,
            std::ptr::null_mut(),
        )
    };
    if rc != sys::ZIMG_ERROR_SUCCESS {
        return Err(zimg_err("process"));
    }

    // Interleave planar RGB (or replicate grey) into RGB24.
    let mut data = vec![0u8; out_w * h * 3];
    for y in 0..h {
        let out_row = &mut data[y * out_w * 3..(y + 1) * out_w * 3];
        if grey {
            let g = &dst_planes[0].data()[y * dst_planes[0].stride..][..out_w];
            for (px, &v) in out_row.chunks_exact_mut(3).zip(g) {
                px.copy_from_slice(&[v, v, v]);
            }
        } else {
            let r = &dst_planes[0].data()[y * dst_planes[0].stride..][..out_w];
            let g = &dst_planes[1].data()[y * dst_planes[1].stride..][..out_w];
            let b = &dst_planes[2].data()[y * dst_planes[2].stride..][..out_w];
            for (x, px) in out_row.chunks_exact_mut(3).enumerate() {
                px[0] = r[x];
                px[1] = g[x];
                px[2] = b[x];
            }
        }
    }

    Ok(RgbImage { width: out_w as u32, height: h as u32, data, color })
}

/// PNG compression effort.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PngCompression {
    #[default]
    Fast,
    Default,
    Best,
}

/// Writes an RGB image as an 8-bit PNG.
pub fn write_png(img: &RgbImage, path: &Path, compression: PngCompression) -> Result<()> {
    let file = BufWriter::new(File::create(path)?);
    let mut enc = png::Encoder::new(file, img.width, img.height);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(match compression {
        PngCompression::Fast => png::Compression::Fast,
        PngCompression::Default => png::Compression::Default,
        PngCompression::Best => png::Compression::Best,
    });
    // sRGB chunk so browsers don't apply odd colour management.
    enc.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
    let mut w = enc.write_header()?;
    w.write_image_data(&img.data)?;
    w.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unspecified() -> ColorInfo {
        ColorInfo { matrix: 2, transfer: 2, primaries: 2, range: 0, chroma_location: 0 }
    }

    #[test]
    fn matrix_fallbacks_by_resolution() {
        let o = ConvertOptions::default();
        let c = unspecified();
        assert_eq!(resolve_color(&c, 1920, 1080, false, &o).matrix, sys::ZIMG_MATRIX_BT709 as i32);
        assert_eq!(resolve_color(&c, 1420, 1080, false, &o).matrix, sys::ZIMG_MATRIX_BT709 as i32);
        assert_eq!(resolve_color(&c, 720, 480, false, &o).matrix, sys::ZIMG_MATRIX_ST170_M as i32);
        assert_eq!(resolve_color(&c, 700, 478, false, &o).matrix, sys::ZIMG_MATRIX_ST170_M as i32);
        assert_eq!(resolve_color(&c, 720, 576, false, &o).matrix, sys::ZIMG_MATRIX_BT470_BG as i32);
        assert!(resolve_color(&c, 720, 480, false, &o).matrix_guessed);
    }

    #[test]
    fn tagged_metadata_and_overrides_win() {
        let mut c = unspecified();
        c.matrix = 1; // BT.709 tagged on an SD stream
        let r = resolve_color(&c, 720, 480, false, &ConvertOptions::default());
        assert_eq!((r.matrix, r.matrix_guessed), (1, false));
        let o = ConvertOptions { matrix_override: Some(6), full_range_override: Some(true), ..Default::default() };
        let r = resolve_color(&c, 720, 480, false, &o);
        assert_eq!((r.matrix, r.full_range), (6, true));
    }

    #[test]
    fn range_and_chroma_location() {
        let o = ConvertOptions::default();
        let mut c = unspecified();
        assert!(!resolve_color(&c, 1920, 1080, false, &o).full_range);
        assert!(resolve_color(&c, 1920, 1080, true, &o).full_range, "yuvj formats default to full range");
        c.range = 2;
        assert!(resolve_color(&c, 1920, 1080, false, &o).full_range);
        c.chroma_location = 3; // AVCHROMA_LOC_TOPLEFT (BT.2020/UHD)
        assert_eq!(resolve_color(&c, 1920, 1080, false, &o).chroma_location, sys::ZIMG_CHROMA_TOP_LEFT as i32);
        c.chroma_location = 0;
        assert_eq!(resolve_color(&c, 1920, 1080, false, &o).chroma_location, sys::ZIMG_CHROMA_LEFT as i32);
    }

    #[test]
    fn anamorphic_width() {
        let on = ConvertOptions { apply_sar: true, ..Default::default() };
        assert_eq!(output_width(720, (32, 27), &on), 853); // NTSC 16:9
        assert_eq!(output_width(720, (8, 9), &on), 640); // NTSC 4:3
        assert_eq!(output_width(700, (10, 11), &on), 636); // cropped NTSC 4:3 encode
        assert_eq!(output_width(720, (16, 15), &on), 768); // PAL 4:3
        assert_eq!(output_width(1920, (1, 1), &on), 1920);
        assert_eq!(output_width(1920, (0, 0), &on), 1920);
        assert_eq!(output_width(720, (32, 27), &ConvertOptions::default()), 720);
        let ov = ConvertOptions { apply_sar: true, sar_override: Some((1, 1)), ..Default::default() };
        assert_eq!(output_width(720, (32, 27), &ov), 720);
    }
}
