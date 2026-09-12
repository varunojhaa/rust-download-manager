use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use futures::stream::{self, StreamExt};
use indicatif::MultiProgress;

use crate::downloader::{filename_from_url, Downloader};

#[derive(Debug, Clone)]
pub struct Job {
    pub url: String,
    pub output: PathBuf,
}

/// Queue file format, one job per line:
///   https://example.com/file.iso
///   https://example.com/file.iso   custom-name.iso
/// Blank lines and `#` comments are ignored.
pub fn parse_queue_file(path: &Path, dir: &Path) -> Result<Vec<Job>> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading queue file {}", path.display()))?;

    let mut jobs = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let url = parts.next().unwrap().to_string();
        let name = parts.next().map(str::to_string).unwrap_or_else(|| filename_from_url(&url));
        jobs.push(Job { url, output: dir.join(name) });
    }
    Ok(jobs)
}

pub struct QueueReport {
    pub completed: usize,
    pub failed: Vec<(String, String)>,
}

/// Runs jobs with at most `parallel` downloads in flight at a time.
pub async fn run_queue(
    downloader: Downloader,
    jobs: Vec<Job>,
    parallel: usize,
    cancel: Arc<AtomicBool>,
) -> Result<QueueReport> {
    let multi = MultiProgress::new();
    let parallel = parallel.max(1);

    let results = stream::iter(jobs.into_iter().map(|job| {
        let downloader = downloader.clone();
        let multi = multi.clone();
        let cancel = cancel.clone();
        async move {
            if cancel.load(Ordering::Relaxed) {
                return (job.url.clone(), Err(anyhow::anyhow!("cancelled before start")));
            }
            let result = downloader.download(&job.url, &job.output, &multi).await.map(|_| ());
            (job.url, result)
        }
    }))
    .buffer_unordered(parallel)
    .collect::<Vec<_>>()
    .await;

    let mut completed = 0;
    let mut failed = Vec::new();
    for (url, result) in results {
        match result {
            Ok(()) => completed += 1,
            Err(err) => failed.push((url, err.to_string())),
        }
    }

    Ok(QueueReport { completed, failed })
}
