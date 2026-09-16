//! Clipboard capture: watches the system clipboard and offers any copied
//! download link to the app, the way IDM and AB Download Manager do.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::engine::Engine;

/// Links the watcher has spotted and the user has not answered yet.
pub type Pending = Arc<Mutex<Vec<String>>>;

pub fn spawn(engine: Arc<Engine>) -> Pending {
    let pending: Pending = Arc::new(Mutex::new(Vec::new()));
    let out = pending.clone();

    std::thread::spawn(move || {
        let mut clipboard = match arboard::Clipboard::new() {
            Ok(c) => c,
            Err(err) => {
                eprintln!("clipboard monitor disabled: {err}");
                return;
            }
        };
        let mut last = String::new();
        loop {
            std::thread::sleep(Duration::from_millis(900));
            let (enabled, extensions) = {
                let settings = engine.settings.lock().unwrap();
                (settings.clipboard_monitor, settings.clipboard_extensions.clone())
            };
            if !enabled {
                continue;
            }
            let Ok(text) = clipboard.get_text() else { continue };
            let text = text.trim().to_string();
            if text == last || text.is_empty() || text.len() > 2048 {
                continue;
            }
            last = text.clone();
            if !text.starts_with("http://") && !text.starts_with("https://") {
                continue;
            }
            if !looks_downloadable(&text, &extensions) || engine.has_url(&text) {
                continue;
            }
            let mut queue = pending.lock().unwrap();
            if !queue.contains(&text) {
                queue.push(text);
            }
        }
    });

    out
}

pub fn looks_downloadable(url: &str, extensions: &[String]) -> bool {
    let path = url.split(['?', '#']).next().unwrap_or(url).to_ascii_lowercase();
    let ext = path.rsplit('.').next().unwrap_or("");
    extensions.iter().any(|e| e.eq_ignore_ascii_case(ext))
}
