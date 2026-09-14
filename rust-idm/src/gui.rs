//! egui desktop front-end: downloads list, categories, queues, settings and
//! link import with a select/deselect window.

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui::{self, Color32, RichText};

use crate::engine::{human_bytes, human_speed, Category, Engine, Queue, Status};
use crate::import::parse_link_file;

pub fn run() -> eframe::Result<()> {
    let engine = Engine::new().expect("engine");
    let browser = {
        let settings = engine.settings.lock().unwrap();
        settings.browser_integration.then_some(settings.browser_port)
    };
    if let Some(port) = browser {
        crate::browser::spawn(engine.clone(), port);
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1040.0, 660.0])
            .with_min_inner_size([760.0, 420.0])
            .with_title("rdm - Download Manager"),
        ..Default::default()
    };
    eframe::run_native("rdm", options, Box::new(|_cc| Box::new(App::new(engine))))
}

#[derive(PartialEq, Clone)]
enum Filter {
    All,
    Unfinished,
    Finished,
    Category(String),
}

struct ImportRow {
    url: String,
    name: String,
    selected: bool,
}

struct App {
    engine: Arc<Engine>,
    filter: Filter,
    show_add: bool,
    show_settings: bool,
    add_urls: String,
    add_name: String,
    add_queue: String,
    import_path: String,
    import_rows: Vec<ImportRow>,
    show_import: bool,
    status: String,
}

impl App {
    fn new(engine: Arc<Engine>) -> Self {
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
            filter: Filter::All,
            show_add: false,
            show_settings: false,
            add_urls: String::new(),
            add_name: String::new(),
            add_queue,
            import_path: String::new(),
            import_rows: Vec::new(),
            show_import: false,
            status: String::new(),
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.engine.tick();
        ctx.request_repaint_after(std::time::Duration::from_millis(400));

        self.top_bar(ctx);
        self.side_bar(ctx);
        self.list(ctx);
        self.add_window(ctx);
        self.import_window(ctx);
        self.settings_window(ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.engine.pause_all();
        self.engine.save();
    }
}

impl App {
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
                ui.separator();
                if ui.button("Settings").clicked() {
                    self.show_settings = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let items = self.engine.items.lock().unwrap();
                    let speed: f64 = items.iter().map(|i| i.speed).sum();
                    let active = items.iter().filter(|i| i.status == Status::Downloading).count();
                    ui.label(format!("{active} active  •  {}", human_speed(speed)));
                });
            });
            if !self.status.is_empty() {
                ui.label(RichText::new(&self.status).color(Color32::LIGHT_BLUE));
            }
        });
    }

    fn side_bar(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("side").default_width(170.0).show(ctx, |ui| {
            ui.add_space(6.0);
            ui.label(RichText::new("STATUS").small().weak());
            ui.selectable_value(&mut self.filter, Filter::All, "All downloads");
            ui.selectable_value(&mut self.filter, Filter::Unfinished, "Unfinished");
            ui.selectable_value(&mut self.filter, Filter::Finished, "Finished");
            ui.add_space(10.0);
            ui.label(RichText::new("CATEGORIES").small().weak());
            let categories: Vec<String> = self
                .engine
                .settings
                .lock()
                .unwrap()
                .categories
                .iter()
                .map(|c| c.name.clone())
                .collect();
            for name in categories {
                ui.selectable_value(&mut self.filter, Filter::Category(name.clone()), name);
            }
            ui.add_space(10.0);
            ui.label(RichText::new("QUEUES").small().weak());
            let queues = self.engine.settings.lock().unwrap().queues.clone();
            for q in queues {
                let state = if q.open_now() { "open" } else { "waiting" };
                ui.label(format!("{}  ({state})", q.name));
            }
        });
    }

    fn list(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            let items = self.engine.items.lock().unwrap().clone();
            let filtered: Vec<_> = items
                .into_iter()
                .filter(|i| match &self.filter {
                    Filter::All => true,
                    Filter::Unfinished => i.status != Status::Completed,
                    Filter::Finished => i.status == Status::Completed,
                    Filter::Category(c) => &i.category == c,
                })
                .collect();

            if filtered.is_empty() {
                ui.centered_and_justified(|ui| {
                    ui.label("No downloads yet. Use “+ New download” or “Import links…”.");
                });
                return;
            }

            egui::ScrollArea::vertical().show(ui, |ui| {
                for item in filtered {
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
                                    }
                                }
                                if ui.button("Remove").clicked() {
                                    self.engine.remove(item.id, false);
                                }
                                if ui.button("Delete file").clicked() {
                                    self.engine.remove(item.id, true);
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
                            ui.label(RichText::new(format!("{} • queue {}", item.category, item.queue)).small());
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.label(RichText::new(&item.added).weak().small());
                            });
                        });
                        ui.label(RichText::new(&item.url).weak().small());
                    });
                    ui.add_space(4.0);
                }
            });
        });
    }

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
            ui.label("Links (one per line)");
            ui.add(egui::TextEdit::multiline(&mut self.add_urls).desired_rows(5).desired_width(460.0));
            ui.horizontal(|ui| {
                ui.label("Save as (optional, single link)");
                ui.text_edit_singleline(&mut self.add_name);
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
                for url in &urls {
                    let name = if single && !self.add_name.trim().is_empty() {
                        Some(self.add_name.trim().to_string())
                    } else {
                        None
                    };
                    self.engine.add(url, name, Some(self.add_queue.clone()));
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

        egui::Window::new("Settings").open(&mut open).default_width(520.0).show(ctx, |ui| {
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

                ui.add_space(10.0);
                ui.label(RichText::new("Queues & scheduling").strong());
                let mut remove_queue: Option<usize> = None;
                for (idx, queue) in settings.queues.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        changed |= ui.text_edit_singleline(&mut queue.name).changed();
                        changed |= ui.checkbox(&mut queue.enabled, "on").changed();
                        let mut start = queue.start_at.clone().unwrap_or_default();
                        ui.label("start");
                        if ui.add(egui::TextEdit::singleline(&mut start).desired_width(60.0)).changed() {
                            queue.start_at = (!start.trim().is_empty()).then(|| start.trim().to_string());
                            changed = true;
                        }
                        let mut stop = queue.stop_at.clone().unwrap_or_default();
                        ui.label("stop");
                        if ui.add(egui::TextEdit::singleline(&mut stop).desired_width(60.0)).changed() {
                            queue.stop_at = (!stop.trim().is_empty()).then(|| stop.trim().to_string());
                            changed = true;
                        }
                        if ui.button("x").clicked() {
                            remove_queue = Some(idx);
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

fn open_path(path: &std::path::Path) {
    #[cfg(target_os = "linux")]
    let cmd = "xdg-open";
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(target_os = "windows")]
    let cmd = "explorer";
    let _ = std::process::Command::new(cmd).arg(path).spawn();
}
