//! Saved comparison setups (TOML), shared by the CLI and (later) the GUI.
//!
//! ```toml
//! name = "To Heart - BD vs DVD"
//! out = "comps"
//! frames = 20
//! seed = 2026
//!
//! [[source]]
//! label = "BD"
//! path = "/media/To Heart BD"      # file or folder of episodes
//! ivtc = "tff"                      # or deint = "auto"
//!
//! [[source]]
//! label = "DVD"
//! path = "/media/To Heart DVD"
//! offset = "-0.5s"                  # seconds ("…s") or frames
//! matrix = "601"
//! frame_type = "B"                  # only frames that are B-frames here ("BP" = B or P)
//! ```

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub name: Option<String>,
    /// Base output directory (relative paths are relative to the project file).
    #[serde(default = "default_out")]
    pub out: PathBuf,
    #[serde(default = "default_frames")]
    pub frames: usize,
    pub seed: Option<u64>,
    /// Explicit reference frames instead of random picks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frame: Vec<i32>,
    #[serde(default)]
    pub exclude_op_ed: bool,
    /// Extra excluded ranges on the reference timeline, "START-END" in seconds or [hh:]mm:ss.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
    /// Older single frame-type rule (`[frame_type] source = "ENC"`, `types = "B"`). Still read,
    /// and moved onto that source's `frame_type` on load; never written.
    #[serde(default, skip_serializing)]
    pub frame_type: Option<FrameTypeRule>,
    #[serde(default)]
    pub rules: Rules,
    /// Keep storage resolution for anamorphic sources.
    #[serde(default)]
    pub no_sar: bool,
    /// Keep black borders instead of detecting and cropping them.
    #[serde(default)]
    pub no_crop: bool,
    #[serde(rename = "source")]
    pub sources: Vec<ProjectSource>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrameTypeRule {
    pub source: String,
    /// e.g. "B" or "BP".
    pub types: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Rules {
    pub black_threshold: Option<f64>,
    pub scene_changes: bool,
    pub duplicates: bool,
    pub flat: bool,
}

impl Default for Rules {
    fn default() -> Self {
        Self { black_threshold: Some(0.03), scene_changes: true, duplicates: false, flat: false }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectSource {
    pub label: String,
    pub path: PathBuf,
    /// "Ns" for seconds or "N" for frames; several can be combined with commas ("-0.5s,2").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<String>,
    /// Per-episode offsets for season runs, same format: `episode_offsets = { E02 = "3" }`.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub episode_offsets: std::collections::BTreeMap<String, String>,
    /// "auto", "tff" or "bff".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ivtc: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matrix: Option<String>,
    /// Only pick frames whose picture type in this source is one of these, e.g. "B" or "BP".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_type: Option<String>,
}

fn default_out() -> PathBuf {
    PathBuf::from("comps")
}
fn default_frames() -> usize {
    20
}

impl Project {
    /// Loads a project. Relative `out` and source paths are resolved against the file's folder.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let mut p: Project =
            toml::from_str(&text).map_err(|e| Error::Spec(format!("{}: {e}", path.display())))?;
        let base = path.parent().unwrap_or(Path::new("."));
        if p.out.is_relative() {
            p.out = base.join(&p.out);
        }
        for s in &mut p.sources {
            if s.path.is_relative() {
                s.path = base.join(&s.path);
            }
        }
        if p.sources.len() < 2 {
            return Err(Error::Spec(format!("{}: need at least two [[source]] entries", path.display())));
        }
        if let Some(rule) = p.frame_type.take() {
            let s = p.sources.iter_mut().find(|s| s.label == rule.source).ok_or_else(|| {
                Error::Spec(format!("{}: frame_type source '{}' isn't one of the sources", path.display(), rule.source))
            })?;
            s.frame_type.get_or_insert(rule.types);
        }
        Ok(p)
    }

    /// Frame-type rules as (source label, types), for [`crate::comp::SelectOptions::frame_types`].
    pub fn frame_type_rules(&self) -> Vec<(String, Vec<char>)> {
        self.sources
            .iter()
            .filter_map(|s| {
                let types: Vec<char> = s.frame_type.as_deref()?.to_ascii_uppercase().chars().filter(|c| !c.is_whitespace()).collect();
                (!types.is_empty()).then(|| (s.label.clone(), types))
            })
            .collect()
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self).map_err(|e| Error::Spec(e.to_string()))?;
        std::fs::write(path, text)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // An absolute path on this platform (on Windows, "/abs/…" has no drive, so it's relative).
    #[cfg(windows)]
    const ABS: &str = "C:/abs/dvd.mkv";
    #[cfg(not(windows))]
    const ABS: &str = "/abs/dvd.mkv";

    #[test]
    fn roundtrip_and_relative_paths() {
        let dir = std::env::temp_dir().join("anime-comp-project-test");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("p.toml");
        std::fs::write(
            &file,
            format!(
                r#"
name = "Test"
frames = 12
seed = 7

[[source]]
label = "BD"
path = "bd"
ivtc = "tff"

[[source]]
label = "DVD"
path = "{ABS}"
offset = "-0.5s,2"
matrix = "601"
"#
            ),
        )
        .unwrap();
        let p = Project::load(&file).unwrap();
        assert_eq!(p.frames, 12);
        assert_eq!(p.sources[0].path, dir.join("bd"));
        assert_eq!(p.sources[1].path, PathBuf::from(ABS));
        assert_eq!(p.out, dir.join("comps"));
        assert_eq!(p.rules, Rules::default());
        p.save(&dir.join("p2.toml")).unwrap();
        let q = Project::load(&dir.join("p2.toml")).unwrap();
        assert_eq!(p, q);

        // The older single rule lands on its source and is written back per source.
        std::fs::write(
            &file,
            "[frame_type]\nsource = 'ENC'\ntypes = 'B'\n[[source]]\nlabel='REF'\npath='a'\nframe_type='P'\n[[source]]\nlabel='ENC'\npath='b'\n",
        )
        .unwrap();
        let p = Project::load(&file).unwrap();
        assert_eq!(p.frame_type, None);
        assert_eq!(p.frame_type_rules(), vec![("REF".into(), vec!['P']), ("ENC".into(), vec!['B'])]);
        p.save(&dir.join("p3.toml")).unwrap();
        let text = std::fs::read_to_string(dir.join("p3.toml")).unwrap();
        assert!(!text.contains("[frame_type]") && text.matches("frame_type =").count() == 2, "{text}");
        assert_eq!(Project::load(&dir.join("p3.toml")).unwrap(), p);

        std::fs::write(&file, "frames = 3\nbogus = 1\n[[source]]\nlabel='a'\npath='x'\n").unwrap();
        assert!(Project::load(&file).is_err(), "unknown keys are rejected");
    }
}
