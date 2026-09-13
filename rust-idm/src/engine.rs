//! Download engine behind the desktop UI: item list, categories, queues,
//! scheduling and the worker tasks that actually move bytes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::runtime::Runtime;

use crate::downloader::{filename_from_url, Downloader};
use crate::progress::Progress;
use crate::state::DownloadState;

pub type Id = u64;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    Queued,
    Downloading,
    Paused,
    Completed,
    Failed(String),
}

impl Status {
    pub fn label(&self) -> String {
        match self {
            Status::Queued => "Queued".into(),
            Status::Downloading => "Downloading".into(),
            Status::Paused => "Paused".into(),
            Status::Completed => "Completed".into(),
            Status::Failed(e) => format!("Failed: {e}"),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DownloadItem {
    pub id: Id,
    pub url: String,
    pub name: String,
    pub folder: PathBuf,
    pub category: String,
    pub queue: String,
    pub total: u64,
    pub downloaded: u64,
    pub status: Status,
    pub connections: u64,
    pub added: String,
    #[serde(skip)]
    pub speed: f64,
}

impl DownloadItem {
    pub fn path(&self) -> PathBuf {
        self.folder.join(&self.name)
    }

    pub fn percent(&self) -> f32 {
        if self.total == 0 {
            return 0.0;
        }
        (self.downloaded as f32 / self.total as f32).clamp(0.0, 1.0)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Category {
    pub name: String,
    pub folder: String,
    pub extensions: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Queue {
    pub name: String,
    /// `HH:MM` local start time; downloads wait until then.
    pub start_at: Option<String>,
    /// `HH:MM` local stop time; running downloads pause after it.
    pub stop_at: Option<String>,
    pub enabled: bool,
}

impl Queue {
    pub fn open_now(&self) -> bool {
        if !self.enabled {
            return false;
        }
        let now = chrono::Local::now();
        let minutes = now.format("%H:%M").to_string();
        let after = self.start_at.as_ref().map(|s| minutes.as_str() >= s.as_str()).unwrap_or(true);
        let before = self.stop_at.as_ref().map(|s| minutes.as_str() < s.as_str()).unwrap_or(true);
        after && before
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Settings {
    pub download_dir: PathBuf,
    pub max_parallel: usize,
    pub connections: u64,
    /// Global speed cap in bytes/sec, 0 = unlimited.
    pub speed_limit: u64,
    pub sort_into_categories: bool,
    pub categories: Vec<Category>,
    pub queues: Vec<Queue>,
    pub browser_port: u16,
    pub browser_integration: bool,
}

impl Default for Settings {
    fn default() -> Self {
        let download_dir = dirs::download_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."));
        Self {
            download_dir,
            max_parallel: 2,
            connections: 8,
            speed_limit: 0,
            sort_into_categories: true,
            categories: default_categories(),
            queues: vec![Queue {
                name: "Main".into(),
                start_at: None,
                stop_at: None,
                enabled: true,
            }],
            browser_port: 15080,
            browser_integration: true,
        }
    }
}

pub fn default_categories() -> Vec<Category> {
    let c = |name: &str, folder: &str, ext: &[&str]| Category {
        name: name.into(),
        folder: folder.into(),
        extensions: ext.iter().map(|s| s.to_string()).collect(),
    };
    vec![
        c("Music", "Music", &["mp3", "flac", "wav", "aac", "ogg", "m4a"]),
        c("Video", "Video", &["mp4", "mkv", "avi", "mov", "webm", "m4v"]),
        c("Documents", "Documents", &["pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "epub"]),
        c("Compressed", "Compressed", &["zip", "rar", "7z", "tar", "gz", "xz", "bz2"]),
        c("Programs", "Programs", &["exe", "msi", "dmg", "pkg", "deb", "rpm", "appimage", "apk"]),
        c("Other", "Other", &[]),
    ]
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Persisted {
    #[serde(default)]
    pub items: Vec<DownloadItem>,
    #[serde(default)]
    pub next_id: Id,
    #[serde(default)]
    pub settings: Option<Settings>,
}

struct Running {
    downloaded: Arc<AtomicU64>,
    total: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
    last_bytes: u64,
    last_tick: Instant,
}

enum Msg {
    Finished(Id, Result<(), String>),
}

pub struct Engine {
    pub items: Mutex<Vec<DownloadItem>>,
    pub settings: Mutex<Settings>,
    running: Mutex<HashMap<Id, Running>>,
    next_id: Mutex<Id>,
    tx: Sender<Msg>,
    rx: Mutex<Receiver<Msg>>,
    rt: Runtime,
}

impl Engine {
    pub fn new() -> Result<Arc<Self>> {
        let persisted = load_persisted().unwrap_or_default();
        let settings = persisted.settings.clone().unwrap_or_default();
        let mut items = persisted.items;
        // Nothing survives a restart mid-flight; show those as paused.
        for item in &mut items {
            if item.status == Status::Downloading {
                item.status = Status::Paused;
            }
            item.speed = 0.0;
        }
        let next_id = persisted.next_id.max(items.iter().map(|i| i.id + 1).max().unwrap_or(1));
        let (tx, rx) = channel();

        Ok(Arc::new(Self {
            items: Mutex::new(items),
            settings: Mutex::new(settings),
            running: Mutex::new(HashMap::new()),
            next_id: Mutex::new(next_id.max(1)),
            tx,
            rx: Mutex::new(rx),
            rt: tokio::runtime::Builder::new_multi_thread().enable_all().build()?,
        }))
    }

    // ---------------------------------------------------------------- items

    pub fn add(self: &Arc<Self>, url: &str, name: Option<String>, queue: Option<String>) -> Id {
        let settings = self.settings.lock().unwrap().clone();
        let name = name
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| filename_from_url(url));
        let category = category_for(&name, &settings);
        let folder = folder_for(&category, &settings);
        let queue = queue.unwrap_or_else(|| {
            settings.queues.first().map(|q| q.name.clone()).unwrap_or_else(|| "Main".into())
        });

        let mut id_guard = self.next_id.lock().unwrap();
        let id = *id_guard;
        *id_guard += 1;
        drop(id_guard);

        let item = DownloadItem {
            id,
            url: url.trim().to_string(),
            name: unique_name(&folder, &name),
            folder,
            category,
            queue,
            total: 0,
            downloaded: 0,
            status: Status::Queued,
            connections: settings.connections,
            added: chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
            speed: 0.0,
        };
        self.items.lock().unwrap().push(item);
        self.save();
        id
    }

    pub fn pause(self: &Arc<Self>, id: Id) {
        if let Some(run) = self.running.lock().unwrap().get(&id) {
            run.cancel.store(true, Ordering::Relaxed);
        }
        self.set_status(id, Status::Paused);
    }

    pub fn resume(self: &Arc<Self>, id: Id) {
        self.set_status(id, Status::Queued);
    }

    pub fn retry(self: &Arc<Self>, id: Id) {
        self.resume(id);
    }

    pub fn pause_all(self: &Arc<Self>) {
        let ids: Vec<Id> = self
            .items
            .lock()
            .unwrap()
            .iter()
            .filter(|i| matches!(i.status, Status::Downloading | Status::Queued))
            .map(|i| i.id)
            .collect();
        for id in ids {
            self.pause(id);
        }
    }

    pub fn resume_all(self: &Arc<Self>) {
        let ids: Vec<Id> = self
            .items
            .lock()
            .unwrap()
            .iter()
            .filter(|i| matches!(i.status, Status::Paused | Status::Failed(_)))
            .map(|i| i.id)
            .collect();
        for id in ids {
            self.resume(id);
        }
    }

    pub fn remove(self: &Arc<Self>, id: Id, delete_file: bool) {
        self.pause(id);
        let mut items = self.items.lock().unwrap();
        if let Some(pos) = items.iter().position(|i| i.id == id) {
            let item = items.remove(pos);
            if delete_file {
                let _ = std::fs::remove_file(item.path());
                DownloadState::clear(&item.path());
            }
        }
        drop(items);
        self.save();
    }

    fn set_status(self: &Arc<Self>, id: Id, status: Status) {
        let mut items = self.items.lock().unwrap();
        if let Some(item) = items.iter_mut().find(|i| i.id == id) {
            if item.status != Status::Completed || status == Status::Queued {
                item.status = status;
                item.speed = 0.0;
            }
        }
        drop(items);
        self.save();
    }

    // ----------------------------------------------------------- scheduling

    /// Call from the UI loop: drains worker messages, refreshes live progress
    /// and starts whatever the queue schedule now allows.
    pub fn tick(self: &Arc<Self>) {
        while let Ok(msg) = self.rx.lock().unwrap().try_recv() {
            match msg {
                Msg::Finished(id, Ok(())) => {
                    self.running.lock().unwrap().remove(&id);
                    let mut items = self.items.lock().unwrap();
                    if let Some(item) = items.iter_mut().find(|i| i.id == id) {
                        item.status = Status::Completed;
                        item.downloaded = item.total;
                        item.speed = 0.0;
                    }
                }
                Msg::Finished(id, Err(err)) => {
                    self.running.lock().unwrap().remove(&id);
                    let mut items = self.items.lock().unwrap();
                    if let Some(item) = items.iter_mut().find(|i| i.id == id) {
                        item.status = if err.contains("paused") {
                            Status::Paused
                        } else {
                            Status::Failed(err)
                        };
                        item.speed = 0.0;
                    }
                }
            }
        }

        // live bytes + speed
        {
            let mut running = self.running.lock().unwrap();
            let mut items = self.items.lock().unwrap();
            for (id, run) in running.iter_mut() {
                if let Some(item) = items.iter_mut().find(|i| i.id == *id) {
                    let now = run.downloaded.load(Ordering::Relaxed);
                    let total = run.total.load(Ordering::Relaxed);
                    if total > 0 {
                        item.total = total;
                    }
                    item.downloaded = now;
                    let elapsed = run.last_tick.elapsed().as_secs_f64();
                    if elapsed >= 0.5 {
                        item.speed = (now.saturating_sub(run.last_bytes)) as f64 / elapsed;
                        run.last_bytes = now;
                        run.last_tick = Instant::now();
                    }
                }
            }
        }

        self.start_due();
    }

    fn start_due(self: &Arc<Self>) {
        let settings = self.settings.lock().unwrap().clone();
        let active = self.running.lock().unwrap().len();
        if active >= settings.max_parallel {
            // Queue windows can close mid-download; pause what is out of window.
            self.enforce_windows(&settings);
            return;
        }
        self.enforce_windows(&settings);

        let mut slots = settings.max_parallel.saturating_sub(self.running.lock().unwrap().len());
        loop {
            if slots == 0 {
                break;
            }
            let candidate = {
                let items = self.items.lock().unwrap();
                items
                    .iter()
                    .find(|i| {
                        i.status == Status::Queued
                            && settings
                                .queues
                                .iter()
                                .find(|q| q.name == i.queue)
                                .map(|q| q.open_now())
                                .unwrap_or(true)
                    })
                    .cloned()
            };
            match candidate {
                Some(item) => {
                    self.spawn(item, &settings);
                    slots -= 1;
                }
                None => break,
            }
        }
    }

    fn enforce_windows(self: &Arc<Self>, settings: &Settings) {
        let closed: Vec<Id> = {
            let items = self.items.lock().unwrap();
            items
                .iter()
                .filter(|i| i.status == Status::Downloading)
                .filter(|i| {
                    settings
                        .queues
                        .iter()
                        .find(|q| q.name == i.queue)
                        .map(|q| !q.open_now())
                        .unwrap_or(false)
                })
                .map(|i| i.id)
                .collect()
        };
        for id in closed {
            self.pause(id);
        }
    }

    fn spawn(self: &Arc<Self>, item: DownloadItem, settings: &Settings) {
        let downloaded = Arc::new(AtomicU64::new(item.downloaded));
        let total = Arc::new(AtomicU64::new(item.total));
        let cancel = Arc::new(AtomicBool::new(false));

        self.running.lock().unwrap().insert(
            item.id,
            Running {
                downloaded: downloaded.clone(),
                total: total.clone(),
                cancel: cancel.clone(),
                last_bytes: item.downloaded,
                last_tick: Instant::now(),
            },
        );

        {
            let mut items = self.items.lock().unwrap();
            if let Some(entry) = items.iter_mut().find(|i| i.id == item.id) {
                entry.status = Status::Downloading;
            }
        }

        let tx = self.tx.clone();
        let connections = item.connections.max(1);
        let limit = settings.speed_limit;
        let url = item.url.clone();
        let path = item.path();
        let id = item.id;

        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        self.rt.spawn(async move {
            let progress = Progress::callback(Arc::new(move |t, d| {
                total.store(t, Ordering::Relaxed);
                downloaded.store(d, Ordering::Relaxed);
            }));

            let result = match Downloader::new(connections, limit, cancel.clone()) {
                Ok(dl) => dl
                    .download_with(&url, &path, progress)
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            };

            let result = match result {
                Err(e) if cancel.load(Ordering::Relaxed) => Err(format!("paused ({e})")),
                other => other,
            };
            let _ = tx.send(Msg::Finished(id, result));
        });
    }

    // -------------------------------------------------------------- storage

    pub fn save(&self) {
        let data = Persisted {
            items: self.items.lock().unwrap().clone(),
            next_id: *self.next_id.lock().unwrap(),
            settings: Some(self.settings.lock().unwrap().clone()),
        };
        if let Some(path) = config_path() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(json) = serde_json::to_string_pretty(&data) {
                let _ = std::fs::write(path, json);
            }
        }
    }

    pub fn runtime(&self) -> tokio::runtime::Handle {
        self.rt.handle().clone()
    }
}

pub fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("rdm").join("state.json"))
}

fn load_persisted() -> Option<Persisted> {
    let path = config_path()?;
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

pub fn category_for(name: &str, settings: &Settings) -> String {
    let ext = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    settings
        .categories
        .iter()
        .find(|c| c.extensions.iter().any(|e| e.eq_ignore_ascii_case(&ext)))
        .map(|c| c.name.clone())
        .unwrap_or_else(|| "Other".to_string())
}

pub fn folder_for(category: &str, settings: &Settings) -> PathBuf {
    if !settings.sort_into_categories {
        return settings.download_dir.clone();
    }
    let sub = settings
        .categories
        .iter()
        .find(|c| c.name == category)
        .map(|c| c.folder.clone())
        .unwrap_or_else(|| "Other".into());
    settings.download_dir.join(sub)
}

/// Avoid clobbering an unrelated file that already sits in the target folder.
fn unique_name(folder: &Path, name: &str) -> String {
    let candidate = folder.join(name);
    if !candidate.exists() || DownloadState::load(&candidate).is_some() {
        return name.to_string();
    }
    let stem = Path::new(name).file_stem().and_then(|s| s.to_str()).unwrap_or(name);
    let ext = Path::new(name).extension().and_then(|s| s.to_str());
    for n in 1..1000 {
        let next = match ext {
            Some(ext) => format!("{stem} ({n}).{ext}"),
            None => format!("{stem} ({n})"),
        };
        if !folder.join(&next).exists() {
            return next;
        }
    }
    name.to_string()
}

pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn human_speed(bytes_per_sec: f64) -> String {
    if bytes_per_sec <= 1.0 {
        return "-".into();
    }
    format!("{}/s", human_bytes(bytes_per_sec as u64))
}
