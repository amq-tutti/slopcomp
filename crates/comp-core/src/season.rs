//! Running a comparison over whole seasons: sources given as folders are paired by episode
//! number, each complete episode gets its own comparison, and a season index links them.

use std::path::PathBuf;

use crate::comp::{self, CompResult, CompSpec, Event};
use crate::error::{Error, Result};
use crate::pairing::{self, Pairing};
use crate::viewer;

/// Events for a whole run (one or many episodes).
#[derive(Debug, Clone)]
pub enum SeasonEvent {
    /// Pairing done. Only sent for multi-episode runs.
    Paired(Pairing),
    EpisodeStarted { index: usize, total: usize, episode: String },
    Episode { index: usize, event: Event },
    EpisodeDone { index: usize, result: Box<CompResult> },
    EpisodeFailed { index: usize, episode: String, error: String },
    /// Season index written (multi-episode runs).
    IndexWritten(PathBuf),
}

#[derive(Debug, Clone)]
pub struct SeasonResult {
    pub results: Vec<CompResult>,
    pub failed: Vec<(String, String)>,
    /// Season index page (multi-episode) or the single comparison's viewer.
    pub index: Option<PathBuf>,
}

/// Expands each source path (file or folder) and pairs episodes.
pub fn plan(inputs: &[PathBuf]) -> Result<(Vec<Vec<PathBuf>>, Pairing)> {
    let lists: Vec<Vec<PathBuf>> = inputs
        .iter()
        .map(|p| {
            let v = pairing::expand_input(p)?;
            if v.is_empty() {
                return Err(Error::Spec(format!("no video files in {}", p.display())));
            }
            Ok(v)
        })
        .collect::<Result<_>>()?;
    let pairs = pairing::pair(&lists);
    Ok((lists, pairs))
}

/// Runs `template` for every complete episode. Each source's `path` in the template is a file or
/// a folder. With single files this is one plain comparison in `template.out_dir`. With folders,
/// each episode goes to `out_dir/<episode>/` and a season index is written to `out_dir`.
pub fn run(template: &CompSpec, on_event: &(dyn Fn(SeasonEvent) + Sync)) -> Result<SeasonResult> {
    let inputs: Vec<PathBuf> = template.sources.iter().map(|s| s.path.clone()).collect();
    let (lists, pairs) = plan(&inputs)?;
    let multi = lists.iter().any(|l| l.len() > 1);

    if !multi {
        let mut spec = template.clone();
        for (s, l) in spec.sources.iter_mut().zip(&lists) {
            s.path = l[0].clone();
        }
        on_event(SeasonEvent::EpisodeStarted { index: 0, total: 1, episode: String::new() });
        let res = comp::run(&spec, &|e| on_event(SeasonEvent::Episode { index: 0, event: e }))?;
        let index = Some(res.out_dir.join("index.html"));
        on_event(SeasonEvent::EpisodeDone { index: 0, result: Box::new(res.clone()) });
        return Ok(SeasonResult { results: vec![res], failed: vec![], index });
    }

    on_event(SeasonEvent::Paired(pairs.clone()));
    let rows: Vec<_> = pairs.complete().cloned().collect();
    if rows.is_empty() {
        return Err(Error::Spec("no episode is present in every source".into()));
    }
    let total = rows.len();
    let mut out = SeasonResult { results: vec![], failed: vec![], index: None };
    let mut entries = Vec::new();
    for (index, row) in rows.into_iter().enumerate() {
        if template.cancel.as_ref().is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed)) {
            return Err(Error::Cancelled);
        }
        let ep = row.episode.to_string();
        on_event(SeasonEvent::EpisodeStarted { index, total, episode: ep.clone() });
        let mut spec = template.clone();
        for (s, f) in spec.sources.iter_mut().zip(&row.files) {
            s.path = f.clone().unwrap();
        }
        spec.name = format!("{} - {ep}", template.name);
        spec.out_dir = template.out_dir.join(&ep);
        match comp::run(&spec, &|e| on_event(SeasonEvent::Episode { index, event: e })) {
            Ok(res) => {
                entries.push(viewer::SeasonEntry {
                    thumb: res.sources[0].shots.first().map(|s| format!("{ep}/{}", s.file)),
                    dir: ep.clone(),
                    frames: res.frames.len(),
                    episode: ep,
                });
                on_event(SeasonEvent::EpisodeDone { index, result: Box::new(res.clone()) });
                out.results.push(res);
            }
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(e) => {
                on_event(SeasonEvent::EpisodeFailed { index, episode: ep.clone(), error: e.to_string() });
                out.failed.push((ep, e.to_string()));
            }
        }
    }
    if !entries.is_empty() {
        std::fs::create_dir_all(&template.out_dir)?;
        let idx = viewer::write_season_index(&template.out_dir, &template.name, &entries)?;
        on_event(SeasonEvent::IndexWritten(idx.clone()));
        out.index = Some(idx);
    }
    Ok(out)
}
