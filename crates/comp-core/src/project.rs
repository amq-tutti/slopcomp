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
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
        Ok(p)
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

        std::fs::write(&file, "frames = 3\nbogus = 1\n[[source]]\nlabel='a'\npath='x'\n").unwrap();
        assert!(Project::load(&file).is_err(), "unknown keys are rejected");
    }
}
