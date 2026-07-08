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
}

fn detect_system_theme() -> egui::Theme {
    let home = std::env::var("HOME").unwrap_or_default();

    if let Ok(output) = Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "color-scheme"])
        .output()
    {
        let s = String::from_utf8_lossy(&output.stdout);
        if s.contains("prefer-dark") {
            return egui::Theme::Dark;
        }
        if let Ok(output) = Command::new("gsettings")
            .args(["get", "org.gnome.desktop.interface", "gtk-theme"])
            .output()
        {
            let t = String::from_utf8_lossy(&output.stdout).to_lowercase();
            if t.contains("dark") || t.contains("night") {
                return egui::Theme::Dark;
            }
        }
    }

    if let Ok(content) = std::fs::read_to_string(format!("{}/.config/kdeglobals", home)) {
        if content.to_lowercase().contains("colorscheme=") {
            for line in content.lines() {
                if let Some(name) = line.strip_prefix("ColorScheme=") {
                    if name.to_lowercase().contains("dark") {
                        return egui::Theme::Dark;
                    }
                }
            }
        }
    }

    for path in &[".config/gtk-3.0/settings.ini", ".config/gtk-4.0/settings.ini"] {
        if let Ok(content) = std::fs::read_to_string(format!("{}/{}", home, path)) {
            if content.contains("gtk-application-prefer-dark-theme=1") {
                return egui::Theme::Dark;
            }
        }
    }

    egui::Theme::Light
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

impl eframe::App for FlintApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
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

        ui.vertical_centered(|ui| {
            ui.heading(egui::RichText::new("flint").size(20.0));
        });
        ui.separator();
        ui.add_space(4.0);

        egui::Frame::default()
            .fill(ui.style().visuals.window_fill())
            .corner_radius(egui::CornerRadius::same(4))
            .inner_margin(egui::Margin::symmetric(8, 8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("ISO File:");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.iso_path).desired_width(240.0),
                    );
                    if ui.button("Browse").clicked() {
                        if let Some(path) = FileDialog::new()
                            .add_filter("ISO", &["iso"])
                            .add_filter("All files", &["*"])
                            .pick_file()
                        {
                            self.iso_path = path.display().to_string();
                        }
                    }
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label("USB Device:");
                    if self.usb_devices.is_empty() {
                        ui.label("No USB devices found");
                    } else {
                        egui::ComboBox::from_id_salt("usb_device")
                            .selected_text(self.usb_devices[self.selected_idx].label.as_str())
                            .show_ui(ui, |ui| {
                                for (i, dev) in self.usb_devices.iter().enumerate() {
                                    ui.selectable_value(
                                        &mut self.selected_idx,
                                        i,
                                        &dev.label,
                                    );
                                }
                            });
                    }
                    if ui.button("Refresh").clicked() {
                        self.refresh_devices();
                    }
                });
            });

        ui.add_space(8.0);

        ui.vertical_centered(|ui| {
            if self.flashing {
                if ui.button("Cancel").clicked() {
                    self.cancel_flash();
                }
                ui.add_space(4.0);
                ui.add(
                    egui::ProgressBar::new(self.progress)
                        .show_percentage()
                        .animate(true),
                );
                ui.label(&self.status);
            } else {
                let can_flash = !self.iso_path.is_empty() && !self.usb_devices.is_empty();
                let btn = egui::Button::new(egui::RichText::new("Start flashing").size(16.0))
                    .min_size(egui::Vec2::new(220.0, 42.0))
                    .fill(ui.style().visuals.selection.bg_fill);
                if ui.add_enabled(can_flash, btn).clicked() {
                    self.start_flash();
                }
            }
        });

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(4.0);

        ui.label("Log:");
        egui::Frame::default()
            .fill(ui.style().visuals.window_fill())
            .corner_radius(egui::CornerRadius::same(4))
            .inner_margin(egui::Margin::symmetric(6, 4))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(120.0)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        for line in &self.log {
                            ui.label(egui::RichText::new(line).monospace().size(11.0));
                        }
                    });
            });

        if self.flashing {
            ui.ctx().request_repaint();
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let is_dark = detect_system_theme() == egui::Theme::Dark;
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([520.0, 440.0]),
        ..Default::default()
    };
    eframe::run_native(
        "flint",
        options,
        Box::new(|cc| {
            cc.egui_ctx.set_visuals(if is_dark {
                egui::Visuals::dark()
            } else {
                egui::Visuals::light()
            });
            Ok(Box::new(FlintApp::new()))
        }),
    )?;
    Ok(())
}
