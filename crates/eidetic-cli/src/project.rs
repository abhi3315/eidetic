//! Reel project files (goals-v0.8.md phase 0).
//!
//! A project file is the persisted cut list plus everything needed to
//! re-render it: JSON, written next to every rendered reel. It is the
//! object follow-up edits (`eidetic reel edit`) and, later, MCP agents
//! inspect and mutate turn by turn — without one, every instruction is a
//! full regeneration.

use crate::reel::{self, Slot};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use eidetic_ingest::beats::BeatGrid;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Bumped when the schema changes incompatibly; loaders refuse newer files.
const VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
pub struct ReelProject {
    pub version: u32,
    pub created: DateTime<Utc>,
    pub modified: DateTime<Utc>,
    /// The prompt the reel was generated from (context for follow-ups; a
    /// swap query searches fresh, it does not re-embed this).
    pub prompt: String,
    pub width: u32,
    pub height: u32,
    /// Framing style as given on the CLI: auto / cover / blur / pad.
    pub frame: String,
    /// The music (or narration) track laid under the cut.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<PathBuf>,
    /// The beat grid the cuts were synced to, kept so edits can reason
    /// about the music without re-decoding it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grid: Option<BeatGrid>,
    /// Where the last render was written.
    pub output: PathBuf,
    pub slots: Vec<Slot>,
}

impl ReelProject {
    /// Where the project file for a render at `output` lives:
    /// `reel.mp4` → `reel.eidetic.json`, always side by side.
    pub fn path_for(output: &Path) -> PathBuf {
        output.with_extension("eidetic.json")
    }

    pub fn total_secs(&self) -> f64 {
        reel::total_secs(&self.slots)
    }

    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)
            .with_context(|| format!("failed to read project file {}", path.display()))?;
        let project: ReelProject = serde_json::from_slice(&bytes)
            .with_context(|| format!("{} is not a reel project file", path.display()))?;
        if project.version > VERSION {
            anyhow::bail!(
                "{} is a version {} project; this build reads up to {}",
                path.display(),
                project.version,
                VERSION
            );
        }
        Ok(project)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_vec_pretty(self).context("serialize project")?;
        std::fs::write(path, json)
            .with_context(|| format!("failed to write project file {}", path.display()))?;
        Ok(())
    }

    /// A fresh project for a just-planned reel. Relative paths are made
    /// absolute so the file re-renders from any working directory.
    pub fn new(
        prompt: String,
        (width, height): (u32, u32),
        frame: String,
        audio: Option<PathBuf>,
        grid: Option<BeatGrid>,
        output: &Path,
        slots: Vec<Slot>,
    ) -> Self {
        let now = Utc::now();
        ReelProject {
            version: VERSION,
            created: now,
            modified: now,
            prompt,
            width,
            height,
            frame,
            audio: audio.map(|a| absolute(&a)),
            grid,
            output: absolute(output),
            slots,
        }
    }

    /// Print the cut list the way the reel command does — the shared
    /// vocabulary between renders, edits and (later) agent output.
    pub fn print_cut_list(&self) {
        println!(
            "Cut list for {:?} ({:.1}s from {} segment(s)):",
            self.prompt,
            self.total_secs(),
            self.slots.len()
        );
        for (i, slot) in self.slots.iter().enumerate() {
            println!("  {:>2}. {}", i + 1, slot.describe());
        }
    }
}

fn absolute(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reel::{SlotKind, Transition};
    use eidetic_core::AssetId;

    fn slot() -> Slot {
        Slot {
            asset: AssetId::new(),
            source: PathBuf::from("/lib/ab/cd/abcd.mp4"),
            kind: SlotKind::Video,
            start: 3.2,
            duration: 2.0,
            focus_x: Some(0.4),
            score: 0.31,
            pinned: true,
            transition_in: Transition::Crossfade { secs: 0.5 },
        }
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("reel.mp4");
        let p = ReelProject::new(
            "sunset".into(),
            (1080, 1920),
            "auto".into(),
            Some(PathBuf::from("/music/track.m4a")),
            None,
            &out,
            vec![slot()],
        );
        let path = ReelProject::path_for(&out);
        assert!(path.ends_with("reel.eidetic.json"));
        p.save(&path).unwrap();
        let back = ReelProject::load(&path).unwrap();
        assert_eq!(back.version, VERSION);
        assert_eq!(back.prompt, "sunset");
        assert_eq!(back.slots.len(), 1);
        assert_eq!(back.slots[0].asset, p.slots[0].asset);
        assert!(back.slots[0].pinned);
        assert_eq!(
            back.slots[0].transition_in,
            Transition::Crossfade { secs: 0.5 }
        );
        assert!(back.output.is_absolute());
    }

    #[test]
    fn refuses_files_from_the_future() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.eidetic.json");
        let mut p = ReelProject::new(
            "x".into(),
            (1920, 1080),
            "pad".into(),
            None,
            None,
            Path::new("/tmp/x.mp4"),
            vec![],
        );
        p.version = VERSION + 1;
        p.save(&path).unwrap();
        let err = ReelProject::load(&path).unwrap_err().to_string();
        assert!(err.contains("version"), "{err}");
    }

    #[test]
    fn rejects_non_project_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("junk.json");
        std::fs::write(&path, b"{\"hello\": 1}").unwrap();
        let err = ReelProject::load(&path).unwrap_err().to_string();
        assert!(err.contains("not a reel project"), "{err}");
    }
}
