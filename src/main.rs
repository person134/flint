use eframe::egui;
use eframe::egui::{Color32, Visuals};
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

enum ThemePref {
    System,
    Light,
    Dark,
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
    theme: ThemePref,
}

fn parse_rgb(s: &str) -> Option<Color32> {
    let parts: Vec<u8> = s.split(',').filter_map(|v| v.trim().parse().ok()).collect();
    if parts.len() == 3 {
        Some(Color32::from_rgb(parts[0], parts[1], parts[2]))
    } else {
        None
    }
}

fn try_kde_visuals() -> Option<Visuals> {
    let home = std::env::var("HOME").ok()?;
    let content = std::fs::read_to_string(format!("{}/.config/kdeglobals", home)).ok()?;

    let mut bg = None;
    let mut fg = None;
    let mut accent = None;
    let mut section = String::new();

    for line in content.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            section = t.to_string();
            continue;
        }
        match section.as_str() {
            "[Colors:Window]" => {
                if let Some(v) = t.strip_prefix("BackgroundNormal=") {
                    bg = parse_rgb(v);
                }
                if let Some(v) = t.strip_prefix("ForegroundNormal=") {
                    fg = parse_rgb(v);
                }
                if let Some(v) = t.strip_prefix("DecorationFocus=") {
                    accent = parse_rgb(v);
                }
            }
            _ => {}
        }
    }

    let bg = bg?;
    let fg = fg?;
    let accent = accent.unwrap_or(Color32::from_rgb(61, 174, 233));

    let mut visuals = Visuals::dark();
    visuals.dark_mode = true;
    visuals.window_fill = bg;
    visuals.panel_fill = bg;
    visuals.faint_bg_color = Color32::from_rgb(40, 43, 46);
    visuals.extreme_bg_color = if bg == Color32::from_rgb(32, 35, 38) {
        Color32::from_rgb(25, 28, 31)
    } else {
        Color32::from_gray(10)
    };
    visuals.code_bg_color = Color32::from_rgb(42, 45, 48);
    visuals.override_text_color = Some(fg);
    visuals.hyperlink_color = accent;

    visuals.widgets.noninteractive.fg_stroke.color = fg;
    visuals.widgets.noninteractive.bg_fill = bg;
    visuals.widgets.noninteractive.bg_stroke.color = Color32::from_rgb(60, 63, 66);

    visuals.widgets.inactive.fg_stroke.color = fg;
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(42, 45, 48);
    visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(38, 41, 44);

    visuals.widgets.hovered.fg_stroke.color = fg;
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(52, 55, 58);

    visuals.widgets.active.fg_stroke.color = fg;
    visuals.widgets.active.bg_fill = Color32::from_rgb(52, 55, 58);

    visuals.selection.bg_fill = accent;
    visuals.selection.stroke.color = accent;

    Some(visuals)
}

fn detect_visuals() -> Visuals {
    try_kde_visuals().unwrap_or_else(|| {
        if detect_is_dark() {
            Visuals::dark()
        } else {
            Visuals::light()
        }
    })
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
            theme: ThemePref::System,
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

    fn apply_theme(&self, ctx: &egui::Context) {
        let visuals = match self.theme {
            ThemePref::System => detect_visuals(),
            ThemePref::Light => Visuals::light(),
            ThemePref::Dark => Visuals::dark(),
        };
        ctx.set_visuals(visuals);
    }

    fn cycle_theme(&mut self, ctx: &egui::Context) {
        self.theme = match self.theme {
            ThemePref::System => ThemePref::Light,
            ThemePref::Light => ThemePref::Dark,
            ThemePref::Dark => ThemePref::System,
        };
        self.apply_theme(ctx);
    }

    fn theme_label(&self) -> &str {
        match self.theme {
            ThemePref::System => "auto",
            ThemePref::Light => "light",
            ThemePref::Dark => "dark",
        }
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
        self.apply_theme(ui.ctx());

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

        ui.horizontal(|ui| {
            ui.vertical_centered(|ui| {
                ui.heading(egui::RichText::new("flint").size(20.0));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(self.theme_label()).clicked() {
                    self.cycle_theme(ui.ctx());
                }
            });
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
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([520.0, 440.0]),
        ..Default::default()
    };
    eframe::run_native(
        "flint",
        options,
        Box::new(|cc| {
            cc.egui_ctx.set_visuals(detect_visuals());
            Ok(Box::new(FlintApp::new()))
        }),
    )?;
    Ok(())
}
