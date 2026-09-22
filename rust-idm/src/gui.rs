//! egui desktop front-end: downloads list, categories, queues, settings,
//! link import, clipboard capture and per-download network options.

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui::{self, Color32, RichText};

use crate::clipboard::Pending;
use crate::engine::{
    human_bytes, human_eta, human_speed, Category, Engine, Event, Id, OnComplete, Queue, Status, Theme,
};
use crate::import::parse_link_file;
use crate::net::NetConfig;

pub fn run() -> eframe::Result<()> {
    let engine = Engine::new().expect("engine");
    let browser = {
        let settings = engine.settings.lock().unwrap();
        settings.browser_integration.then_some(settings.browser_port)
    };
    if let Some(port) = browser {
        crate::browser::spawn(engine.clone(), port);
    }
    let pending_clipboard = crate::clipboard::spawn(engine.clone());

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 700.0])
            .with_min_inner_size([760.0, 420.0])
            .with_title("rdm - Download Manager"),
        ..Default::default()
    };
    eframe::run_native(
        "rdm",
        options,
        Box::new(|_cc| Ok(Box::new(App::new(engine, pending_clipboard)))),
    )
}

#[derive(PartialEq, Clone)]
enum Filter {
    All,
    Unfinished,
    Finished,
    Failed,
    Category(String),
    Queue(String),
}

#[derive(PartialEq, Clone, Copy)]
enum Sort {
    Added,
    Name,
    Size,
    Status,
}

struct ImportRow {
    url: String,
    name: String,
    selected: bool,
}

/// Editable copy of one download's network options.
struct Details {
    id: Id,
    referer: String,
    cookie: String,
    user_agent: String,
    headers: String,
    speed_limit_kb: u64,
}

struct App {
    engine: Arc<Engine>,
    pending_clipboard: Pending,
    filter: Filter,
    sort: Sort,
    search: String,
    show_add: bool,
    show_settings: bool,
    show_import: bool,
    confirm_clear_all: bool,
    add_urls: String,
    add_name: String,
    add_queue: String,
    add_referer: String,
    add_cookie: String,
    import_path: String,
    import_rows: Vec<ImportRow>,
    details: Option<Details>,
    status: String,
    toasts: Vec<String>,
    theme_applied: Option<(Theme, [u8; 3])>,
}

impl App {
    fn new(engine: Arc<Engine>, pending_clipboard: Pending) -> Self {
        let add_queue = engine
            .settings
            .lock()
            .unwrap()
            .queues
            .first()
            .map(|q| q.name.clone())
            .unwrap_or_else(|| "Main".into());
        Self {
            engine,
            pending_clipboard,
            filter: Filter::All,
            sort: Sort::Added,
            search: String::new(),
            show_add: false,
            show_settings: false,
            show_import: false,
            confirm_clear_all: false,
            add_urls: String::new(),
            add_name: String::new(),
            add_queue,
            add_referer: String::new(),
            add_cookie: String::new(),
            import_path: String::new(),
            import_rows: Vec::new(),
            details: None,
            status: String::new(),
            toasts: Vec::new(),
            theme_applied: None,
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.engine.tick();
        self.apply_theme(ctx);
        self.drain_events(ctx);
        ctx.request_repaint_after(std::time::Duration::from_millis(400));

        self.top_bar(ctx);
        self.side_bar(ctx);
        self.list(ctx);
        self.add_window(ctx);
        self.import_window(ctx);
        self.details_window(ctx);
        self.clipboard_window(ctx);
        self.settings_window(ctx);
        self.toast_area(ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.engine.pause_all();
        self.engine.save();
    }
}

impl App {
    // ---------------------------------------------------------------- chrome

    fn apply_theme(&mut self, ctx: &egui::Context) {
        let (theme, accent) = {
            let s = self.engine.settings.lock().unwrap();
            (s.theme, s.accent)
        };
        if self.theme_applied == Some((theme, accent)) {
            return;
        }
        self.theme_applied = Some((theme, accent));

        let mut visuals = match theme {
            Theme::Light => egui::Visuals::light(),
            _ => egui::Visuals::dark(),
        };
        if theme == Theme::Black {
            visuals.panel_fill = Color32::from_rgb(8, 8, 10);
            visuals.window_fill = Color32::from_rgb(12, 12, 14);
            visuals.extreme_bg_color = Color32::BLACK;
        }
        let accent = Color32::from_rgb(accent[0], accent[1], accent[2]);
        visuals.selection.bg_fill = accent.linear_multiply(0.55);
        visuals.hyperlink_color = accent;
        visuals.widgets.hovered.bg_stroke.color = accent;
        ctx.set_visuals(visuals);
    }

    fn drain_events(&mut self, ctx: &egui::Context) {
        let notifications = self.engine.settings.lock().unwrap().notifications;
        for event in self.engine.take_events() {
            match event {
                Event::Completed(name) => {
                    self.toasts.push(format!("Finished: {name}"));
                    if notifications {
                        crate::notify::notify("Download finished", &name);
                    }
                }
                Event::Failed(name, err) => {
                    self.toasts.push(format!("Failed: {name} — {err}"));
                    if notifications {
                        crate::notify::notify("Download failed", &format!("{name}: {err}"));
                    }
                }
                Event::Added(name) => self.toasts.push(format!("Added: {name}")),
                Event::AllDone(action) => match action {
                    OnComplete::ExitApp => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                    OnComplete::ShutdownSystem => crate::notify::shutdown_system(),
                    OnComplete::Nothing => {}
                },
            }
        }
        if self.toasts.len() > 4 {
            let extra = self.toasts.len() - 4;
            self.toasts.drain(0..extra);
        }
    }

    fn toast_area(&mut self, ctx: &egui::Context) {
        if self.toasts.is_empty() {
            return;
        }
        let mut clear = false;
        egui::Area::new("toasts".into())
            .anchor(egui::Align2::RIGHT_BOTTOM, [-16.0, -16.0])
            .show(ctx, |ui| {
                for toast in self.toasts.iter().rev().take(4) {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.label(RichText::new(toast).small());
                    });
                }
                if ui.small_button("dismiss").clicked() {
                    clear = true;
                }
            });
        if clear {
            self.toasts.clear();
        }
    }

    fn top_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("rdm");
                ui.separator();
                if ui.button("+ New download").clicked() {
                    self.show_add = true;
                }
                if ui.button("Import links…").clicked() {
                    self.show_import = true;
                }
                if ui.button("Resume all").clicked() {
                    self.engine.resume_all();
                }
                if ui.button("Pause all").clicked() {
                    self.engine.pause_all();
                }
                if ui.button("Clear finished").clicked() {
                    self.engine.clear_completed();
                }
                if ui
                    .button("Clear history")
                    .on_hover_text("Remove every download from the list")
                    .clicked()
                {
                    self.confirm_clear_all = true;
                }
                ui.separator();
                if ui.button("Settings").clicked() {
                    self.show_settings = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let items = self.engine.items.lock().unwrap();
                    let speed: f64 = items.iter().map(|i| i.speed).sum();
                    let active = items.iter().filter(|i| i.status == Status::Downloading).count();
                    drop(items);
                    ui.label(format!("{active} active  •  {}", human_speed(speed)));
                    ui.separator();
                    ui.add(
                        egui::TextEdit::singleline(&mut self.search)
                            .hint_text("Search")
                            .desired_width(160.0),
                    );
                    egui::ComboBox::from_id_source("sort")
                        .selected_text(match self.sort {
                            Sort::Added => "Newest",
                            Sort::Name => "Name",
                            Sort::Size => "Size",
                            Sort::Status => "Status",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.sort, Sort::Added, "Newest");
                            ui.selectable_value(&mut self.sort, Sort::Name, "Name");
                            ui.selectable_value(&mut self.sort, Sort::Size, "Size");
                            ui.selectable_value(&mut self.sort, Sort::Status, "Status");
                        });
                });
            });
            if !self.status.is_empty() {
                ui.label(RichText::new(&self.status).color(Color32::LIGHT_BLUE));
            }
        });
    }

    fn side_bar(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("side").default_width(180.0).show(ctx, |ui| {
            ui.add_space(6.0);
            ui.label(RichText::new("STATUS").small().weak());
            ui.selectable_value(&mut self.filter, Filter::All, "All downloads");
            ui.selectable_value(&mut self.filter, Filter::Unfinished, "Unfinished");
            ui.selectable_value(&mut self.filter, Filter::Finished, "Finished");
            ui.selectable_value(&mut self.filter, Filter::Failed, "Failed");
            ui.add_space(10.0);
            ui.label(RichText::new("CATEGORIES").small().weak());
            let (categories, queues) = {
                let s = self.engine.settings.lock().unwrap();
                (
                    s.categories.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
                    s.queues.clone(),
                )
            };
            for name in categories {
                ui.selectable_value(&mut self.filter, Filter::Category(name.clone()), name);
            }
            ui.add_space(10.0);
            ui.label(RichText::new("QUEUES").small().weak());
            for q in queues {
                let state = if q.open_now() { "open" } else { "waiting" };
                ui.selectable_value(
                    &mut self.filter,
                    Filter::Queue(q.name.clone()),
                    format!("{}  ({state})", q.name),
                );
            }

            ui.add_space(12.0);
            ui.separator();
            let items = self.engine.items.lock().unwrap();
            let done = items.iter().filter(|i| i.status == Status::Completed).count();
            let bytes: u64 = items.iter().map(|i| i.downloaded).sum();
            drop(items);
            ui.label(RichText::new(format!("{done} finished")).small().weak());
            ui.label(RichText::new(format!("{} downloaded", human_bytes(bytes))).small().weak());
        });
    }

    fn list(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            let mut items = self.engine.items.lock().unwrap().clone();
            let needle = self.search.to_ascii_lowercase();
            items.retain(|i| match &self.filter {
                Filter::All => true,
                Filter::Unfinished => i.status != Status::Completed,
                Filter::Finished => i.status == Status::Completed,
                Filter::Failed => matches!(i.status, Status::Failed(_)),
                Filter::Category(c) => &i.category == c,
                Filter::Queue(q) => &i.queue == q,
            });
            if !needle.is_empty() {
                items.retain(|i| {
                    i.name.to_ascii_lowercase().contains(&needle)
                        || i.url.to_ascii_lowercase().contains(&needle)
                });
            }
            match self.sort {
                Sort::Added => items.reverse(),
                Sort::Name => items.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
                Sort::Size => items.sort_by(|a, b| b.total.cmp(&a.total)),
                Sort::Status => items.sort_by_key(|i| i.status.label()),
            }

            if items.is_empty() {
                ui.centered_and_justified(|ui| {
                    ui.label("No downloads here. Use “+ New download” or “Import links…”.");
                });
                return;
            }

            egui::ScrollArea::vertical().show(ui, |ui| {
                for item in items {
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&item.name).strong());
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                match item.status {
                                    Status::Downloading => {
                                        if ui.button("Pause").clicked() {
                                            self.engine.pause(item.id);
                                        }
                                    }
                                    Status::Paused | Status::Queued => {
                                        if ui.button("Resume").clicked() {
                                            self.engine.resume(item.id);
                                        }
                                    }
                                    Status::Failed(_) => {
                                        if ui.button("Retry").clicked() {
                                            self.engine.retry(item.id);
                                        }
                                    }
                                    Status::Completed => {
                                        if ui.button("Open folder").clicked() {
                                            open_path(&item.folder);
                                        }
                                        if ui.button("Open file").clicked() {
                                            open_path(&item.path());
                                        }
                                    }
                                }
                                if ui.button("Remove").clicked() {
                                    self.engine.remove(item.id, false);
                                }
                                if ui.button("Delete file").clicked() {
                                    self.engine.remove(item.id, true);
                                }
                                if ui.button("Restart").clicked() {
                                    self.engine.restart(item.id);
                                }
                                if ui.button("Options").clicked() {
                                    let net = item.net.clone().unwrap_or_default();
                                    self.details = Some(Details {
                                        id: item.id,
                                        referer: net.referer.clone().unwrap_or_default(),
                                        cookie: net.cookie.clone().unwrap_or_default(),
                                        user_agent: net.user_agent.clone().unwrap_or_default(),
                                        headers: net
                                            .headers
                                            .iter()
                                            .map(|(k, v)| format!("{k}: {v}"))
                                            .collect::<Vec<_>>()
                                            .join("\n"),
                                        speed_limit_kb: item.speed_limit / 1024,
                                    });
                                }
                                if ui.button("Copy link").clicked() {
                                    ui.output_mut(|o| o.copied_text = item.url.clone());
                                }
                            });
                        });

                        let text = if item.total > 0 {
                            format!(
                                "{} / {}  ({:.0}%)",
                                human_bytes(item.downloaded),
                                human_bytes(item.total),
                                item.percent() * 100.0
                            )
                        } else {
                            human_bytes(item.downloaded)
                        };
                        ui.add(egui::ProgressBar::new(item.percent()).text(text).desired_height(14.0));

                        ui.horizontal(|ui| {
                            let color = match item.status {
                                Status::Completed => Color32::from_rgb(90, 200, 120),
                                Status::Failed(_) => Color32::from_rgb(230, 110, 110),
                                Status::Downloading => Color32::from_rgb(110, 170, 240),
                                _ => Color32::GRAY,
                            };
                            ui.label(RichText::new(item.status.label()).color(color).small());
                            ui.label(RichText::new("•").weak().small());
                            ui.label(RichText::new(human_speed(item.speed)).small());
                            ui.label(RichText::new("•").weak().small());
                            ui.label(RichText::new(format!("eta {}", human_eta(item.eta_secs()))).small());
                            ui.label(RichText::new("•").weak().small());
                            ui.label(
                                RichText::new(format!(
                                    "{} • queue {} • x{} conn",
                                    item.category, item.queue, item.connections
                                ))
                                .small(),
                            );
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.label(RichText::new(item.finished.clone().unwrap_or(item.added.clone())).weak().small());
                            });
                        });
                        ui.label(RichText::new(&item.url).weak().small());
                    });
                    ui.add_space(4.0);
                }
            });
        });
    }

    // --------------------------------------------------------------- windows

    fn add_window(&mut self, ctx: &egui::Context) {
        if !self.show_add {
            return;
        }
        let queues: Vec<String> = self
            .engine
            .settings
            .lock()
            .unwrap()
            .queues
            .iter()
            .map(|q| q.name.clone())
            .collect();
        let mut open = true;
        egui::Window::new("New download").open(&mut open).resizable(true).show(ctx, |ui| {
            ui.label("Links (one per line, .m3u8 streams are supported)");
            ui.add(egui::TextEdit::multiline(&mut self.add_urls).desired_rows(5).desired_width(460.0));
            ui.horizontal(|ui| {
                ui.label("Save as (optional, single link)");
                ui.text_edit_singleline(&mut self.add_name);
            });
            ui.horizontal(|ui| {
                ui.label("Referer");
                ui.add(egui::TextEdit::singleline(&mut self.add_referer).desired_width(340.0));
            });
            ui.horizontal(|ui| {
                ui.label("Cookie");
                ui.add(egui::TextEdit::singleline(&mut self.add_cookie).desired_width(340.0));
            });
            ui.horizontal(|ui| {
                ui.label("Queue");
                egui::ComboBox::from_id_source("add_queue")
                    .selected_text(self.add_queue.clone())
                    .show_ui(ui, |ui| {
                        for q in &queues {
                            ui.selectable_value(&mut self.add_queue, q.clone(), q);
                        }
                    });
            });
            ui.add_space(6.0);
            if ui.button("Add to queue").clicked() {
                let urls: Vec<String> = self
                    .add_urls
                    .lines()
                    .map(|l| l.trim().to_string())
                    .filter(|l| l.starts_with("http"))
                    .collect();
                let single = urls.len() == 1;
                let base = self.engine.settings.lock().unwrap().net.clone();
                let net = (!self.add_referer.trim().is_empty() || !self.add_cookie.trim().is_empty()).then(|| {
                    let mut net = base;
                    net.referer = (!self.add_referer.trim().is_empty()).then(|| self.add_referer.trim().to_string());
                    net.cookie = (!self.add_cookie.trim().is_empty()).then(|| self.add_cookie.trim().to_string());
                    net
                });
                for url in &urls {
                    let name = if single && !self.add_name.trim().is_empty() {
                        Some(self.add_name.trim().to_string())
                    } else {
                        None
                    };
                    self.engine.add_full(url, name, Some(self.add_queue.clone()), net.clone(), 0);
                }
                self.status = format!("Added {} link(s)", urls.len());
                self.add_urls.clear();
                self.add_name.clear();
                self.show_add = false;
            }
        });
        if !open {
            self.show_add = false;
        }
    }

    fn details_window(&mut self, ctx: &egui::Context) {
        let Some(details) = self.details.as_mut() else { return };
        let mut open = true;
        let mut apply = false;
        egui::Window::new("Download options").open(&mut open).default_width(480.0).show(ctx, |ui| {
            ui.label("Referer");
            ui.add(egui::TextEdit::singleline(&mut details.referer).desired_width(440.0));
            ui.label("Cookie");
            ui.add(egui::TextEdit::singleline(&mut details.cookie).desired_width(440.0));
            ui.label("User agent");
            ui.add(egui::TextEdit::singleline(&mut details.user_agent).desired_width(440.0));
            ui.label("Extra headers (one “Name: value” per line)");
            ui.add(egui::TextEdit::multiline(&mut details.headers).desired_rows(4).desired_width(440.0));
            ui.add(
                egui::Slider::new(&mut details.speed_limit_kb, 0..=102_400)
                    .text("Speed limit for this download (KB/s, 0 = global)"),
            );
            if ui.button("Apply (restart the download to use them)").clicked() {
                apply = true;
            }
        });

        if apply {
            let mut net = NetConfig::default();
            net.referer = (!details.referer.trim().is_empty()).then(|| details.referer.trim().to_string());
            net.cookie = (!details.cookie.trim().is_empty()).then(|| details.cookie.trim().to_string());
            net.user_agent = (!details.user_agent.trim().is_empty()).then(|| details.user_agent.trim().to_string());
            net.headers = parse_headers(&details.headers);
            let limit = details.speed_limit_kb * 1024;
            let id = details.id;
            {
                let mut items = self.engine.items.lock().unwrap();
                if let Some(item) = items.iter_mut().find(|i| i.id == id) {
                    item.net = Some(net);
                    item.speed_limit = limit;
                }
            }
            self.engine.save();
            self.status = "Download options saved".into();
            self.details = None;
            return;
        }
        if !open {
            self.details = None;
        }
    }

    fn clipboard_window(&mut self, ctx: &egui::Context) {
        let links: Vec<String> = self.pending_clipboard.lock().unwrap().clone();
        if links.is_empty() {
            return;
        }
        let mut accepted: Vec<String> = Vec::new();
        let mut dismiss = false;
        egui::Window::new("Links copied to the clipboard")
            .collapsible(false)
            .default_width(520.0)
            .show(ctx, |ui| {
                for link in &links {
                    ui.horizontal(|ui| {
                        if ui.button("Download").clicked() {
                            accepted.push(link.clone());
                        }
                        ui.label(RichText::new(link).small());
                    });
                }
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Download all").clicked() {
                        accepted.extend(links.iter().cloned());
                    }
                    if ui.button("Ignore").clicked() {
                        dismiss = true;
                    }
                });
            });

        if !accepted.is_empty() || dismiss {
            for link in &accepted {
                self.engine.add(link, None, None);
            }
            let mut pending = self.pending_clipboard.lock().unwrap();
            if dismiss {
                pending.clear();
            } else {
                pending.retain(|l| !accepted.contains(l));
            }
        }
    }

    fn clear_all_window(&mut self, ctx: &egui::Context) {
        if !self.confirm_clear_all {
            return;
        }
        let mut open = true;
        egui::Window::new("Clear download history")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                let count = self.engine.items.lock().unwrap().len();
                ui.label(format!("Remove all {count} download(s) from the list?"));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Clear list only").clicked() {
                        self.engine.clear_all(false);
                        self.confirm_clear_all = false;
                    }
                    if ui.button("Clear list and delete files").clicked() {
                        self.engine.clear_all(true);
                        self.confirm_clear_all = false;
                    }
                    if ui.button("Cancel").clicked() {
                        self.confirm_clear_all = false;
                    }
                });
            });
        if !open {
            self.confirm_clear_all = false;
        }
    }

    fn import_window(&mut self, ctx: &egui::Context) {
        if !self.show_import {
            return;
        }
        let mut open = true;
        egui::Window::new("Import links from a text file")
            .open(&mut open)
            .default_width(560.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("File");
                    ui.add(egui::TextEdit::singleline(&mut self.import_path).desired_width(380.0));
                    if ui.button("Scan").clicked() {
                        let dir = self.engine.settings.lock().unwrap().download_dir.clone();
                        match parse_link_file(&PathBuf::from(self.import_path.trim()), &dir) {
                            Ok(jobs) => {
                                self.import_rows = jobs
                                    .into_iter()
                                    .map(|j| ImportRow {
                                        name: j
                                            .output
                                            .file_name()
                                            .map(|n| n.to_string_lossy().to_string())
                                            .unwrap_or_default(),
                                        url: j.url,
                                        selected: true,
                                    })
                                    .collect();
                                self.status = format!("Found {} link(s)", self.import_rows.len());
                            }
                            Err(err) => self.status = format!("Import failed: {err}"),
                        }
                    }
                });

                if !self.import_rows.is_empty() {
                    ui.horizontal(|ui| {
                        if ui.button("Select all").clicked() {
                            self.import_rows.iter_mut().for_each(|r| r.selected = true);
                        }
                        if ui.button("Deselect all").clicked() {
                            self.import_rows.iter_mut().for_each(|r| r.selected = false);
                        }
                        let picked = self.import_rows.iter().filter(|r| r.selected).count();
                        ui.label(format!("{picked}/{} selected", self.import_rows.len()));
                    });
                    ui.separator();
                    egui::ScrollArea::vertical().max_height(280.0).show(ui, |ui| {
                        for row in &mut self.import_rows {
                            ui.horizontal(|ui| {
                                ui.checkbox(&mut row.selected, "");
                                ui.vertical(|ui| {
                                    ui.label(RichText::new(&row.name).strong());
                                    ui.label(RichText::new(&row.url).weak().small());
                                });
                            });
                        }
                    });
                    ui.separator();
                    if ui.button("Add selected to queue").clicked() {
                        let mut added = 0;
                        for row in self.import_rows.iter().filter(|r| r.selected) {
                            self.engine.add(&row.url, Some(row.name.clone()), None);
                            added += 1;
                        }
                        self.status = format!("Added {added} link(s) from file");
                        self.import_rows.clear();
                        self.show_import = false;
                    }
                }
            });
        if !open {
            self.show_import = false;
        }
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }
        let mut open = true;
        let mut changed = false;
        let mut settings = self.engine.settings.lock().unwrap().clone();

        egui::Window::new("Settings").open(&mut open).default_width(560.0).show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.label(RichText::new("Downloads").strong());
                let mut dir = settings.download_dir.to_string_lossy().to_string();
                ui.horizontal(|ui| {
                    ui.label("Save folder");
                    if ui.add(egui::TextEdit::singleline(&mut dir).desired_width(360.0)).changed() {
                        settings.download_dir = PathBuf::from(dir.clone());
                        changed = true;
                    }
                });
                changed |= ui
                    .checkbox(&mut settings.sort_into_categories, "Sort files into category folders")
                    .changed();
                changed |= ui
                    .add(egui::Slider::new(&mut settings.max_parallel, 1..=10).text("Simultaneous downloads"))
                    .changed();
                changed |= ui
                    .add(egui::Slider::new(&mut settings.connections, 1..=32).text("Connections per download"))
                    .changed();
                let mut limit_kb = settings.speed_limit / 1024;
                if ui
                    .add(egui::Slider::new(&mut limit_kb, 0..=102_400).text("Speed limit (KB/s, 0 = unlimited)"))
                    .changed()
                {
                    settings.speed_limit = limit_kb * 1024;
                    changed = true;
                }
                changed |= ui
                    .add(egui::Slider::new(&mut settings.auto_retry, 0..=20).text("Automatic retries"))
                    .changed();
                changed |= ui
                    .add(egui::Slider::new(&mut settings.retry_delay_secs, 1..=300).text("Retry delay (seconds)"))
                    .changed();

                ui.add_space(10.0);
                ui.label(RichText::new("Appearance").strong());
                ui.horizontal(|ui| {
                    changed |= ui.selectable_value(&mut settings.theme, Theme::Dark, "Dark").changed();
                    changed |= ui.selectable_value(&mut settings.theme, Theme::Light, "Light").changed();
                    changed |= ui.selectable_value(&mut settings.theme, Theme::Black, "Black").changed();
                    let mut rgb = [
                        settings.accent[0] as f32 / 255.0,
                        settings.accent[1] as f32 / 255.0,
                        settings.accent[2] as f32 / 255.0,
                    ];
                    if ui.color_edit_button_rgb(&mut rgb).changed() {
                        settings.accent = [
                            (rgb[0] * 255.0) as u8,
                            (rgb[1] * 255.0) as u8,
                            (rgb[2] * 255.0) as u8,
                        ];
                        changed = true;
                    }
                });

                ui.add_space(10.0);
                ui.label(RichText::new("Notifications & automation").strong());
                changed |= ui
                    .checkbox(&mut settings.notifications, "Desktop notification when a download ends")
                    .changed();
                changed |= ui
                    .checkbox(&mut settings.clipboard_monitor, "Watch the clipboard for download links")
                    .changed();
                let mut exts = settings.clipboard_extensions.join(", ");
                if ui
                    .add(egui::TextEdit::singleline(&mut exts).desired_width(440.0).hint_text("zip, iso, mp4…"))
                    .changed()
                {
                    settings.clipboard_extensions = exts
                        .split(',')
                        .map(|e| e.trim().trim_start_matches('.').to_ascii_lowercase())
                        .filter(|e| !e.is_empty())
                        .collect();
                    changed = true;
                }
                let mut action = settings.on_complete.unwrap_or(OnComplete::Nothing);
                egui::ComboBox::from_label("When every download finishes")
                    .selected_text(match action {
                        OnComplete::Nothing => "Do nothing",
                        OnComplete::ExitApp => "Close rdm",
                        OnComplete::ShutdownSystem => "Shut down the computer",
                    })
                    .show_ui(ui, |ui| {
                        changed |= ui.selectable_value(&mut action, OnComplete::Nothing, "Do nothing").changed();
                        changed |= ui.selectable_value(&mut action, OnComplete::ExitApp, "Close rdm").changed();
                        changed |= ui
                            .selectable_value(&mut action, OnComplete::ShutdownSystem, "Shut down the computer")
                            .changed();
                    });
                settings.on_complete = Some(action);

                ui.add_space(10.0);
                ui.label(RichText::new("Network").strong());
                let mut ua = settings.net.user_agent.clone().unwrap_or_default();
                ui.horizontal(|ui| {
                    ui.label("User agent");
                    if ui.add(egui::TextEdit::singleline(&mut ua).desired_width(380.0)).changed() {
                        settings.net.user_agent = (!ua.trim().is_empty()).then(|| ua.trim().to_string());
                        changed = true;
                    }
                });
                let mut proxy = settings.net.proxy.clone().unwrap_or_default();
                ui.horizontal(|ui| {
                    ui.label("Proxy");
                    if ui
                        .add(
                            egui::TextEdit::singleline(&mut proxy)
                                .desired_width(380.0)
                                .hint_text("http://host:port or socks5://host:port"),
                        )
                        .changed()
                    {
                        settings.net.proxy = (!proxy.trim().is_empty()).then(|| proxy.trim().to_string());
                        changed = true;
                    }
                });
                let mut doh = settings.net.doh.clone().unwrap_or_default();
                ui.horizontal(|ui| {
                    ui.label("DNS over HTTPS");
                    if ui
                        .add(
                            egui::TextEdit::singleline(&mut doh)
                                .desired_width(360.0)
                                .hint_text("https://cloudflare-dns.com/dns-query"),
                        )
                        .changed()
                    {
                        settings.net.doh = (!doh.trim().is_empty()).then(|| doh.trim().to_string());
                        changed = true;
                    }
                });
                let mut headers = settings
                    .net
                    .headers
                    .iter()
                    .map(|(k, v)| format!("{k}: {v}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                ui.label("Default extra headers (one “Name: value” per line)");
                if ui
                    .add(egui::TextEdit::multiline(&mut headers).desired_rows(3).desired_width(440.0))
                    .changed()
                {
                    settings.net.headers = parse_headers(&headers);
                    changed = true;
                }

                ui.add_space(10.0);
                ui.label(RichText::new("Queues & scheduling").strong());
                let mut remove_queue: Option<usize> = None;
                for (idx, queue) in settings.queues.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        changed |= ui
                            .add(egui::TextEdit::singleline(&mut queue.name).desired_width(110.0))
                            .changed();
                        changed |= ui.checkbox(&mut queue.enabled, "on").changed();
                        let mut start = queue.start_at.clone().unwrap_or_default();
                        ui.label("start");
                        if ui.add(egui::TextEdit::singleline(&mut start).desired_width(56.0)).changed() {
                            queue.start_at = (!start.trim().is_empty()).then(|| start.trim().to_string());
                            changed = true;
                        }
                        let mut stop = queue.stop_at.clone().unwrap_or_default();
                        ui.label("stop");
                        if ui.add(egui::TextEdit::singleline(&mut stop).desired_width(56.0)).changed() {
                            queue.stop_at = (!stop.trim().is_empty()).then(|| stop.trim().to_string());
                            changed = true;
                        }
                        if ui.button("x").clicked() {
                            remove_queue = Some(idx);
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.add_space(12.0);
                        for (day, label) in ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"].iter().enumerate() {
                            let day = day as u8;
                            let mut on = queue.days.is_empty() || queue.days.contains(&day);
                            if ui.toggle_value(&mut on, *label).changed() {
                                if queue.days.is_empty() {
                                    queue.days = (0..7u8).collect();
                                }
                                if on {
                                    if !queue.days.contains(&day) {
                                        queue.days.push(day);
                                    }
                                } else {
                                    queue.days.retain(|d| *d != day);
                                }
                                queue.days.sort_unstable();
                                changed = true;
                            }
                        }
                    });
                }
                if let Some(idx) = remove_queue {
                    settings.queues.remove(idx);
                    changed = true;
                }
                if ui.button("+ Add queue").clicked() {
                    settings.queues.push(Queue {
                        name: format!("Queue {}", settings.queues.len() + 1),
                        start_at: None,
                        stop_at: None,
                        enabled: true,
                        days: Vec::new(),
                    });
                    changed = true;
                }
                ui.label(RichText::new("Times are local, 24h HH:MM. Empty = always open.").weak().small());

                ui.add_space(10.0);
                ui.label(RichText::new("Categories").strong());
                let mut remove_cat: Option<usize> = None;
                for (idx, cat) in settings.categories.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        changed |= ui
                            .add(egui::TextEdit::singleline(&mut cat.name).desired_width(90.0))
                            .changed();
                        changed |= ui
                            .add(egui::TextEdit::singleline(&mut cat.folder).desired_width(110.0))
                            .changed();
                        let mut ext = cat.extensions.join(", ");
                        if ui.add(egui::TextEdit::singleline(&mut ext).desired_width(230.0)).changed() {
                            cat.extensions = ext
                                .split(',')
                                .map(|e| e.trim().trim_start_matches('.').to_ascii_lowercase())
                                .filter(|e| !e.is_empty())
                                .collect();
                            changed = true;
                        }
                        if ui.button("x").clicked() {
                            remove_cat = Some(idx);
                        }
                    });
                }
                if let Some(idx) = remove_cat {
                    settings.categories.remove(idx);
                    changed = true;
                }
                if ui.button("+ Add category").clicked() {
                    settings.categories.push(Category {
                        name: "New".into(),
                        folder: "New".into(),
                        extensions: vec![],
                    });
                    changed = true;
                }

                ui.add_space(10.0);
                ui.label(RichText::new("Browser integration").strong());
                changed |= ui
                    .checkbox(&mut settings.browser_integration, "Accept links from the browser extension")
                    .changed();
                let mut port = settings.browser_port as u32;
                if ui.add(egui::DragValue::new(&mut port).range(1024..=65535).prefix("port ")).changed() {
                    settings.browser_port = port as u16;
                    changed = true;
                }
                ui.label(
                    RichText::new("POST http://127.0.0.1:<port>/add  {\"url\": \"…\"} — restart to apply port changes.")
                        .weak()
                        .small(),
                );
            });
        });

        if changed {
            *self.engine.settings.lock().unwrap() = settings;
            self.engine.save();
        }
        if !open {
            self.show_settings = false;
        }
    }
}

fn parse_headers(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let (k, v) = line.split_once(':')?;
            let (k, v) = (k.trim(), v.trim());
            (!k.is_empty() && !v.is_empty()).then(|| (k.to_string(), v.to_string()))
        })
        .collect()
}

fn open_path(path: &std::path::Path) {
    #[cfg(target_os = "linux")]
    let cmd = "xdg-open";
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(target_os = "windows")]
    let cmd = "explorer";
    let _ = std::process::Command::new(cmd).arg(path).spawn();
}
