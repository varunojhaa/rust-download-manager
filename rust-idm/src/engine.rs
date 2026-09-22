//! Download engine behind the desktop UI: item list, categories, queues,
//! scheduling, auto-retry, clipboard capture and the worker tasks that
//! actually move bytes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::runtime::Runtime;

use crate::downloader::{filename_from_url, Downloader};
use crate::net::NetConfig;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Theme {
    Dark,
    Light,
    Black,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OnComplete {
    Nothing,
    ExitApp,
    ShutdownSystem,
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
    #[serde(default)]
    pub finished: Option<String>,
    /// Per-download cap in bytes/sec, 0 = use the global limit.
    #[serde(default)]
    pub speed_limit: u64,
    #[serde(default)]
    pub net: Option<NetConfig>,
    #[serde(default)]
    pub retries: u32,
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

    /// Seconds left at the current speed, `None` when unknown.
    pub fn eta_secs(&self) -> Option<u64> {
        if self.speed < 1.0 || self.total <= self.downloaded {
            return None;
        }
        Some(((self.total - self.downloaded) as f64 / self.speed) as u64)
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
    /// Active weekdays, Monday = 0 … Sunday = 6. Empty = every day.
    #[serde(default)]
    pub days: Vec<u8>,
}

impl Queue {
    pub fn open_now(&self) -> bool {
        if !self.enabled {
            return false;
        }
        let now = chrono::Local::now();
        if !self.days.is_empty() {
            let weekday = chrono::Datelike::weekday(&now).num_days_from_monday() as u8;
            if !self.days.contains(&weekday) {
                return false;
            }
        }
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
    #[serde(default = "default_retries")]
    pub auto_retry: u32,
    #[serde(default = "default_retry_delay")]
    pub retry_delay_secs: u64,
    #[serde(default)]
    pub clipboard_monitor: bool,
    #[serde(default = "default_clipboard_extensions")]
    pub clipboard_extensions: Vec<String>,
    #[serde(default = "default_true")]
    pub notifications: bool,
    #[serde(default = "default_theme")]
    pub theme: Theme,
    #[serde(default = "default_accent")]
    pub accent: [u8; 3],
    #[serde(default)]
    pub on_complete: Option<OnComplete>,
    #[serde(default)]
    pub net: NetConfig,
}

fn default_retries() -> u32 {
    3
}
fn default_retry_delay() -> u64 {
    10
}
fn default_true() -> bool {
    true
}
fn default_theme() -> Theme {
    Theme::Dark
}
fn default_accent() -> [u8; 3] {
    [59, 130, 246]
}
fn default_clipboard_extensions() -> Vec<String> {
    ["zip", "rar", "7z", "iso", "exe", "msi", "dmg", "pkg", "deb", "rpm", "apk", "mp3", "mp4", "mkv", "pdf", "m3u8"]
        .iter()
        .map(|s| s.to_string())
        .collect()
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
                days: Vec::new(),
            }],
            browser_port: 15080,
            browser_integration: true,
            auto_retry: default_retries(),
            retry_delay_secs: default_retry_delay(),
            clipboard_monitor: false,
            clipboard_extensions: default_clipboard_extensions(),
            notifications: true,
            theme: Theme::Dark,
            accent: default_accent(),
            on_complete: None,
            net: NetConfig::default(),
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
        c("Video", "Video", &["mp4", "mkv", "avi", "mov", "webm", "m4v", "ts", "m3u8"]),
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

/// Something the UI should surface: a toast, a desktop notification, a
/// shutdown request.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Completed(String),
    Failed(String, String),
    Added(String),
    AllDone(OnComplete),
}

pub struct Engine {
    pub items: Mutex<Vec<DownloadItem>>,
    pub settings: Mutex<Settings>,
    running: Mutex<HashMap<Id, Running>>,
    retry_at: Mutex<HashMap<Id, Instant>>,
    events: Mutex<Vec<Event>>,
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
            item.retries = 0;
        }
        let next_id = persisted.next_id.max(items.iter().map(|i| i.id + 1).max().unwrap_or(1));
        let (tx, rx) = channel();

        Ok(Arc::new(Self {
            items: Mutex::new(items),
            settings: Mutex::new(settings),
            running: Mutex::new(HashMap::new()),
            retry_at: Mutex::new(HashMap::new()),
            events: Mutex::new(Vec::new()),
            next_id: Mutex::new(next_id.max(1)),
            tx,
            rx: Mutex::new(rx),
            rt: tokio::runtime::Builder::new_multi_thread().enable_all().build()?,
        }))
    }

    // ---------------------------------------------------------------- items

    pub fn add(self: &Arc<Self>, url: &str, name: Option<String>, queue: Option<String>) -> Id {
        self.add_full(url, name, queue, None, 0)
    }

    pub fn add_full(
        self: &Arc<Self>,
        url: &str,
        name: Option<String>,
        queue: Option<String>,
        net: Option<NetConfig>,
        speed_limit: u64,
    ) -> Id {
        let settings = self.settings.lock().unwrap().clone();
        let mut name = name
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| filename_from_url(url));
        if crate::hls::is_hls(url) {
            name = crate::hls::output_name(&name);
        }
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
            finished: None,
            speed_limit,
            net,
            retries: 0,
            speed: 0.0,
        };
        let label = item.name.clone();
        self.items.lock().unwrap().push(item);
        self.push_event(Event::Added(label));
        self.save();
        id
    }

    /// True when this URL is already in the list (used by clipboard capture).
    pub fn has_url(&self, url: &str) -> bool {
        self.items.lock().unwrap().iter().any(|i| i.url == url.trim())
    }

    pub fn pause(self: &Arc<Self>, id: Id) {
        if let Some(run) = self.running.lock().unwrap().get(&id) {
            run.cancel.store(true, Ordering::Relaxed);
        }
        self.retry_at.lock().unwrap().remove(&id);
        self.set_status(id, Status::Paused);
    }

    pub fn resume(self: &Arc<Self>, id: Id) {
        self.retry_at.lock().unwrap().remove(&id);
        if let Some(item) = self.items.lock().unwrap().iter_mut().find(|i| i.id == id) {
            item.retries = 0;
        }
        self.set_status(id, Status::Queued);
    }

    pub fn retry(self: &Arc<Self>, id: Id) {
        self.resume(id);
    }

    /// Throws away the partial file so the next run starts from byte zero.
    pub fn restart(self: &Arc<Self>, id: Id) {
        self.pause(id);
        let path = self.items.lock().unwrap().iter().find(|i| i.id == id).map(|i| i.path());
        if let Some(path) = path {
            let _ = std::fs::remove_file(&path);
            DownloadState::clear(&path);
        }
        if let Some(item) = self.items.lock().unwrap().iter_mut().find(|i| i.id == id) {
            item.downloaded = 0;
            item.total = 0;
        }
        self.resume(id);
    }

    pub fn set_queue(self: &Arc<Self>, id: Id, queue: String) {
        if let Some(item) = self.items.lock().unwrap().iter_mut().find(|i| i.id == id) {
            item.queue = queue;
        }
        self.save();
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

    pub fn clear_completed(self: &Arc<Self>) {
        self.items.lock().unwrap().retain(|i| i.status != Status::Completed);
        self.save();
    }

    /// Empties the whole download list. Files already on disk are kept;
    /// when `delete_files` is set, downloaded and partial files go too.
    pub fn clear_all(self: &Arc<Self>, delete_files: bool) {
        self.pause_all();
        let mut items = self.items.lock().unwrap();
        if delete_files {
            for item in items.iter() {
                let _ = std::fs::remove_file(item.path());
                DownloadState::clear(&item.path());
            }
        }
        items.clear();
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

    // --------------------------------------------------------------- events

    fn push_event(&self, event: Event) {
        self.events.lock().unwrap().push(event);
    }

    /// Drains pending events for the UI (toasts + desktop notifications).
    pub fn take_events(&self) -> Vec<Event> {
        std::mem::take(&mut *self.events.lock().unwrap())
    }

    pub fn total_speed(&self) -> f64 {
        self.items.lock().unwrap().iter().map(|i| i.speed).sum()
    }

    // ----------------------------------------------------------- scheduling

    /// Call from the UI loop: drains worker messages, refreshes live progress
    /// and starts whatever the queue schedule now allows.
    pub fn tick(self: &Arc<Self>) {
        let settings = self.settings.lock().unwrap().clone();
        let mut finished_any = false;

        while let Ok(msg) = self.rx.lock().unwrap().try_recv() {
            match msg {
                Msg::Finished(id, Ok(())) => {
                    self.running.lock().unwrap().remove(&id);
                    let mut name = String::new();
                    let mut items = self.items.lock().unwrap();
                    if let Some(item) = items.iter_mut().find(|i| i.id == id) {
                        item.status = Status::Completed;
                        if item.total > 0 {
                            item.downloaded = item.total;
                        }
                        item.speed = 0.0;
                        item.retries = 0;
                        item.finished = Some(chrono::Local::now().format("%Y-%m-%d %H:%M").to_string());
                        name = item.name.clone();
                    }
                    drop(items);
                    finished_any = true;
                    self.push_event(Event::Completed(name));
                }
                Msg::Finished(id, Err(err)) => {
                    self.running.lock().unwrap().remove(&id);
                    let mut retry = false;
                    let mut name = String::new();
                    let mut items = self.items.lock().unwrap();
                    if let Some(item) = items.iter_mut().find(|i| i.id == id) {
                        name = item.name.clone();
                        if err.contains("paused") {
                            item.status = Status::Paused;
                        } else if item.retries < settings.auto_retry {
                            item.retries += 1;
                            item.status = Status::Queued;
                            retry = true;
                        } else {
                            item.status = Status::Failed(err.clone());
                        }
                        item.speed = 0.0;
                    }
                    drop(items);
                    if retry {
                        self.retry_at.lock().unwrap().insert(
                            id,
                            Instant::now() + Duration::from_secs(settings.retry_delay_secs.max(1)),
                        );
                    } else if !err.contains("paused") {
                        finished_any = true;
                        self.push_event(Event::Failed(name, err));
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

        // "when everything is finished, do X"
        if finished_any {
            let idle = {
                let items = self.items.lock().unwrap();
                self.running.lock().unwrap().is_empty()
                    && !items.iter().any(|i| matches!(i.status, Status::Queued | Status::Downloading))
                    && items.iter().any(|i| i.status == Status::Completed)
            };
            if idle {
                if let Some(action) = settings.on_complete.filter(|a| *a != OnComplete::Nothing) {
                    self.push_event(Event::AllDone(action));
                }
            }
        }
    }

    fn start_due(self: &Arc<Self>) {
        let settings = self.settings.lock().unwrap().clone();
        self.enforce_windows(&settings);

        let mut slots = settings.max_parallel.saturating_sub(self.running.lock().unwrap().len());
        loop {
            if slots == 0 {
                break;
            }
            let candidate = {
                let waiting = self.retry_at.lock().unwrap();
                let items = self.items.lock().unwrap();
                items
                    .iter()
                    .find(|i| {
                        i.status == Status::Queued
                            && waiting.get(&i.id).map(|at| *at <= Instant::now()).unwrap_or(true)
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
                    self.retry_at.lock().unwrap().remove(&item.id);
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
        let limit = if item.speed_limit > 0 { item.speed_limit } else { settings.speed_limit };
        let net = item.net.clone().unwrap_or_else(|| settings.net.clone());
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

            let result = match Downloader::new_with(connections, limit, cancel.clone(), net) {
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

pub fn human_eta(secs: Option<u64>) -> String {
    match secs {
        None => "-".into(),
        Some(s) if s < 60 => format!("{s}s"),
        Some(s) if s < 3600 => format!("{}m {}s", s / 60, s % 60),
        Some(s) => format!("{}h {}m", s / 3600, (s % 3600) / 60),
    }
}
