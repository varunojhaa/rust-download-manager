mod downloader;
mod import;
mod limiter;
mod queue;
mod state;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use indicatif::MultiProgress;

use crate::downloader::{filename_from_url, Downloader};
use crate::import::{choose_jobs, parse_link_file, write_queue_file};
use crate::queue::{parse_queue_file, run_queue, Job};
use crate::state::DownloadState;

#[derive(Parser)]
#[command(
    name = "rdm",
    version,
    about = "rdm - a Rust download manager: segmented downloads, pause/resume, queue & scheduling"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Download one or more URLs right now.
    Get {
        urls: Vec<String>,
        /// Output file (single URL) or directory (multiple URLs).
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Connections per file.
        #[arg(short = 'c', long, default_value_t = 8)]
        connections: u64,
        /// Files downloaded at the same time.
        #[arg(short = 'j', long, default_value_t = 2)]
        parallel: usize,
        /// Global speed cap, e.g. 2M, 500k. 0 = unlimited.
        #[arg(long, default_value = "0")]
        limit: String,
    },
    /// Download every URL listed in a queue file.
    Queue {
        /// Queue file: one "<url> [filename]" per line.
        file: PathBuf,
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
        #[arg(short = 'c', long, default_value_t = 8)]
        connections: u64,
        #[arg(short = 'j', long, default_value_t = 2)]
        parallel: usize,
        #[arg(long, default_value = "0")]
        limit: String,
        /// Start at a wall-clock time today/tomorrow, e.g. --at 02:30
        #[arg(long)]
        at: Option<String>,
    },
    /// Import links from a .txt file, tick the ones you want, then queue them.
    Import {
        /// Text file containing http/https links (one per line, or mixed in text).
        file: PathBuf,
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
        #[arg(short = 'c', long, default_value_t = 8)]
        connections: u64,
        #[arg(short = 'j', long, default_value_t = 2)]
        parallel: usize,
        #[arg(long, default_value = "0")]
        limit: String,
        /// Start the queue at a wall-clock time, e.g. --at 02:30
        #[arg(long)]
        at: Option<String>,
        /// Skip the selection window and take every link.
        #[arg(long)]
        all: bool,
        /// Save the picked links to a queue file instead of downloading now.
        #[arg(long)]
        save: Option<PathBuf>,
    },
    /// Show saved progress for a partially downloaded file.
    Status {
        /// The output file whose resume data should be inspected.
        file: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Ctrl-C pauses cleanly: workers stop, resume data is flushed to disk.
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let cancel = cancel.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                eprintln!("\npausing - progress saved, rerun the same command to resume");
                cancel.store(true, Ordering::Relaxed);
            }
        });
    }

    match cli.command {
        Command::Get { urls, output, connections, parallel, limit } => {
            if urls.is_empty() {
                bail!("give at least one URL");
            }
            let limit = parse_size(&limit)?;
            let downloader = Downloader::new(connections, limit, cancel.clone())?;

            let jobs: Vec<Job> = if urls.len() == 1 {
                let out = match output {
                    Some(p) if p.is_dir() => p.join(filename_from_url(&urls[0])),
                    Some(p) => p,
                    None => PathBuf::from(filename_from_url(&urls[0])),
                };
                vec![Job { url: urls[0].clone(), output: out }]
            } else {
                let dir = output.unwrap_or_else(|| PathBuf::from("."));
                urls.iter()
                    .map(|u| Job { url: u.clone(), output: dir.join(filename_from_url(u)) })
                    .collect()
            };

            let report = run_queue(downloader, jobs, parallel, cancel).await?;
            print_report(report.completed, &report.failed);
        }

        Command::Queue { file, dir, connections, parallel, limit, at } => {
            let jobs = parse_queue_file(&file, &dir)?;
            if jobs.is_empty() {
                bail!("queue file has no jobs");
            }
            if let Some(at) = at {
                wait_until(&at).await?;
            }
            let limit = parse_size(&limit)?;
            let downloader = Downloader::new(connections, limit, cancel.clone())?;
            println!("starting {} job(s), {parallel} at a time", jobs.len());
            let report = run_queue(downloader, jobs, parallel, cancel).await?;
            print_report(report.completed, &report.failed);
        }

        Command::Status { file } => match DownloadState::load(&file) {
            None => println!("no resume data for {}", file.display()),
            Some(state) => {
                let done = state.downloaded();
                let pct = if state.total > 0 { done as f64 / state.total as f64 * 100.0 } else { 0.0 };
                println!("{}", state.url);
                println!("  {done} / {} bytes ({pct:.1}%)", state.total);
                println!("  segments: {}  resumable: {}", state.segments.len(), state.resumable);
                for (i, seg) in state.segments.iter().enumerate() {
                    println!("    #{i}: {}-{} {}/{}", seg.start, seg.end, seg.downloaded, seg.len());
                }
            }
        },
    }

    // Keep the multi-progress drawing target tidy on exit.
    let _ = MultiProgress::new();
    Ok(())
}

fn print_report(completed: usize, failed: &[(String, String)]) {
    println!("\n{completed} completed, {} failed", failed.len());
    for (url, err) in failed {
        println!("  {url}: {err}");
    }
}

/// Accepts plain bytes or suffixed sizes: 800k, 2M, 1G.
fn parse_size(input: &str) -> Result<u64> {
    let raw = input.trim().to_lowercase();
    if raw.is_empty() {
        return Ok(0);
    }
    let (number, factor) = match raw.chars().last().unwrap() {
        'k' => (&raw[..raw.len() - 1], 1024u64),
        'm' => (&raw[..raw.len() - 1], 1024 * 1024),
        'g' => (&raw[..raw.len() - 1], 1024 * 1024 * 1024),
        _ => (raw.as_str(), 1),
    };
    let value: f64 = number.trim().parse().map_err(|_| anyhow::anyhow!("invalid size: {input}"))?;
    Ok((value * factor as f64) as u64)
}

/// Sleeps until the next occurrence of a local HH:MM wall-clock time.
async fn wait_until(hhmm: &str) -> Result<()> {
    let (h, m) = hhmm
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("time must look like HH:MM"))?;
    let hour: u64 = h.trim().parse()?;
    let minute: u64 = m.trim().parse()?;
    if hour > 23 || minute > 59 {
        bail!("time must be between 00:00 and 23:59");
    }

    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
    let seconds_today = now % 86_400; // UTC seconds since midnight
    let target = hour * 3600 + minute * 60;
    let wait = if target > seconds_today { target - seconds_today } else { 86_400 - seconds_today + target };

    println!("scheduled: waiting {} minute(s) until {hhmm} UTC", wait / 60);
    tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
    Ok(())
}
