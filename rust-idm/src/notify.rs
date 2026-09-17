//! Desktop notifications for finished and failed downloads.
//! Falls back to doing nothing when the platform has no notification service.

pub fn notify(title: &str, body: &str) {
    #[cfg(feature = "notifications")]
    {
        let _ = notify_rust::Notification::new()
            .summary(title)
            .body(body)
            .appname("rdm")
            .show();
    }
    #[cfg(not(feature = "notifications"))]
    {
        let _ = (title, body);
    }
}

/// Requests a system shutdown (used by the "when all downloads finish" action).
pub fn shutdown_system() {
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("shutdown").args(["/s", "/t", "30"]).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("osascript")
        .args(["-e", "tell app \"System Events\" to shut down"])
        .spawn();
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("systemctl").arg("poweroff").spawn();
}
