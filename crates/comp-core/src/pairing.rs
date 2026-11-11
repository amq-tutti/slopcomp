//! Episode numbers from release file names, and pairing episodes across sources.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

/// Episode identity parsed from a file name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EpisodeId {
    /// "" for regular episodes, otherwise "OVA", "OAD", "SP", "NCOP", "NCED", "MOVIE", "PV", "CM".
    pub kind: String,
    pub number: Option<u32>,
}

impl EpisodeId {
    pub fn episode(n: u32) -> Self {
        Self { kind: String::new(), number: Some(n) }
    }
}

impl fmt::Display for EpisodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.kind[..], self.number) {
            ("", Some(n)) => write!(f, "E{n:02}"),
            ("", None) => write!(f, "?"),
            (k, Some(n)) => write!(f, "{k}{n}"),
            (k, None) => write!(f, "{k}"),
        }
    }
}

const SPECIAL_KINDS: [&str; 8] = ["NCOP", "NCED", "OVA", "OAD", "SP", "MOVIE", "PV", "CM"];

/// Numbers that are almost certainly not episode numbers when they stand alone.
fn is_noise_number(tok: &str) -> bool {
    matches!(tok, "480" | "540" | "576" | "720" | "1080" | "1440" | "2160" | "264" | "265") || (tok.len() == 4 && tok.starts_with("19")) || (tok.len() == 4 && tok.starts_with("20"))
}

/// Parses "05", "05v2", "5" into a number (versions ignored).
fn ep_number(tok: &str) -> Option<u32> {
    let t = tok.trim();
    let digits = match t.find(['v', 'V']) {
        Some(i) if t[i + 1..].chars().all(|c| c.is_ascii_digit()) && i > 0 => &t[..i],
        _ => t,
    };
    if digits.is_empty() || digits.len() > 4 || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// Removes `[...]`, `(...)` and `{...}` groups, returning the remaining text and the group contents.
fn split_groups(s: &str) -> (String, Vec<String>) {
    let mut outside = String::new();
    let mut groups = Vec::new();
    let mut depth = 0;
    let mut cur = String::new();
    for c in s.chars() {
        match c {
            '[' | '(' | '{' => {
                if depth == 0 {
                    outside.push(' ');
                }
                depth += 1;
            }
            ']' | ')' | '}' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    groups.push(std::mem::take(&mut cur));
                }
            }
            _ if depth > 0 => cur.push(c),
            _ => outside.push(c),
        }
    }
    (outside, groups)
}

/// Parses the episode identity from a file name (with or without extension/directory).
pub fn parse_episode(name: &str) -> Option<EpisodeId> {
    let stem = Path::new(name).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let (outside, groups) = split_groups(&stem);
    // Separators: underscores and dots are spaces in many release names.
    let norm = |s: &str| s.replace(['_', '.'], " ");
    let outside = norm(&outside);
    let upper = outside.to_uppercase();
    let tokens: Vec<&str> = upper.split_whitespace().collect();

    // 1. SxxEyy
    for t in &tokens {
        if let Some(rest) = t.strip_prefix('S') {
            if let Some((s, e)) = rest.split_once('E') {
                if !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()) {
                    if let Some(n) = ep_number(e) {
                        return Some(EpisodeId::episode(n));
                    }
                }
            }
        }
    }
    // 2. Specials: NCOP1, NCED, OVA 2, SP03, …
    for (i, t) in tokens.iter().enumerate() {
        for kind in SPECIAL_KINDS {
            if let Some(rest) = t.strip_prefix(kind) {
                if rest.is_empty() || ep_number(rest).is_some() {
                    let number = ep_number(rest).or_else(|| tokens.get(i + 1).and_then(|n| ep_number(n)));
                    return Some(EpisodeId { kind: kind.to_string(), number });
                }
            }
        }
    }
    // 3. " - 05" / " - 05v2": the most common fansub/BD-rip convention.
    for (i, t) in tokens.iter().enumerate() {
        if *t == "-" {
            if let Some(n) = tokens.get(i + 1).and_then(|n| ep_number(n)) {
                return Some(EpisodeId::episode(n));
            }
        }
    }
    // 4. "E05", "EP05", "Episode 5"
    for (i, t) in tokens.iter().enumerate() {
        for p in ["EPISODE", "EP", "E"] {
            if let Some(rest) = t.strip_prefix(p) {
                if let Some(n) = ep_number(rest) {
                    return Some(EpisodeId::episode(n));
                }
                if rest.is_empty() && p != "E" {
                    if let Some(n) = tokens.get(i + 1).and_then(|n| ep_number(n)) {
                        return Some(EpisodeId::episode(n));
                    }
                }
            }
        }
    }
    // 5. The first standalone number after the title that isn't a resolution/year/codec.
    //    The first token is skipped when there are others, so "86 - 05" style titles still work
    //    via rule 3, and "Show 2 05" picks 05.
    let nums: Vec<u32> = tokens
        .iter()
        .enumerate()
        .filter(|(i, t)| (*i > 0 || tokens.len() == 1) && !is_noise_number(t))
        .filter_map(|(_, t)| ep_number(t))
        .collect();
    if let Some(&n) = nums.last().filter(|_| nums.len() <= 2) {
        // With two candidates ("Show 2 05"), the later one is the episode.
        return Some(EpisodeId::episode(n));
    }
    // 6. A group that's only a number: "[03]".
    for g in &groups {
        if let Some(n) = ep_number(&norm(g)) {
            if !is_noise_number(g.trim()) {
                return Some(EpisodeId::episode(n));
            }
        }
    }
    None
}

/// Normalises a user-typed episode ("2", "e02", "ncop1") to the displayed form ("E02", "NCOP1").
pub fn normalize_episode(s: &str) -> String {
    let t = s.trim();
    let digits = t.strip_prefix(['e', 'E']).unwrap_or(t);
    if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
        if let Ok(n) = digits.parse::<u32>() {
            return EpisodeId::episode(n).to_string();
        }
    }
    t.to_uppercase()
}

/// One row of the pairing table: an episode and the matching file in each source (if any).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairRow {
    pub episode: EpisodeId,
    pub files: Vec<Option<PathBuf>>,
}

#[derive(Debug, Clone, Default)]
pub struct Pairing {
    pub rows: Vec<PairRow>,
    /// Files whose episode couldn't be parsed, per source.
    pub unparsed: Vec<Vec<PathBuf>>,
    /// Files dropped because another file in the same source had the same episode (e.g. v1 vs v2):
    /// (source, dropped file, kept file). The last one in name order is kept.
    pub duplicates: Vec<(usize, PathBuf, PathBuf)>,
}

impl Pairing {
    /// Rows where every source has a file.
    pub fn complete(&self) -> impl Iterator<Item = &PairRow> {
        self.rows.iter().filter(|r| r.files.iter().all(|f| f.is_some()))
    }
}

/// Pairs files across sources by parsed episode identity.
/// If every source has exactly one file, they are paired directly (no parsing needed).
pub fn pair(sources: &[Vec<PathBuf>]) -> Pairing {
    let mut out = Pairing { unparsed: vec![Vec::new(); sources.len()], ..Default::default() };
    if sources.iter().all(|s| s.len() == 1) {
        let ep = parse_episode(&sources[0][0].to_string_lossy()).unwrap_or(EpisodeId { kind: String::new(), number: None });
        out.rows.push(PairRow { episode: ep, files: sources.iter().map(|s| Some(s[0].clone())).collect() });
        return out;
    }
    let mut table: BTreeMap<EpisodeId, Vec<Option<PathBuf>>> = BTreeMap::new();
    for (si, files) in sources.iter().enumerate() {
        let mut files = files.clone();
        files.sort();
        for f in files {
            match parse_episode(&f.to_string_lossy()) {
                Some(ep) => {
                    let row = table.entry(ep).or_insert_with(|| vec![None; sources.len()]);
                    if let Some(old) = row[si].replace(f.clone()) {
                        out.duplicates.push((si, old, f));
                    }
                }
                None => out.unparsed[si].push(f),
            }
        }
    }
    out.rows = table.into_iter().map(|(episode, files)| PairRow { episode, files }).collect();
    out
}

/// Video files in a directory (non-recursive), or the file itself.
pub fn expand_input(path: &Path) -> std::io::Result<Vec<PathBuf>> {
    const EXTS: [&str; 10] = ["mkv", "mp4", "m2ts", "ts", "mpg", "mpeg", "vob", "m2v", "avi", "webm"];
    if path.is_dir() {
        let mut v: Vec<PathBuf> = std::fs::read_dir(path)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.is_file()
                    && p.extension().is_some_and(|e| EXTS.contains(&e.to_string_lossy().to_lowercase().as_str()))
                    && !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.'))
            })
            .collect();
        v.sort();
        Ok(v)
    } else {
        Ok(vec![path.to_owned()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(name: &str) -> Option<String> {
        parse_episode(name).map(|e| e.to_string())
    }

    #[test]
    fn parses_release_names() {
        let cases = [
            ("[Xanth]_To_Heart_01_DVDRip_dual_AC3_x264_10b_[72D92C94].mkv", "E01"),
            ("[ChupaCabra] To Heart - 01 [FD5B0B11].mkv", "E01"),
            ("Show.Name.S01E03.1080p.BluRay.x264-GRP.mkv", "E03"),
            ("[Group] Show Name 2 - 05v2 (BD 1080p HEVC) [ABCD1234].mkv", "E05"),
            ("[Moozzi2] Some Show - 07 (BD 1920x1080 x.264 Flac).mkv", "E07"),
            ("Show_-_04_(1080p).mkv", "E04"),
            ("Show Ep03 [1080p].mkv", "E03"),
            ("Show Episode 12.mkv", "E12"),
            ("[Group] Show [03][1080p].mkv", "E03"),
            ("[Group] Show - NCOP1 [1080p].mkv", "NCOP1"),
            ("[Group] Show - NCED [BD].mkv", "NCED"),
            ("[Group] Show OVA 2 [DVD].mkv", "OVA2"),
            ("[Group] Show - SP03.mkv", "SP3"),
            ("Show 2 05.mkv", "E05"),
            ("Show 1999 02 720p.mkv", "E02"),
            ("[SubsPlease] Frieren - 101 (1080p) [ABCDEF12].mkv", "E101"),
        ];
        for (name, want) in cases {
            assert_eq!(ep(name).as_deref(), Some(want), "{name}");
        }
        assert_eq!(ep("Some Film 1080p.mkv"), None);
    }

    #[test]
    fn pairs_episodes_across_sources() {
        let bd: Vec<PathBuf> = ["Show - 01 [BD].mkv", "Show - 02 [BD].mkv", "Show - 03 [BD].mkv"].map(PathBuf::from).into();
        let dvd: Vec<PathBuf> = ["Show_01_DVD.mkv", "Show_03_DVD.mkv", "Show_03v2_DVD.mkv", "notes.mkv"].map(PathBuf::from).into();
        let p = pair(&[bd, dvd]);
        assert_eq!(p.rows.len(), 3);
        assert_eq!(p.complete().count(), 2);
        assert!(p.rows[1].files[1].is_none(), "E02 missing on DVD");
        assert_eq!(p.duplicates.len(), 1);
        assert_eq!(p.rows[2].files[1].as_deref(), Some(Path::new("Show_03v2_DVD.mkv")));
        assert_eq!(p.unparsed[1], vec![PathBuf::from("notes.mkv")]);
    }

    #[test]
    fn normalizes_typed_episodes() {
        assert_eq!(normalize_episode("2"), "E02");
        assert_eq!(normalize_episode("e12"), "E12");
        assert_eq!(normalize_episode("E101"), "E101");
        assert_eq!(normalize_episode("ncop1"), "NCOP1");
    }

    #[test]
    fn single_files_pair_directly() {
        let p = pair(&[vec![PathBuf::from("00003.mkv")], vec![PathBuf::from("whatever.mkv")]]);
        assert_eq!(p.rows.len(), 1);
        assert_eq!(p.complete().count(), 1);
    }
}
