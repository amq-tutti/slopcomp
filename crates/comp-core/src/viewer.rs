//! Offline HTML viewer written next to the screenshots (`index.html`), plus a season index.
//!
//! Self-contained: the comparison data is inlined (no fetch, so it works from `file://`), and
//! images are referenced by relative path.

use std::path::Path;

use crate::comp::CompResult;
use crate::error::Result;

const TEMPLATE: &str = include_str!("viewer.html");

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// JSON that is safe to embed inside a `<script>` element.
fn script_json<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string(v).expect("serialise").replace("</", "<\\/").replace("<!--", "<\\!--")
}

/// Writes `<out_dir>/index.html` for a finished comparison.
pub fn write(result: &CompResult) -> Result<std::path::PathBuf> {
    let html = TEMPLATE.replace("__TITLE__", &html_escape(&result.name)).replace("__DATA__", &script_json(result));
    let path = result.out_dir.join("index.html");
    std::fs::write(&path, html)?;
    Ok(path)
}

/// One episode in a season index.
pub struct SeasonEntry {
    pub episode: String,
    /// Path of the episode's folder relative to the index.
    pub dir: String,
    pub frames: usize,
    /// Relative path of a thumbnail (first reference screenshot), if any.
    pub thumb: Option<String>,
}

/// Writes `<dir>/index.html` linking each episode's viewer.
pub fn write_season_index(dir: &Path, title: &str, entries: &[SeasonEntry]) -> Result<std::path::PathBuf> {
    let mut cards = String::new();
    for e in entries {
        let img = e
            .thumb
            .as_ref()
            .map(|t| format!(r#"<img loading="lazy" src="{}" alt="">"#, html_escape(t)))
            .unwrap_or_default();
        cards.push_str(&format!(
            r#"<a class="card" href="{dir}/index.html">{img}<span>{ep}</span><small>{n} frames</small></a>"#,
            dir = html_escape(&e.dir),
            ep = html_escape(&e.episode),
            n = e.frames
        ));
    }
    let html = format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1"><title>{t}</title>
<style>
:root {{ --bg:#f4f4f2; --panel:#fff; --text:#1d1d1f; --muted:#6b6b70; --line:#dcdcd8; --accent:#2563eb; }}
@media (prefers-color-scheme: dark) {{ :root {{ --bg:#121214; --panel:#1b1b1e; --text:#ececf0; --muted:#9a9aa2; --line:#2c2c31; --accent:#4f8cff; }} }}
body {{ margin:0; background:var(--bg); color:var(--text); font:14px/1.4 -apple-system,BlinkMacSystemFont,"Segoe UI",system-ui,sans-serif; }}
h1 {{ font-size:18px; font-weight:600; margin:0; padding:16px; }}
.grid {{ display:grid; grid-template-columns:repeat(auto-fill,minmax(220px,1fr)); gap:12px; padding:0 16px 24px; }}
.card {{ display:flex; flex-direction:column; background:var(--panel); border:1px solid var(--line); border-radius:8px;
  overflow:hidden; color:inherit; text-decoration:none; }}
.card:hover {{ border-color:var(--accent); }}
.card img {{ width:100%; aspect-ratio:16/9; object-fit:cover; background:#000; }}
.card span {{ font-weight:600; padding:8px 10px 0; }}
.card small {{ color:var(--muted); padding:0 10px 10px; }}
</style></head><body><h1>{t}</h1><div class="grid">{cards}</div></body></html>"#,
        t = html_escape(title)
    );
    let path = dir.join("index.html");
    std::fs::write(&path, html)?;
    Ok(path)
}
