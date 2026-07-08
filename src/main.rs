use eframe::egui;
use rfd::FileDialog;
use serde::Deserialize;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;

#[derive(Deserialize)]
struct LsblkDevices {
    blockdevices: Vec<LsblkDevice>,
}

#[derive(Deserialize)]
struct LsblkDevice {
    name: String,
    size: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    tran: Option<String>,
}

#[derive(Clone)]
struct UsbDevice {
    path: String,
    label: String,
}

enum Message {
    Progress(u64, u64),
    Done(bool),
    Log(String),
}

struct FlintApp {
    iso_path: String,
    usb_devices: Vec<UsbDevice>,
    selected_idx: usize,
    flashing: bool,
    progress: f32,
    status: String,
    log: Vec<String>,
    rx: Option<mpsc::Receiver<Message>>,
    cancel: Arc<AtomicBool>,
    dark_mode: bool,
    show_log: bool,
}

fn detect_is_dark() -> bool {
    let home = std::env::var("HOME").unwrap_or_default();

    if let Ok(output) = Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "color-scheme"])
        .output()
    {
        let s = String::from_utf8_lossy(&output.stdout);
        if s.contains("prefer-dark") {
            return true;
        }
        if let Ok(output) = Command::new("gsettings")
            .args(["get", "org.gnome.desktop.interface", "gtk-theme"])
            .output()
        {
            let t = String::from_utf8_lossy(&output.stdout).to_lowercase();
            if t.contains("dark") || t.contains("night") {
                return true;
            }
        }
    }

    if let Ok(content) = std::fs::read_to_string(format!("{}/.config/kdeglobals", home)) {
        if content.to_lowercase().contains("colorscheme=") {
            for line in content.lines() {
                if let Some(name) = line.strip_prefix("ColorScheme=") {
                    if name.to_lowercase().contains("dark") {
                        return true;
                    }
                }
            }
        }
    }

    for path in &[".config/gtk-3.0/settings.ini", ".config/gtk-4.0/settings.ini"] {
        if let Ok(content) = std::fs::read_to_string(format!("{}/{}", home, path)) {
            if content.contains("gtk-application-prefer-dark-theme=1") {
                return true;
            }
        }
    }

    false
}

impl FlintApp {
    fn new() -> Self {
        let devices = Self::detect_usb_devices();
        Self {
            iso_path: String::new(),
            usb_devices: devices,
            selected_idx: 0,
            flashing: false,
            progress: 0.0,
            status: "Ready".to_string(),
            log: vec!["flint ready".to_string()],
            rx: None,
            cancel: Arc::new(AtomicBool::new(false)),
            dark_mode: detect_is_dark(),
            show_log: false,
        }
    }

    fn detect_usb_devices() -> Vec<UsbDevice> {
        let mut devices = Vec::new();
        if let Ok(output) = Command::new("lsblk")
            .args(["-d", "-o", "NAME,SIZE,MODEL,TRAN", "-J"])
            .output()
        {
            if let Ok(parsed) = serde_json::from_slice::<LsblkDevices>(&output.stdout) {
                for dev in parsed.blockdevices {
                    if dev.tran.as_deref() == Some("usb") {
                        let model = dev.model.as_deref().unwrap_or("Unknown").trim();
                        let path = format!("/dev/{}", dev.name);
                        let label = format!("{} - {} - {}", dev.name, dev.size, model);
                        devices.push(UsbDevice { path, label });
                    }
                }
            }
        }
        devices
    }

    fn refresh_devices(&mut self) {
        self.usb_devices = Self::detect_usb_devices();
        self.selected_idx = self.selected_idx.min(self.usb_devices.len().saturating_sub(1));
    }

    fn start_flash(&mut self) {
        if self.iso_path.is_empty() || self.usb_devices.is_empty() {
            return;
        }
        let iso_path = self.iso_path.clone();
        let dev_path = self.usb_devices[self.selected_idx].path.clone();
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        self.flashing = true;
        self.progress = 0.0;
        self.log.push("Starting flash...".to_string());
        self.status = "Flashing...".to_string();
        self.cancel.store(false, Ordering::SeqCst);
        let cancel = self.cancel.clone();

        thread::spawn(move || {
            let total = Command::new("stat")
                .args(["-c", "%s", &iso_path])
                .output()
                .ok()
                .and_then(|o| {
                    String::from_utf8(o.stdout)
                        .ok()
                        .and_then(|s| s.trim().parse::<u64>().ok())
                })
                .unwrap_or(0);

            if total == 0 {
                let _ = tx.send(Message::Log("Failed to get ISO file size".to_string()));
                let _ = tx.send(Message::Done(false));
                return;
            }

            let mut child = match Command::new("dd")
                .arg(format!("if={}", iso_path))
                .arg(format!("of={}", dev_path))
                .args(["bs=4M", "status=progress", "conv=fsync"])
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
            {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(Message::Log(format!("Failed to start dd: {}", e)));
                    let _ = tx.send(Message::Done(false));
                    return;
                }
            };

            let stderr = child.stderr.take().expect("stderr not captured");
            let reader = BufReader::new(stderr);

            for line in reader.lines() {
                if cancel.load(Ordering::SeqCst) {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = tx.send(Message::Log("Cancelled by user".to_string()));
                    let _ = tx.send(Message::Done(false));
                    return;
                }
                if let Ok(line) = line {
                    if let Some(bytes) = parse_dd_progress(&line) {
                        let _ = tx.send(Message::Progress(bytes, total));
                    }
                    let _ = tx.send(Message::Log(line));
                }
            }

            let success = child.wait().map(|s| s.success()).unwrap_or(false);
            let _ = tx.send(Message::Done(success));
        });
    }

    fn cancel_flash(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        self.status = "Cancelling...".to_string();
    }
}

fn parse_dd_progress(line: &str) -> Option<u64> {
    let line = line.trim();
    if let Some(end) = line.find(" bytes (") {
        line[..end].parse::<u64>().ok()
    } else if let Some(end) = line.find(" bytes copied") {
        line[..end].parse::<u64>().ok()
    } else {
        None
    }
}

fn render_action_ui(ui: &mut egui::Ui, visuals: &egui::Visuals, app: &mut FlintApp) {
    egui::Frame::default()
        .fill(visuals.window_fill)
        .corner_radius(egui::CornerRadius::same(6))
        .inner_margin(egui::Margin::symmetric(12, 12))
        .show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.strong("ISO File");
            });
            ui.add_space(4.0);
            ui.vertical_centered(|ui| {
                ui.horizontal(|ui| {
                    let full = ui.available_width();
                    let pad = ((full - 290.0 - 10.0 - 75.0) / 2.0).max(0.0);
                    ui.add_space(pad);
                    ui.add(egui::TextEdit::singleline(&mut app.iso_path).desired_width(290.0));
                    ui.add_space(10.0);
                    if ui.button("Browse").clicked() {
                        if let Some(path) = FileDialog::new()
                            .add_filter("ISO", &["iso"])
                            .add_filter("All files", &["*"])
                            .pick_file()
                        {
                            app.iso_path = path.display().to_string();
                        }
                    }
                });
            });

            ui.add_space(10.0);

            ui.vertical_centered(|ui| {
                ui.strong("USB Device");
            });
            ui.add_space(4.0);
            ui.vertical_centered(|ui| {
                ui.horizontal(|ui| {
                    if app.usb_devices.is_empty() {
                        ui.label("No USB devices found");
                    } else {
                        let full = ui.available_width();
                        let pad = ((full - 290.0 - 10.0 - 75.0) / 2.0).max(0.0);
                        ui.add_space(pad);
                        egui::ComboBox::from_id_salt("usb_device")
                            .selected_text(app.usb_devices[app.selected_idx].label.as_str())
                            .width(290.0)
                            .show_ui(ui, |ui| {
                                for (i, dev) in app.usb_devices.iter().enumerate() {
                                    ui.selectable_value(
                                        &mut app.selected_idx,
                                        i,
                                        &dev.label,
                                    );
                                }
                            });
                        ui.add_space(10.0);
                        if ui.button("Refresh").clicked() {
                            app.refresh_devices();
                        }
                    }
                });
            });

            ui.add_space(10.0);

            ui.vertical_centered(|ui| {
                if app.flashing {
                    if ui.button("Cancel").clicked() {
                        app.cancel_flash();
                    }
                    ui.add_space(4.0);
                    ui.add(
                        egui::ProgressBar::new(app.progress)
                            .show_percentage()
                            .animate(true),
                    );
                    ui.label(&app.status);
                } else {
                    let can_flash = !app.iso_path.is_empty() && !app.usb_devices.is_empty();
                    let btn = egui::Button::new(egui::RichText::new("Start flashing").size(16.0))
                        .min_size(egui::Vec2::new(220.0, 42.0))
                        .fill(visuals.selection.bg_fill);
                    if ui.add_enabled(can_flash, btn).clicked() {
                        app.start_flash();
                    }
                }

                ui.add_space(4.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                    let term_btn = egui::Button::new(
                        egui::RichText::new(">_").monospace().size(14.0),
                    )
                    .min_size(egui::Vec2::new(30.0, 22.0));
                    if ui.add(term_btn).clicked() {
                        app.show_log = !app.show_log;
                    }
                });
            });
        });
}

fn render_log_ui(ui: &mut egui::Ui, visuals: &egui::Visuals, app: &mut FlintApp) {
    egui::Frame::default()
        .fill(visuals.window_fill)
        .corner_radius(egui::CornerRadius::same(6))
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.strong("Log");
            });
            ui.add_space(4.0);
            egui::ScrollArea::vertical()
                .max_height(120.0)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for line in &app.log {
                        ui.label(egui::RichText::new(line).monospace().size(11.0));
                    }
                });
        });
}

impl eframe::App for FlintApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let visuals = if self.dark_mode {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        ui.ctx().set_visuals(visuals.clone());

        ui.painter().rect_filled(ui.max_rect(), 0.0, visuals.panel_fill);

        if let Some(rx) = &self.rx {
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    Message::Progress(bytes, total) => {
                        self.progress = bytes as f32 / total as f32;
                        self.status = format!("Flashing... {:.1}%", self.progress * 100.0);
                    }
                    Message::Done(success) => {
                        self.flashing = false;
                        self.cancel.store(false, Ordering::SeqCst);
                        if success {
                            self.status = "Done!".to_string();
                            self.log.push("Flash completed successfully.".to_string());
                        } else {
                            self.status = "Failed!".to_string();
                            self.log.push("Flash failed or was cancelled.".to_string());
                        }
                    }
                    Message::Log(line) => {
                        self.log.push(line);
                    }
                }
            }
        }

        ui.add_space(8.0);

        if self.show_log {
            ui.vertical_centered(|ui| {
                render_action_ui(ui, &visuals, self);
            });
            ui.add_space(8.0);
            ui.vertical_centered(|ui| {
                render_log_ui(ui, &visuals, self);
            });
        } else {
            let avail = egui::vec2(ui.available_width(), ui.available_height() - 8.0);
            ui.allocate_ui_with_layout(
                avail,
                egui::Layout::top_down(egui::Align::Center).with_main_align(egui::Align::Center),
                |ui| {
                    ui.vertical_centered(|ui| {
                        render_action_ui(ui, &visuals, self);
                    });
                },
            );
        }

        if self.flashing {
            ui.ctx().request_repaint();
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let is_dark = detect_is_dark();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([520.0, 440.0]),
        ..Default::default()
    };
    eframe::run_native(
        "flint",
        options,
        Box::new(|cc| {
            if is_dark {
                cc.egui_ctx.set_visuals(egui::Visuals::dark());
            } else {
                cc.egui_ctx.set_visuals(egui::Visuals::light());
            }
            Ok(Box::new(FlintApp::new()))
        }),
    )?;
    Ok(())
}
