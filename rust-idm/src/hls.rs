//! HLS (`.m3u8`) streaming support: resolves the playlist, picks the highest
//! quality variant and downloads every segment into one file.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{bail, Result};
use reqwest::Client;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;

use crate::limiter::RateLimiter;
use crate::progress::Progress;

pub fn is_hls(url: &str) -> bool {
    let path = url.split('?').next().unwrap_or(url).to_ascii_lowercase();
    path.ends_with(".m3u8") || path.ends_with(".m3u")
}

/// Turns `stream.m3u8` into `stream.ts` so players recognise the result.
pub fn output_name(name: &str) -> String {
    let base = name.split('?').next().unwrap_or(name);
    let lower = base.to_ascii_lowercase();
    if lower.ends_with(".m3u8") || lower.ends_with(".m3u") {
        let stem = &base[..base.rfind('.').unwrap_or(base.len())];
        return format!("{stem}.ts");
    }
    base.to_string()
}

pub async fn download(
    client: &Client,
    url: &str,
    output: &Path,
    progress: Progress,
    limiter: &RateLimiter,
    cancel: Arc<AtomicBool>,
) -> Result<()> {
    let (playlist_url, segments) = resolve_segments(client, url, 0).await?;
    if segments.is_empty() {
        bail!("no media segments found in {playlist_url}");
    }

    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
    }

    // HLS streams are not byte-range resumable; restart the file each run.
    let mut file = OpenOptions::new().create(true).write(true).truncate(true).open(output).await?;
    let count = segments.len() as u64;
    let mut done: u64 = 0;
    let mut bytes: u64 = 0;

    for segment in segments {
        if cancel.load(Ordering::Relaxed) {
            file.flush().await?;
            bail!("download paused");
        }
        let data = client.get(&segment).send().await?.error_for_status()?.bytes().await?;
        limiter.acquire(data.len() as u64).await;
        file.write_all(&data).await?;
        bytes += data.len() as u64;
        done += 1;
        // Estimate the final size from the average segment seen so far.
        progress.set_total(bytes / done.max(1) * count);
        progress.set_position(bytes);
        progress.set_message(format!("HLS segment {done}/{count}"));
    }

    file.flush().await?;
    progress.set_total(bytes.max(1));
    progress.set_position(bytes);
    Ok(())
}

/// Follows master playlists (max 3 levels) and returns absolute segment URLs.
async fn resolve_segments(client: &Client, url: &str, depth: usize) -> Result<(String, Vec<String>)> {
    if depth > 3 {
        bail!("too many nested playlists");
    }
    let text = client.get(url).send().await?.error_for_status()?.text().await?;
    let base = url::Url::parse(url)?;

    let mut variants: Vec<(u64, String)> = Vec::new();
    let mut segments: Vec<String> = Vec::new();
    let mut pending_bandwidth: Option<u64> = None;

    for line in text.lines().map(str::trim) {
        if line.is_empty() {
            continue;
        }
        if line.starts_with("#EXT-X-STREAM-INF") {
            pending_bandwidth = line
                .split(&[',', ':'][..])
                .find_map(|part| part.trim().strip_prefix("BANDWIDTH=")?.parse::<u64>().ok());
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        let absolute = base.join(line).map(|u| u.to_string()).unwrap_or_else(|_| line.to_string());
        match pending_bandwidth.take() {
            Some(bw) => variants.push((bw, absolute)),
            None => segments.push(absolute),
        }
    }

    if !variants.is_empty() {
        variants.sort_by_key(|(bw, _)| *bw);
        let best = variants.pop().unwrap().1;
        return Box::pin(resolve_segments(client, &best, depth + 1)).await;
    }

    Ok((url.to_string(), segments))
}
