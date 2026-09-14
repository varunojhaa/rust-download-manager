use std::time::{Duration, Instant};

use rdm::engine::{category_for, human_bytes, Engine, Settings, Status};

#[test]
fn categories_map_by_extension() {
    let s = Settings::default();
    assert_eq!(category_for("song.mp3", &s), "Music");
    assert_eq!(category_for("clip.mkv", &s), "Video");
    assert_eq!(category_for("noext", &s), "Other");
    assert_eq!(human_bytes(2048), "2.0 KB");
}

#[test]
fn downloads_a_file_end_to_end() {
    let dir = std::env::temp_dir().join(format!("rdm-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let engine = Engine::new().unwrap();
    {
        let mut s = engine.settings.lock().unwrap();
        s.download_dir = dir.clone();
        s.sort_into_categories = false;
        s.max_parallel = 1;
        s.connections = 4;
    }
    engine.items.lock().unwrap().clear();

    let id = engine.add("https://static.rust-lang.org/dist/channel-rust-1.80.0.toml", None, None);

    let start = Instant::now();
    loop {
        engine.tick();
        let status = engine
            .items
            .lock()
            .unwrap()
            .iter()
            .find(|i| i.id == id)
            .map(|i| i.status.clone())
            .unwrap();
        if status == Status::Completed {
            break;
        }
        if let Status::Failed(err) = status {
            panic!("download failed: {err}");
        }
        assert!(start.elapsed() < Duration::from_secs(90), "timed out");
        std::thread::sleep(Duration::from_millis(200));
    }

    let item = engine.items.lock().unwrap()[0].clone();
    assert!(item.path().exists());
    assert!(std::fs::metadata(item.path()).unwrap().len() > 0);
    let _ = std::fs::remove_dir_all(&dir);
}
