//! Container chapters (via libavformat) and OP/ED classification by chapter title.

use std::ffi::{CStr, CString};
use std::path::Path;

use comp_sys as sys;

use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct Chapter {
    /// Absolute container time in seconds.
    pub start: f64,
    pub end: f64,
    pub title: Option<String>,
}

/// What a chapter title says about its content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChapterKind {
    Opening,
    Ending,
    Other,
}

/// Reads chapters from a media file. Files without chapters return an empty list.
pub fn read(path: &Path) -> Result<Vec<Chapter>> {
    let cpath = CString::new(path.to_str().ok_or_else(|| Error::BadPath(path.to_owned()))?)
        .map_err(|_| Error::BadPath(path.to_owned()))?;
    unsafe {
        let mut ctx: *mut sys::AVFormatContext = std::ptr::null_mut();
        let rc = sys::avformat_open_input(&mut ctx, cpath.as_ptr(), std::ptr::null(), std::ptr::null_mut());
        if rc < 0 || ctx.is_null() {
            return Err(Error::Spec(format!("can't open {} to read chapters ({rc})", path.display())));
        }
        let mut out = Vec::new();
        let key = CString::new("title").unwrap();
        for i in 0..(*ctx).nb_chapters as usize {
            let ch = &**(*ctx).chapters.add(i);
            let tb = ch.time_base.num as f64 / ch.time_base.den as f64;
            let e = sys::av_dict_get(ch.metadata, key.as_ptr(), std::ptr::null(), 0);
            let title = (!e.is_null()).then(|| CStr::from_ptr((*e).value).to_string_lossy().into_owned());
            // Without avformat_find_stream_info the end is often unset (AV_NOPTS_VALUE).
            let end = if ch.end > ch.start { ch.end as f64 * tb } else { f64::NAN };
            out.push(Chapter { start: ch.start as f64 * tb, end, title });
        }
        // Fill missing ends: next chapter's start, or the container duration for the last one.
        let duration = if (*ctx).duration > 0 { Some((*ctx).duration as f64 / 1_000_000.0) } else { None };
        sys::avformat_close_input(&mut ctx);
        out.sort_by(|a, b| a.start.total_cmp(&b.start));
        let starts: Vec<f64> = out.iter().map(|c| c.start).collect();
        for (i, c) in out.iter_mut().enumerate() {
            if c.end.is_nan() {
                c.end = starts.get(i + 1).copied().or(duration).unwrap_or(f64::INFINITY);
            }
        }
        Ok(out)
    }
}

/// Classifies a chapter title. Matches whole tokens like `OP`, `OP1`, `ED2`, `Opening`,
/// `Ending`, `Intro`, `Outro`, `Credits`, and the Japanese オープニング / エンディング.
/// `NCOP`/`NCED` (creditless) count too.
pub fn classify(title: &str) -> ChapterKind {
    let t = title.to_lowercase();
    if t.contains("オープニング") || t.contains("opening") || t.contains("intro") {
        return ChapterKind::Opening;
    }
    if t.contains("エンディング") || t.contains("ending") || t.contains("outro") || t.contains("credit") {
        return ChapterKind::Ending;
    }
    for tok in t.split(|c: char| !c.is_alphanumeric()) {
        let tok = tok.strip_prefix("nc").unwrap_or(tok);
        let digits_after = |p: &str| tok.strip_prefix(p).is_some_and(|r| r.chars().all(|c| c.is_ascii_digit()));
        if digits_after("op") {
            return ChapterKind::Opening;
        }
        if digits_after("ed") {
            return ChapterKind::Ending;
        }
    }
    ChapterKind::Other
}

/// OP/ED time ranges (absolute seconds) from named chapters.
pub fn op_ed_ranges(chapters: &[Chapter]) -> Vec<(f64, f64, ChapterKind)> {
    chapters
        .iter()
        .filter_map(|c| {
            let kind = classify(c.title.as_deref()?);
            (kind != ChapterKind::Other && c.end > c.start).then_some((c.start, c.end, kind))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_titles() {
        use ChapterKind::*;
        for (t, k) in [
            ("Opening", Opening),
            ("OP", Opening),
            ("OP1", Opening),
            ("Part A / OP", Opening),
            ("NCOP", Opening),
            ("Intro", Opening),
            ("オープニング", Opening),
            ("Ending", Ending),
            ("ED2", Ending),
            ("End Credits", Ending),
            ("エンディング", Ending),
            ("Chapter 02", Other),
            ("Part A", Other),
            ("Prologue", Other),
            ("Episode", Other),
            ("Opposition", Other),
            ("Edge", Other),
            ("Preview", Other),
        ] {
            assert_eq!(classify(t), k, "{t}");
        }
    }
}
