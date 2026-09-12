use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// One byte range of the file, downloaded by a dedicated connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Segment {
    pub start: u64,
    pub end: u64, // inclusive
    pub downloaded: u64,
}

impl Segment {
    pub fn len(&self) -> u64 {
        self.end - self.start + 1
    }

    pub fn remaining(&self) -> u64 {
        self.len().saturating_sub(self.downloaded)
    }

    pub fn is_complete(&self) -> bool {
        self.downloaded >= self.len()
    }
}

/// Persisted next to the output file as `<file>.rdm.json` so an interrupted
/// download can be resumed byte-exactly on the next run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadState {
    pub url: String,
    pub total: u64,
    pub resumable: bool,
    pub etag: Option<String>,
    pub segments: Vec<Segment>,
}

impl DownloadState {
    pub fn new(url: &str, total: u64, resumable: bool, etag: Option<String>, connections: u64) -> Self {
        let connections = connections.max(1);
        let mut segments = Vec::new();

        if !resumable || total == 0 {
            segments.push(Segment { start: 0, end: total.saturating_sub(1), downloaded: 0 });
        } else {
            let chunk = total / connections;
            for i in 0..connections {
                let start = i * chunk;
                let end = if i == connections - 1 { total - 1 } else { start + chunk - 1 };
                if start > end {
                    continue;
                }
                segments.push(Segment { start, end, downloaded: 0 });
            }
        }

        Self { url: url.to_string(), total, resumable, etag, segments }
    }

    pub fn sidecar_path(output: &Path) -> PathBuf {
        let mut name = output.as_os_str().to_os_string();
        name.push(".rdm.json");
        PathBuf::from(name)
    }

    pub fn load(output: &Path) -> Option<Self> {
        let raw = std::fs::read(Self::sidecar_path(output)).ok()?;
        serde_json::from_slice(&raw).ok()
    }

    /// Atomic save: write to a temp file and rename, so a crash mid-write
    /// never leaves a corrupt resume file behind.
    pub fn save(&self, output: &Path) -> Result<()> {
        let final_path = Self::sidecar_path(output);
        let tmp_path = final_path.with_extension("json.tmp");
        std::fs::write(&tmp_path, serde_json::to_vec(self)?)
            .with_context(|| format!("writing resume file {}", tmp_path.display()))?;
        std::fs::rename(&tmp_path, &final_path)?;
        Ok(())
    }

    pub fn clear(output: &Path) {
        let _ = std::fs::remove_file(Self::sidecar_path(output));
    }

    pub fn downloaded(&self) -> u64 {
        self.segments.iter().map(|s| s.downloaded).sum()
    }

    pub fn is_complete(&self) -> bool {
        self.total > 0 && self.segments.iter().all(Segment::is_complete)
    }

    /// True when the remote file still looks like the one we started.
    pub fn matches(&self, url: &str, total: u64, etag: &Option<String>) -> bool {
        if self.url != url || self.total != total {
            return false;
        }
        match (&self.etag, etag) {
            (Some(a), Some(b)) => a == b,
            _ => true,
        }
    }
}
