//! Frame-accurate video access via ffms2.
//!
//! [`VideoSource::open`] indexes the file (or loads a cached index) and then serves any frame by
//! number with [`VideoSource::frame`]. Frames are returned in the decoder's native planar YUV
//! format together with their colour metadata. No scaling or format conversion happens here.

use std::ffi::{CStr, CString};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::raw::{c_char, c_int};
use std::path::{Path, PathBuf};
use std::sync::Once;

use comp_sys as sys;

use crate::error::{Error, Result};

static INIT: Once = Once::new();

fn init() {
    INIT.call_once(|| unsafe {
        sys::FFMS_Init(0, 0);
        sys::FFMS_SetLogLevel(sys::FFMS_LOG_QUIET);
    });
}

/// Owned error buffer for ffms2 calls.
struct ErrInfo {
    buf: Box<[c_char; 1024]>,
    info: sys::FFMS_ErrorInfo,
}

impl ErrInfo {
    fn new() -> Self {
        let mut buf = Box::new([0 as c_char; 1024]);
        let info = sys::FFMS_ErrorInfo {
            ErrorType: 0,
            SubType: 0,
            BufferSize: buf.len() as c_int,
            Buffer: buf.as_mut_ptr(),
        };
        Self { buf, info }
    }
    fn ptr(&mut self) -> *mut sys::FFMS_ErrorInfo {
        self.info.Buffer = self.buf.as_mut_ptr();
        &mut self.info
    }
    fn err(&self, ctx: &str) -> Error {
        let msg = unsafe { CStr::from_ptr(self.buf.as_ptr()) }.to_string_lossy();
        Error::Ffms(format!("{ctx}: {msg}"))
    }
}

fn cpath(p: &Path) -> Result<CString> {
    let s = p.to_str().ok_or_else(|| Error::BadPath(p.to_owned()))?;
    CString::new(s).map_err(|_| Error::BadPath(p.to_owned()))
}

/// Stable cache key for a media file: size + mtime + xxh3 of the first and last MiB.
pub fn cache_key(path: &Path) -> Result<String> {
    const CHUNK: u64 = 1 << 20;
    let mut f = File::open(path)?;
    let meta = f.metadata()?;
    let len = meta.len();
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut hasher = xxhash_rust::xxh3::Xxh3::new();
    let mut buf = vec![0u8; CHUNK as usize];
    let n = f.read(&mut buf)?;
    hasher.update(&buf[..n]);
    if len > CHUNK {
        f.seek(SeekFrom::Start(len.saturating_sub(CHUNK)))?;
        let n = f.read(&mut buf)?;
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:016x}-{len:x}-{mtime:x}", hasher.digest()))
}

/// ffms2 seek strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SeekMode {
    /// Pick automatically from the container format (see [`SeekMode::for_format`]).
    #[default]
    Auto,
    /// Never seek: decode linearly from the last keyframe/start. Always correct, slow for backwards access.
    Linear,
    /// Seek to keyframes using the index (ffms2 default).
    Normal,
    Unsafe,
    Aggressive,
}

impl SeekMode {
    /// Seek policy for a container, as reported by libavformat's demuxer name.
    ///
    /// MPEG program streams (DVD VOB / .mpg) and elementary MPEG video streams return the frame
    /// *before* the requested one under keyframe seeking (checked against an independent decoder in
    /// `tests/seek_accuracy.rs`), so they're decoded linearly. Callers should request frames in
    /// ascending order to keep this to a single pass.
    pub fn for_format(container: &str) -> SeekMode {
        match container {
            "mpeg" | "mpegvideo" | "vob" | "dvd" => SeekMode::Linear,
            _ => SeekMode::Normal,
        }
    }

    fn ffms(self) -> std::os::raw::c_int {
        match self {
            SeekMode::Linear => sys::FFMS_SEEK_LINEAR,
            SeekMode::Normal | SeekMode::Auto => sys::FFMS_SEEK_NORMAL,
            SeekMode::Unsafe => sys::FFMS_SEEK_UNSAFE,
            SeekMode::Aggressive => sys::FFMS_SEEK_AGGRESSIVE,
        }
    }
}

/// Options for opening a source.
#[derive(Debug, Clone)]
pub struct OpenOptions {
    pub seek_mode: SeekMode,
    /// Directory where `.ffindex` files are cached. `None` disables caching.
    pub index_cache: Option<PathBuf>,
    /// Decoder threads (0 = auto).
    pub threads: i32,
}

impl Default for OpenOptions {
    fn default() -> Self {
        Self { seek_mode: SeekMode::Auto, index_cache: None, threads: 0 }
    }
}

/// Colour metadata of a frame, using ISO/IEC 23091-4 (H.273) code points, as ffmpeg and zimg do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorInfo {
    /// Matrix coefficients (2 = unspecified).
    pub matrix: i32,
    pub transfer: i32,
    pub primaries: i32,
    /// 0 = unspecified, 1 = limited (MPEG), 2 = full (JPEG).
    pub range: i32,
    /// ffmpeg `AVChromaLocation` (0 = unspecified, 1 = left, 2 = center, ...).
    pub chroma_location: i32,
}

/// Static description of the decoded pixel layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelLayout {
    /// ffmpeg `AVPixelFormat` id.
    pub av_format: i32,
    /// 1 (grey) or 3 (YUV).
    pub planes: usize,
    pub bit_depth: u32,
    pub log2_chroma_w: u32,
    pub log2_chroma_h: u32,
    /// Bytes per sample (1 or 2).
    pub bytes_per_sample: usize,
    /// True for the legacy `yuvj*` formats (full range).
    pub is_jpeg_range: bool,
}

impl PixelLayout {
    fn from_av(id: i32) -> Result<Self> {
        let desc = unsafe { sys::av_pix_fmt_desc_get(id) };
        if desc.is_null() {
            return Err(Error::UnsupportedPixelFormat { id, name: "?".into() });
        }
        let d = unsafe { &*desc };
        let name = unsafe { CStr::from_ptr(d.name) }.to_string_lossy().into_owned();
        let bad = (sys::AV_PIX_FMT_FLAG_BE
            | sys::AV_PIX_FMT_FLAG_PAL
            | sys::AV_PIX_FMT_FLAG_BITSTREAM
            | sys::AV_PIX_FMT_FLAG_HWACCEL
            | sys::AV_PIX_FMT_FLAG_RGB
            | sys::AV_PIX_FMT_FLAG_ALPHA
            | sys::AV_PIX_FMT_FLAG_FLOAT
            | sys::AV_PIX_FMT_FLAG_BAYER
            | sys::AV_PIX_FMT_FLAG_XYZ) as u64;
        let nb = d.nb_components as usize;
        let planar = d.flags & sys::AV_PIX_FMT_FLAG_PLANAR as u64 != 0 || nb == 1;
        let comps = &d.comp[..nb];
        let depth = comps.first().map(|c| c.depth).unwrap_or(0);
        let ok = d.flags & bad == 0
            && planar
            && (nb == 1 || nb == 3)
            && comps.iter().enumerate().all(|(i, c)| c.plane == i as c_int && c.shift == 0 && c.offset == 0 && c.depth == depth)
            && (1..=16).contains(&depth);
        if !ok {
            return Err(Error::UnsupportedPixelFormat { id, name });
        }
        Ok(Self {
            av_format: id,
            planes: nb,
            bit_depth: depth as u32,
            log2_chroma_w: d.log2_chroma_w as u32,
            log2_chroma_h: d.log2_chroma_h as u32,
            bytes_per_sample: if depth > 8 { 2 } else { 1 },
            is_jpeg_range: name.starts_with("yuvj"),
        })
    }

    /// Width and height of plane `p` for a frame of `w`×`h`.
    pub fn plane_dims(&self, p: usize, w: usize, h: usize) -> (usize, usize) {
        if p == 0 {
            (w, h)
        } else {
            let cw = (w + (1 << self.log2_chroma_w) - 1) >> self.log2_chroma_w;
            let ch = (h + (1 << self.log2_chroma_h) - 1) >> self.log2_chroma_h;
            (cw, ch)
        }
    }
}

/// Video stream properties.
#[derive(Debug, Clone)]
pub struct VideoInfo {
    /// libavformat demuxer name (e.g. "matroska,webm", "mpegts", "mpeg").
    pub container: String,
    /// Seek mode actually used (never `Auto`).
    pub seek_mode: SeekMode,
    pub num_frames: i32,
    pub fps_num: i32,
    pub fps_den: i32,
    /// Sample aspect ratio; (0, 0) or (1, 1) mean square pixels.
    pub sar: (i32, i32),
    pub width: u32,
    pub height: u32,
    pub layout: PixelLayout,
    pub color: ColorInfo,
}

/// A decoded frame borrowed from the source. Valid until the next call to `frame()`.
pub struct Frame<'a> {
    pub number: i32,
    pub width: u32,
    pub height: u32,
    pub layout: PixelLayout,
    pub color: ColorInfo,
    /// 'I', 'P', 'B', or another ffmpeg picture type character ('?' if unknown).
    pub pict_type: char,
    pub key_frame: bool,
    pub interlaced: bool,
    pub top_field_first: bool,
    planes: [(&'a [u8], usize); 3],
}

impl<'a> Frame<'a> {
    /// Plane `p` as (data, stride in bytes). `data` covers exactly `height` rows of the plane.
    pub fn plane(&self, p: usize) -> (&'a [u8], usize) {
        self.planes[p]
    }

    /// Bytes of row `y` of plane `p`, without any padding.
    pub fn row(&self, p: usize, y: usize) -> &'a [u8] {
        let (w, _) = self.layout.plane_dims(p, self.width as usize, self.height as usize);
        let (data, stride) = self.planes[p];
        let start = y * stride;
        &data[start..start + w * self.layout.bytes_per_sample]
    }
}

/// Frame-accurate random-access video source.
pub struct VideoSource {
    path: PathBuf,
    vs: *mut sys::FFMS_VideoSource,
    info: VideoInfo,
    err: ErrInfo,
}

// The ffms2 source can be moved to another thread, but not shared between threads.
unsafe impl Send for VideoSource {}

impl VideoSource {
    /// Opens `path`, indexing the first video track. Cached indexes are used when available.
    pub fn open(path: &Path, opts: &OpenOptions) -> Result<Self> {
        Self::open_with_progress(path, opts, |_, _| true)
    }

    /// Like [`open`](Self::open), but reports indexing progress as `(bytes_done, bytes_total)`.
    /// Return `false` from the callback to cancel indexing.
    pub fn open_with_progress<F>(path: &Path, opts: &OpenOptions, mut progress: F) -> Result<Self>
    where
        F: FnMut(i64, i64) -> bool,
    {
        init();
        let cpath_ = cpath(path)?;
        let mut err = ErrInfo::new();

        let cache_file = match &opts.index_cache {
            Some(dir) => {
                std::fs::create_dir_all(dir)?;
                Some(dir.join(format!("{}.ffindex", cache_key(path)?)))
            }
            None => None,
        };

        // Always create an indexer: it's cheap (it only opens and probes the file) and tells us the
        // container format, which the seek policy needs even when a cached index is used.
        let indexer = unsafe { sys::FFMS_CreateIndexer(cpath_.as_ptr(), err.ptr()) };
        if indexer.is_null() {
            return Err(err.err("create indexer"));
        }
        let container = unsafe { CStr::from_ptr(sys::FFMS_GetFormatNameI(indexer)) }.to_string_lossy().into_owned();
        let seek_mode = match opts.seek_mode {
            SeekMode::Auto => SeekMode::for_format(&container),
            m => m,
        };

        let mut index: *mut sys::FFMS_Index = std::ptr::null_mut();
        if let Some(cf) = cache_file.as_ref().filter(|p| p.exists()) {
            let ccf = cpath(cf)?;
            index = unsafe { sys::FFMS_ReadIndex(ccf.as_ptr(), err.ptr()) };
            if !index.is_null() && unsafe { sys::FFMS_IndexBelongsToFile(index, cpath_.as_ptr(), err.ptr()) } != 0 {
                unsafe { sys::FFMS_DestroyIndex(index) };
                index = std::ptr::null_mut();
            }
        }

        if !index.is_null() {
            // CancelIndexing frees the indexer without indexing.
            unsafe { sys::FFMS_CancelIndexing(indexer) };
        } else {
            // Only video is needed. Audio decoders aren't even built.
            unsafe { sys::FFMS_TrackTypeIndexSettings(indexer, sys::FFMS_TYPE_AUDIO, 0, 0) };

            unsafe extern "C" fn cb<F: FnMut(i64, i64) -> bool>(
                current: i64,
                total: i64,
                private: *mut std::ffi::c_void,
            ) -> c_int {
                let f = &mut *(private as *mut F);
                if f(current, total) {
                    0
                } else {
                    1
                }
            }
            unsafe {
                sys::FFMS_SetProgressCallback(indexer, Some(cb::<F>), &mut progress as *mut F as *mut _);
            }
            // DoIndexing2 always frees the indexer.
            index = unsafe { sys::FFMS_DoIndexing2(indexer, sys::FFMS_IEH_STOP_TRACK as c_int, err.ptr()) };
            if index.is_null() {
                return Err(err.err("indexing"));
            }
            if let Some(cf) = &cache_file {
                let ccf = cpath(cf)?;
                // A failed cache write is not fatal.
                unsafe { sys::FFMS_WriteIndex(ccf.as_ptr(), index, err.ptr()) };
            }
        }

        let track = unsafe { sys::FFMS_GetFirstIndexedTrackOfType(index, sys::FFMS_TYPE_VIDEO, err.ptr()) };
        if track < 0 {
            unsafe { sys::FFMS_DestroyIndex(index) };
            return Err(Error::NoVideoTrack(path.to_owned()));
        }
        let vs = unsafe {
            sys::FFMS_CreateVideoSource(cpath_.as_ptr(), track, index, opts.threads, seek_mode.ffms(), err.ptr())
        };
        unsafe { sys::FFMS_DestroyIndex(index) };
        if vs.is_null() {
            return Err(err.err("create video source"));
        }

        // Decode frame 0 to learn the real pixel format and colour info.
        let props = unsafe { &*sys::FFMS_GetVideoProperties(vs) };
        let f0 = unsafe { sys::FFMS_GetFrame(vs, 0, err.ptr()) };
        if f0.is_null() {
            unsafe { sys::FFMS_DestroyVideoSource(vs) };
            return Err(err.err("decode frame 0"));
        }
        let f0 = unsafe { &*f0 };
        let layout = match PixelLayout::from_av(f0.EncodedPixelFormat) {
            Ok(l) => l,
            Err(e) => {
                unsafe { sys::FFMS_DestroyVideoSource(vs) };
                return Err(e);
            }
        };
        let info = VideoInfo {
            container,
            seek_mode,
            num_frames: props.NumFrames,
            fps_num: props.FPSNumerator,
            fps_den: props.FPSDenominator,
            sar: (props.SARNum, props.SARDen),
            width: f0.EncodedWidth as u32,
            height: f0.EncodedHeight as u32,
            layout,
            color: color_of(f0),
        };
        Ok(Self { path: path.to_owned(), vs, info, err })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn info(&self) -> &VideoInfo {
        &self.info
    }

    /// Picture type of frame `n` without returning pixel data ('?' if unknown).
    pub fn pict_type(&mut self, n: i32) -> Result<char> {
        Ok(self.frame(n)?.pict_type)
    }

    /// Decodes frame `n` (0-based, in presentation order).
    pub fn frame(&mut self, n: i32) -> Result<Frame<'_>> {
        if n < 0 || n >= self.info.num_frames {
            return Err(Error::FrameOutOfRange { frame: n as i64, count: self.info.num_frames });
        }
        let f = unsafe { sys::FFMS_GetFrame(self.vs, n, self.err.ptr()) };
        if f.is_null() {
            return Err(self.err.err(&format!("decode frame {n}")));
        }
        let f = unsafe { &*f };
        let layout = PixelLayout::from_av(f.EncodedPixelFormat)?;
        let (w, h) = (f.EncodedWidth as usize, f.EncodedHeight as usize);
        let mut planes: [(&[u8], usize); 3] = [(&[], 0); 3];
        for p in 0..layout.planes {
            let (pw, ph) = layout.plane_dims(p, w, h);
            let stride = f.Linesize[p];
            assert!(stride > 0, "negative/zero stride from ffms2");
            let stride = stride as usize;
            let len = stride * (ph - 1) + pw * layout.bytes_per_sample;
            planes[p] = (unsafe { std::slice::from_raw_parts(f.Data[p], len) }, stride);
        }
        Ok(Frame {
            number: n,
            width: w as u32,
            height: h as u32,
            layout,
            color: color_of(f),
            pict_type: if f.PictType > 0 { f.PictType as u8 as char } else { '?' },
            key_frame: f.KeyFrame != 0,
            interlaced: f.InterlacedFrame != 0,
            top_field_first: f.TopFieldFirst != 0,
            planes,
        })
    }
}

fn color_of(f: &sys::FFMS_Frame) -> ColorInfo {
    ColorInfo {
        matrix: f.ColorSpace,
        transfer: f.TransferCharateristics,
        primaries: f.ColorPrimaries,
        range: f.ColorRange,
        chroma_location: f.ChromaLocation,
    }
}

impl Drop for VideoSource {
    fn drop(&mut self) {
        unsafe { sys::FFMS_DestroyVideoSource(self.vs) };
    }
}
