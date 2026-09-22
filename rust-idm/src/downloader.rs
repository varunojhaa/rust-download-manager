use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use futures::StreamExt;
use indicatif::MultiProgress;
use reqwest::header::{ACCEPT_RANGES, CONTENT_DISPOSITION, CONTENT_LENGTH, ETAG, RANGE};
use reqwest::{Client, StatusCode};
use tokio::fs::OpenOptions;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::sync::Mutex;

use crate::limiter::RateLimiter;
use crate::net::{self, NetConfig};
use crate::progress::Progress;
use crate::state::{DownloadState, Segment};

#[derive(Clone)]
pub struct Downloader {
    pub connections: u64,
    pub net: NetConfig,
    limiter: Arc<RateLimiter>,
    cancel: Arc<AtomicBool>,
}

pub struct RemoteInfo {
    pub total: u64,
    pub resumable: bool,
    pub etag: Option<String>,
    pub filename: String,
}

impl Downloader {
    pub fn new(connections: u64, limit_bytes_per_sec: u64, cancel: Arc<AtomicBool>) -> Result<Self> {
        Self::new_with(connections, limit_bytes_per_sec, cancel, NetConfig::default())
    }

    pub fn new_with(
        connections: u64,
        limit_bytes_per_sec: u64,
        cancel: Arc<AtomicBool>,
        net: NetConfig,
    ) -> Result<Self> {
        Ok(Self {
            connections: connections.max(1),
            net,
            limiter: Arc::new(RateLimiter::new(limit_bytes_per_sec)),
            cancel,
        })
    }

    async fn client(&self, url: &str) -> Result<Client> {
        net::build_client(&self.net, url).await
    }

    /// Ask the server for size + range support before splitting the work.
    pub async fn probe(&self, url: &str) -> Result<RemoteInfo> {
        let client = self.client(url).await?;
        self.probe_with(&client, url).await
    }

    pub async fn probe_with(&self, client: &Client, url: &str) -> Result<RemoteInfo> {
        let resp = client
            .head(url)
            .send()
            .await
            .with_context(|| format!("HEAD request failed for {url}"))?;

        let mut filename = filename_from_url(url);
        if let Some(name) = resp
            .headers()
            .get(CONTENT_DISPOSITION)
            .and_then(|v| v.to_str().ok())
            .and_then(filename_from_disposition)
        {
            filename = name;
        }

        let (total, accept_ranges, etag) = if resp.status().is_success() {
            (
                resp.headers().get(CONTENT_LENGTH).and_then(|v| v.to_str().ok()?.parse::<u64>().ok()).unwrap_or(0),
                resp.headers().get(ACCEPT_RANGES).and_then(|v| v.to_str().ok()).unwrap_or("none").to_string(),
                resp.headers().get(ETAG).and_then(|v| v.to_str().ok()).map(str::to_string),
            )
        } else {
            (0, "none".to_string(), None)
        };

        // Some servers ignore HEAD; confirm range support with a 1-byte probe.
        let mut resumable = accept_ranges.eq_ignore_ascii_case("bytes");
        let mut total = total;
        if total == 0 || !resumable {
            let probe = client.get(url).header(RANGE, "bytes=0-0").send().await?;
            if probe.status() == StatusCode::PARTIAL_CONTENT {
                resumable = true;
                if let Some(range) = probe.headers().get("content-range").and_then(|v| v.to_str().ok()) {
                    if let Some(len) = range.split('/').nth(1).and_then(|v| v.trim().parse::<u64>().ok()) {
                        total = len;
                    }
                }
            } else if !probe.status().is_success() {
                bail!("server returned {} for {url}", probe.status());
            }
        }

        Ok(RemoteInfo { total, resumable, etag, filename })
    }

    /// Replaces the cancel flag, so each GUI download can be paused on its own.
    pub fn with_cancel(&self, cancel: Arc<AtomicBool>) -> Self {
        let mut clone = self.clone();
        clone.cancel = cancel;
        clone
    }

    pub async fn download(&self, url: &str, output: &Path, multi: &MultiProgress) -> Result<PathBuf> {
        let progress = Progress::bar(multi, 0);
        self.download_with(url, output, progress).await
    }

    pub async fn download_with(&self, url: &str, output: &Path, bar: Progress) -> Result<PathBuf> {
        let client = self.client(url).await?;

        if crate::hls::is_hls(url) {
            crate::hls::download(&client, url, output, bar.clone(), &self.limiter, self.cancel.clone()).await?;
            bar.finish_with_message(format!("{}  done", output.display()));
            return Ok(output.to_path_buf());
        }

        let info = self.probe_with(&client, url).await?;

        if let Some(parent) = output.parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent).await.ok();
            }
        }

        // Resume when the sidecar still matches the remote file, else start fresh.
        let existing = DownloadState::load(output)
            .filter(|s| s.matches(url, info.total, &info.etag) && output.exists());

        let state = match existing {
            Some(state) => {
                if state.is_complete() {
                    DownloadState::clear(output);
                    return Ok(output.to_path_buf());
                }
                state
            }
            None => {
                let connections = if info.resumable && info.total > 0 { self.connections } else { 1 };
                DownloadState::new(url, info.total, info.resumable, info.etag.clone(), connections)
            }
        };

        // Preallocate so every connection can write at its own offset.
        let file = OpenOptions::new().create(true).write(true).read(true).open(output).await?;
        if info.total > 0 {
            file.set_len(info.total).await?;
        }
        drop(file);

        bar.set_total(info.total.max(1));
        bar.set_message(format!(
            "{}  x{} conn{}",
            info.filename,
            state.segments.len(),
            if state.resumable { "" } else { " (no resume support)" }
        ));
        bar.set_position(state.downloaded());

        let state = Arc::new(Mutex::new(state));
        let written = Arc::new(AtomicU64::new(0));

        // Periodically persist progress so a kill -9 still leaves a usable resume point.
        let saver = {
            let state = state.clone();
            let output = output.to_path_buf();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_millis(750)).await;
                    let snapshot = state.lock().await.clone();
                    let _ = snapshot.save(&output);
                }
            })
        };

        let segment_count = state.lock().await.segments.len();
        let mut tasks = Vec::new();
        for index in 0..segment_count {
            let this = self.clone();
            let client = client.clone();
            let state = state.clone();
            let bar = bar.clone();
            let written = written.clone();
            let output = output.to_path_buf();
            let url = url.to_string();
            tasks.push(tokio::spawn(async move {
                this.run_segment(index, &client, &url, &output, state, bar, written).await
            }));
        }

        let mut error: Option<anyhow::Error> = None;
        for task in tasks {
            match task.await {
                Ok(Ok(())) => {}
                Ok(Err(err)) => {
                    error.get_or_insert(err);
                }
                Err(err) => {
                    error.get_or_insert(anyhow!(err));
                }
            };
        }

        saver.abort();
        let final_state = state.lock().await.clone();
        final_state.save(output)?;

        if let Some(err) = error {
            bar.abandon_with_message(format!("{}  paused - rerun to resume", info.filename));
            return Err(err);
        }

        if self.cancel.load(Ordering::Relaxed) && !final_state.is_complete() {
            bar.abandon_with_message(format!("{}  paused at {} bytes", info.filename, final_state.downloaded()));
            bail!("download paused");
        }

        DownloadState::clear(output);
        bar.finish_with_message(format!("{}  done", info.filename));
        Ok(output.to_path_buf())
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_segment(
        &self,
        index: usize,
        client: &Client,
        url: &str,
        output: &Path,
        state: Arc<Mutex<DownloadState>>,
        bar: Progress,
        written: Arc<AtomicU64>,
    ) -> Result<()> {
        let (segment, resumable) = {
            let guard = state.lock().await;
            (guard.segments[index].clone(), guard.resumable)
        };

        if segment.is_complete() && resumable {
            return Ok(());
        }

        let mut request = client.get(url);
        if resumable {
            let from = segment.start + segment.downloaded;
            let range = if segment.end >= from && segment.len() > 0 {
                format!("bytes={from}-{}", segment.end)
            } else {
                format!("bytes={from}-")
            };
            request = request.header(RANGE, range);
        }

        let response = request.send().await?;
        if !response.status().is_success() && response.status() != StatusCode::PARTIAL_CONTENT {
            bail!("segment {index} failed with status {}", response.status());
        }

        let mut file = OpenOptions::new().write(true).open(output).await?;
        let mut offset = if resumable { segment.start + segment.downloaded } else { 0 };
        file.seek(std::io::SeekFrom::Start(offset)).await?;

        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            if self.cancel.load(Ordering::Relaxed) {
                file.flush().await?;
                return Ok(());
            }

            let chunk = chunk?;
            if chunk.is_empty() {
                continue;
            }
            self.limiter.acquire(chunk.len() as u64).await;
            file.write_all(&chunk).await?;
            let _ = &mut offset;

            {
                let mut guard = state.lock().await;
                guard.segments[index].downloaded += chunk.len() as u64;
            }
            bar.inc(chunk.len() as u64);
            written.fetch_add(chunk.len() as u64, Ordering::Relaxed);
        }

        file.flush().await?;

        // A non-resumable stream has unknown length; record what we actually got.
        if !resumable {
            let mut guard = state.lock().await;
            let total = guard.segments[index].downloaded;
            guard.total = total;
            guard.segments[index] = Segment { start: 0, end: total.saturating_sub(1), downloaded: total };
        }

        Ok(())
    }
}

pub fn filename_from_url(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| {
            u.path_segments()
                .and_then(|s| s.filter(|p| !p.is_empty()).last().map(str::to_string))
        })
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "download.bin".to_string())
}

/// `attachment; filename="report.pdf"` -> `report.pdf`
pub fn filename_from_disposition(value: &str) -> Option<String> {
    let lower = value.to_ascii_lowercase();
    let idx = lower.find("filename")?;
    let rest = &value[idx..];
    let raw = rest.split('=').nth(1)?.trim().trim_matches('"').trim();
    let name = raw.split(';').next()?.trim().trim_matches('"');
    let name = name.rsplit(['/', '\\']).next()?.trim();
    (!name.is_empty()).then(|| name.to_string())
}
