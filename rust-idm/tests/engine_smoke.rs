use rdm::downloader::filename_from_disposition;
use rdm::engine::{category_for, human_bytes, human_eta, Settings};
use rdm::hls::{is_hls, output_name};
use rdm::net::NetConfig;

#[test]
fn categories_match_extensions() {
    let settings = Settings::default();
    assert_eq!(category_for("song.mp3", &settings), "Music");
    assert_eq!(category_for("clip.mkv", &settings), "Video");
    assert_eq!(category_for("noext", &settings), "Other");
}

#[test]
fn human_helpers() {
    assert_eq!(human_bytes(2048), "2.0 KB");
    assert_eq!(human_eta(Some(90)), "1m 30s");
    assert_eq!(human_eta(None), "-");
}

#[test]
fn hls_detection_and_naming() {
    assert!(is_hls("https://x.test/stream.m3u8?token=1"));
    assert!(!is_hls("https://x.test/file.zip"));
    assert_eq!(output_name("stream.m3u8"), "stream.ts");
    assert_eq!(output_name("movie.mp4"), "movie.mp4");
}

#[test]
fn content_disposition_filename() {
    assert_eq!(
        filename_from_disposition("attachment; filename=\"report 2026.pdf\""),
        Some("report 2026.pdf".to_string())
    );
    assert_eq!(filename_from_disposition("inline"), None);
}

#[test]
fn net_config_builds_headers() {
    let cfg = NetConfig {
        referer: Some("https://example.com".into()),
        cookie: Some("a=b".into()),
        headers: vec![("X-Token".into(), "42".into())],
        ..Default::default()
    };
    let map = cfg.header_map();
    assert_eq!(map.get("referer").unwrap(), "https://example.com");
    assert_eq!(map.get("cookie").unwrap(), "a=b");
    assert_eq!(map.get("x-token").unwrap(), "42");
}

#[tokio::test]
async fn downloads_a_real_file() {
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    let dir = std::env::temp_dir().join("rdm-test");
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("channel.toml");
    let _ = std::fs::remove_file(&out);

    let dl = rdm::downloader::Downloader::new(4, 0, Arc::new(AtomicBool::new(false))).unwrap();
    let path = dl
        .download_with(
            "https://static.rust-lang.org/dist/channel-rust-1.80.0.toml",
            &out,
            rdm::progress::Progress::silent(),
        )
        .await
        .unwrap();
    assert!(std::fs::metadata(path).unwrap().len() > 1000);
}
