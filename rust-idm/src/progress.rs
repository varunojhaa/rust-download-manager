use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

/// Progress sink shared by the CLI (terminal bars) and the GUI (callbacks).
#[derive(Clone)]
pub struct Progress {
    bar: Option<ProgressBar>,
    sink: Option<Arc<dyn Fn(u64, u64) + Send + Sync>>,
    total: Arc<AtomicU64>,
    pos: Arc<AtomicU64>,
}

impl Progress {
    pub fn bar(multi: &MultiProgress, total: u64) -> Self {
        let bar = multi.add(ProgressBar::new(total.max(1)));
        bar.set_style(
            ProgressStyle::with_template(
                "{msg}\n  [{bar:38.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, eta {eta})",
            )
            .unwrap()
            .progress_chars("=>-"),
        );
        Self {
            bar: Some(bar),
            sink: None,
            total: Arc::new(AtomicU64::new(total)),
            pos: Arc::new(AtomicU64::new(0)),
        }
    }

    /// `f(total, downloaded)` is invoked whenever progress moves.
    pub fn callback(f: Arc<dyn Fn(u64, u64) + Send + Sync>) -> Self {
        Self {
            bar: None,
            sink: Some(f),
            total: Arc::new(AtomicU64::new(0)),
            pos: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn silent() -> Self {
        Self { bar: None, sink: None, total: Arc::new(AtomicU64::new(0)), pos: Arc::new(AtomicU64::new(0)) }
    }

    fn emit(&self) {
        if let Some(sink) = &self.sink {
            sink(self.total.load(Ordering::Relaxed), self.pos.load(Ordering::Relaxed));
        }
    }

    pub fn set_total(&self, total: u64) {
        self.total.store(total, Ordering::Relaxed);
        if let Some(bar) = &self.bar {
            bar.set_length(total.max(1));
        }
        self.emit();
    }

    pub fn set_position(&self, pos: u64) {
        self.pos.store(pos, Ordering::Relaxed);
        if let Some(bar) = &self.bar {
            bar.set_position(pos);
        }
        self.emit();
    }

    pub fn inc(&self, delta: u64) {
        self.pos.fetch_add(delta, Ordering::Relaxed);
        if let Some(bar) = &self.bar {
            bar.inc(delta);
        }
        self.emit();
    }

    pub fn set_message(&self, msg: String) {
        if let Some(bar) = &self.bar {
            bar.set_message(msg);
        }
    }

    pub fn finish_with_message(&self, msg: String) {
        if let Some(bar) = &self.bar {
            bar.finish_with_message(msg);
        }
        self.emit();
    }

    pub fn abandon_with_message(&self, msg: String) {
        if let Some(bar) = &self.bar {
            bar.abandon_with_message(msg);
        }
        self.emit();
    }
}
