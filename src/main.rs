use eframe::egui;
use rfd::FileDialog;
#[cfg(target_os = "linux")]
use std::io::Read;
#[cfg(target_os = "linux")]
use std::process::Stdio;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;

#[cfg(target_os = "linux")]
use serde::Deserialize;

#[cfg(target_os = "linux")]
#[derive(Deserialize)]
struct LsblkDevices {
    blockdevices: Vec<LsblkDevice>,
}

#[cfg(target_os = "linux")]
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
    Status(String),
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
    show_dialog: bool,
    dialog_message: String,
}

fn detect_is_dark() -> bool {
    #[cfg(target_os = "windows")]
    {
        if let Ok(output) = Command::new("reg")
            .args([
                "query",
                "HKCU\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize",
                "/v",
                "AppsUseLightTheme",
            ])
            .output()
        {
            let s = String::from_utf8_lossy(&output.stdout);
            if s.contains("0x0") {
                return true;
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
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
            show_dialog: false,
            dialog_message: String::new(),

        }
    }

    #[cfg(target_os = "linux")]
    fn detect_usb_devices() -> Vec<UsbDevice> {
        let mut devices = Vec::new();

        let output = match Command::new("lsblk")
            .args(["-Jpo", "name,size,model,tran"])
            .output()
        {
            Ok(o) => o,
            Err(_) => return devices,
        };

        let parsed: LsblkDevices = match serde_json::from_slice(&output.stdout) {
            Ok(d) => d,
            Err(_) => return devices,
        };

        for dev in &parsed.blockdevices {
            let is_usb = dev.tran.as_deref() == Some("usb");
            let is_disk = dev.name.starts_with("/dev/sd")
                || dev.name.starts_with("/dev/nvme")
                || dev.name.starts_with("/dev/mmcblk");
            if !is_usb || !is_disk {
                continue;
            }
            let label = if let Some(model) = &dev.model {
                format!("{} ({} - {})", dev.name, model.trim(), dev.size)
            } else {
                format!("{} ({})", dev.name, dev.size)
            };
            devices.push(UsbDevice {
                path: dev.name.clone(),
                label,
            });
        }

        devices
    }

    #[cfg(target_os = "windows")]
    fn detect_usb_devices() -> Vec<UsbDevice> {
        let mut devices = Vec::new();
        if let Ok(output) = Command::new("wmic")
            .args([
                "diskdrive",
                "where",
                "InterfaceType='USB'",
                "get",
                "DeviceID,Model,Size",
                "/format:csv",
            ])
            .output()
        {
            let s = String::from_utf8_lossy(&output.stdout);
            for line in s.lines().skip(1) {
                let parts: Vec<&str> = line.split(',').collect();
                if parts.len() >= 3 {
                    let dev_id = parts.get(1).unwrap_or(&"").trim();
                    let model = parts.get(2).unwrap_or(&"").trim();
                    if !dev_id.is_empty() {
                        let label = format!("{} ({})", dev_id, model);
                        devices.push(UsbDevice {
                            path: dev_id.to_string(),
                            label,
                        });
                    }
                }
            }
        }
        devices
    }

    fn refresh_devices(&mut self) {
        self.usb_devices = Self::detect_usb_devices();
        if self.selected_idx >= self.usb_devices.len() {
            self.selected_idx = self.usb_devices.len().saturating_sub(1);
        }
    }

    #[cfg(target_os = "linux")]
    fn is_root() -> bool {
        Command::new("id")
            .arg("-u")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| s.trim().parse::<u32>().ok())
            == Some(0)
    }

    #[cfg(target_os = "linux")]
    fn start_flash(&mut self) {
        if self.iso_path.is_empty() || self.usb_devices.is_empty() {
            return;
        }
        let iso_path = self.iso_path.clone();
        let dev_path = self.usb_devices[self.selected_idx].path.clone();
        let dev_label = self.usb_devices[self.selected_idx].label.clone();
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        self.flashing = true;
        self.progress = 0.0;
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

            let total_mb = total as f64 / 1_048_576.0;
            let _ = tx.send(Message::Log(format!(
                "ISO: {} ({:.1} MB)", iso_path, total_mb
            )));
            let _ = tx.send(Message::Log(format!(
                "Device: {} ({})", dev_label, dev_path
            )));

            let root = Self::is_root();
            let _ = tx.send(Message::Log(format!(
                "Running as admin: {}", root
            )));

            let mut child;

            if root {
                child = match Command::new("dd")
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
            } else {
                let cmd = format!(
                    "dd if='{}' of='{}' bs=4M status=progress conv=fsync",
                    iso_path, dev_path
                );
                child = match Command::new("pkexec")
                    .args(["script", "-q", "-f", "-c", &cmd, "/dev/stdout"])
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn()
                {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = tx.send(Message::Log(format!("Failed to start dd via pkexec: {}", e)));
                        let _ = tx.send(Message::Done(false));
                        return;
                    }
                };
            }

            let mut out_pipe: Box<dyn Read + Send> = if root {
                Box::new(child.stderr.take().expect("stderr not captured"))
            } else {
                Box::new(child.stdout.take().expect("stdout not captured"))
            };
            let mut buf = vec![0u8; 4096];
            let mut acc = Vec::new();

            loop {
                if cancel.load(Ordering::SeqCst) {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = tx.send(Message::Log("Cancelled by user".to_string()));
                    let _ = tx.send(Message::Done(false));
                    return;
                }

                let n = out_pipe.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    break;
                }
                acc.extend_from_slice(&buf[..n]);

                while let Some(pos) = acc.iter().position(|&b| b == b'\r' || b == b'\n') {
                    let line: Vec<u8> = acc.drain(..pos).collect();
                    acc.drain(..1);
                    if !line.is_empty() {
                        let s = String::from_utf8_lossy(&line);
                        let trimmed = s.trim();
                        if !trimmed.is_empty() {
                            if let Some(bytes) = parse_dd_progress(trimmed) {
                                let pct = bytes as f64 / total as f64;
                                let done_mb = bytes as f64 / 1_048_576.0;
                                let _ = tx.send(Message::Progress(bytes, total));
                                let _ = tx.send(Message::Status(format!(
                                    "Flashing... {:.1}% ({:.1} / {:.1} MB)", pct * 100.0, done_mb, total_mb
                                )));
                            }
                            let _ = tx.send(Message::Log(trimmed.to_string()));
                        }
                    }
                }
            }

            if !acc.is_empty() {
                let s = String::from_utf8_lossy(&acc);
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    if let Some(bytes) = parse_dd_progress(trimmed) {
                        let pct = bytes as f64 / total as f64;
                        let done_mb = bytes as f64 / 1_048_576.0;
                        let _ = tx.send(Message::Progress(bytes, total));
                        let _ = tx.send(Message::Status(format!(
                            "Flashing... {:.1}% ({:.1} / {:.1} MB)", pct * 100.0, done_mb, total_mb
                        )));
                    }
                    let _ = tx.send(Message::Log(trimmed.to_string()));
                }
            }

            let success = child.wait().map(|s| s.success()).unwrap_or(false);
            let _ = tx.send(Message::Done(success));
        });
    }

    #[cfg(target_os = "windows")]
    fn start_flash(&mut self) {
        if self.iso_path.is_empty() || self.usb_devices.is_empty() {
            return;
        }
        let iso_path = self.iso_path.clone();
        let dev_path = self.usb_devices[self.selected_idx].path.clone();
        let dev_label = self.usb_devices[self.selected_idx].label.clone();
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        self.flashing = true;
        self.progress = 0.0;
        self.status = "Flashing...".to_string();
        self.cancel.store(false, Ordering::SeqCst);
        let cancel = self.cancel.clone();

        thread::spawn(move || {
            use std::io::{Read, Write};

            let total = std::fs::metadata(&iso_path)
                .map(|m| m.len())
                .unwrap_or(0);

            if total == 0 {
                let _ = tx.send(Message::Log("Failed to get ISO file size".to_string()));
                let _ = tx.send(Message::Done(false));
                return;
            }

            let total_mb = total as f64 / 1_048_576.0;
            let _ = tx.send(Message::Log(format!(
                "ISO: {} ({:.1} MB)", iso_path, total_mb
            )));
            let _ = tx.send(Message::Log(format!(
                "Device: {} ({})", dev_label, dev_path
            )));

            let mut inp = match std::fs::File::open(&iso_path) {
                Ok(f) => f,
                Err(e) => {
                    let _ = tx.send(Message::Log(format!("Failed to open ISO: {}", e)));
                    let _ = tx.send(Message::Done(false));
                    return;
                }
            };

            let mut out = match std::fs::OpenOptions::new().write(true).open(&dev_path) {
                Ok(f) => f,
                Err(e) => {
                    let _ = tx.send(Message::Log(format!(
                        "Failed to open device (run as admin): {}", e
                    )));
                    let _ = tx.send(Message::Done(false));
                    return;
                }
            };

            let mut buf = vec![0u8; 4 * 1024 * 1024];
            let mut written: u64 = 0;

            loop {
                if cancel.load(Ordering::SeqCst) {
                    let _ = tx.send(Message::Log("Cancelled by user".to_string()));
                    let _ = tx.send(Message::Done(false));
                    return;
                }

                let n = match inp.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(e) => {
                        let _ = tx.send(Message::Log(format!("Read error: {}", e)));
                        let _ = tx.send(Message::Done(false));
                        return;
                    }
                };

                if let Err(e) = out.write_all(&buf[..n]) {
                    let _ = tx.send(Message::Log(format!("Write error: {}", e)));
                    let _ = tx.send(Message::Done(false));
                    return;
                }

                written += n as u64;
                let pct = written as f64 / total as f64;
                let done_mb = written as f64 / 1_048_576.0;
                let _ = tx.send(Message::Progress(written, total));
                let _ = tx.send(Message::Status(format!(
                    "Flashing... {:.1}% ({:.1} / {:.1} MB)", pct * 100.0, done_mb, total_mb
                )));
            }

            let _ = tx.send(Message::Done(true));
        });
    }

    fn cancel_flash(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        self.status = "Cancelling...".to_string();
    }

    #[cfg(target_os = "linux")]
    fn setup_desktop_integration() {
        let home = match std::env::var("HOME") {
            Ok(h) => h,
            Err(_) => return,
        };

        let png_128 = include_bytes!("../icon-128.png");

        let path = format!("{}/.local/share/icons/hicolor/128x128/apps", home);
        let _ = std::fs::create_dir_all(&path);
        let _ = std::fs::write(format!("{}/flint.png", path), png_128);

        let _ = std::process::Command::new("gtk-update-icon-cache")
            .args(["-f", "-t", &format!("{}/.local/share/icons/hicolor", home)])
            .output();

        let apps_dir = format!("{}/.local/share/applications", home);
        let _ = std::fs::create_dir_all(&apps_dir);
        let desktop_path = format!("{}/flint.desktop", apps_dir);
        if !std::path::Path::new(&desktop_path).exists() {
            let bin_path = std::env::current_exe()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "flint".to_string());
            let desktop = format!(
                "[Desktop Entry]\n\
                 Type=Application\n\
                 Name=flint\n\
                 Comment=Flash ISO files to USB drives\n\
                 Exec={}\n\
                 Icon=flint\n\
                 Terminal=false\n\
                 Categories=Utility;X-GNOME-Utilities;\n\
                 StartupWMClass=flint\n",
                bin_path
            );
            let _ = std::fs::write(&desktop_path, desktop);
        }
    }
}

#[cfg(target_os = "linux")]
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

fn box_fill_from(panel: egui::Color32) -> egui::Color32 {
    let [r, g, b, a] = panel.to_array();
    let avg = (r as u16 + g as u16 + b as u16) / 3;
    if avg > 128 {
        egui::Color32::from_rgba_premultiplied(
            r.saturating_sub(25),
            g.saturating_sub(25),
            b.saturating_sub(25),
            a,
        )
    } else {
        egui::Color32::from_rgba_premultiplied(
            (r as u16 + 25).min(255) as u8,
            (g as u16 + 25).min(255) as u8,
            (b as u16 + 25).min(255) as u8,
            a,
        )
    }
}

fn render_action_ui(ui: &mut egui::Ui, visuals: &egui::Visuals, app: &mut FlintApp) {
    let box_fill = box_fill_from(visuals.panel_fill);
    egui::Frame::default()
        .fill(box_fill)
        .corner_radius(egui::CornerRadius::same(6))
        .inner_margin(egui::Margin::symmetric(12, 12))
        .show(ui, |ui| {
            let avail_h = ui.available_height();
            ui.set_min_height(avail_h);
            ui.add_space(((avail_h - 200.0) * 0.5).max(0.0));

            ui.vertical_centered(|ui| {
                ui.strong("ISO File");
            });
            ui.add_space(4.0);
            ui.vertical_centered(|ui| {
                ui.horizontal(|ui| {
                    let full = ui.available_width();
                    let text_w = (full - 85.0).max(150.0).min(290.0);
                    let pad = ((full - text_w - 10.0 - 75.0) / 2.0).max(0.0);
                    ui.add_space(pad);
                    ui.add(egui::TextEdit::singleline(&mut app.iso_path).desired_width(text_w));
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
                        let text_w = (full - 85.0).max(150.0).min(290.0);
                        let pad = ((full - text_w - 10.0 - 75.0) / 2.0).max(0.0);
                        ui.add_space(pad);
                        egui::ComboBox::from_id_salt("usb_device")
                            .selected_text(app.usb_devices[app.selected_idx].label.as_str())
                            .width(text_w)
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
                    let bar = egui::ProgressBar::new(app.progress)
                        .show_percentage()
                        .animate(true);
                    ui.add(bar);
                    ui.horizontal(|ui| {
                        if app.progress == 0.0 {
                            ui.add(egui::Spinner::default());
                        }
                        ui.label(&app.status);
                    });
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
    let box_fill = box_fill_from(visuals.panel_fill);
    egui::Frame::default()
        .fill(box_fill)
        .corner_radius(egui::CornerRadius::same(6))
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_min_height(ui.available_height());
            egui::ScrollArea::vertical()
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
                    }
                    Message::Status(s) => {
                        self.status = s;
                    }
                    Message::Done(success) => {
                        self.flashing = false;
                        self.cancel.store(false, Ordering::SeqCst);
                        if success {
                            self.status = "Done!".to_string();
                            self.log.push("Flash completed successfully.".to_string());
                            self.dialog_message = "Flash completed successfully!".to_string();
                        } else {
                            self.status = "Failed!".to_string();
                            self.log.push("Flash failed or was cancelled.".to_string());
                            self.dialog_message = "Flash failed!\nCheck the log for more information.".to_string();
                        }
                        self.show_dialog = true;
                    }
                    Message::Log(line) => {
                        self.log.push(line);
                    }
                }
            }
        }

        let gap = 16.0;
        if self.show_log {
            let aw = ui.available_width();
            let ah = ui.available_height();
            let half = (aw - 3.0 * gap) / 2.0;
            let bh = ah - 2.0 * gap;
            ui.add_space(gap);
            ui.horizontal(|ui| {
                ui.add_space(gap);
                ui.allocate_ui_with_layout(
                    egui::vec2(half, bh),
                    egui::Layout::top_down(egui::Align::Center),
                    |ui| render_action_ui(ui, &visuals, self),
                );
                ui.add_space(gap);
                ui.allocate_ui_with_layout(
                    egui::vec2(half, bh),
                    egui::Layout::top_down(egui::Align::Center),
                    |ui| render_log_ui(ui, &visuals, self),
                );
                ui.add_space(gap);
            });
        } else {
            let aw = ui.available_width();
            let ah = ui.available_height();
            let bw = aw - 2.0 * gap;
            let bh = ah - 2.0 * gap;
            ui.add_space(gap);
            ui.horizontal(|ui| {
                ui.add_space(gap);
                ui.allocate_ui_with_layout(
                    egui::vec2(bw, bh),
                    egui::Layout::top_down(egui::Align::Center),
                    |ui| render_action_ui(ui, &visuals, self),
                );
                ui.add_space(gap);
            });
        }

        if self.flashing {
            ui.ctx().request_repaint();
        }

        if self.show_dialog {
            let ctx = ui.ctx();
            egui::Area::new("flash_done_dialog".into())
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    let frame = egui::Frame::default()
                        .fill(ui.visuals().window_fill())
                        .corner_radius(8.0)
                        .inner_margin(egui::Margin::symmetric(24, 16));
                    frame.show(ui, |ui| {
                        ui.set_min_width(280.0);
                        ui.vertical_centered(|ui| {
                            ui.label(egui::RichText::new(&self.dialog_message).size(14.0));
                        });
                        ui.add_space(12.0);
                        ui.vertical_centered(|ui| {
                            if ui.button("OK").clicked() {
                                self.show_dialog = false;
                            }
                        });
                    });
                });
        }
    }
}

fn load_icon() -> egui::IconData {
    let img = image::load_from_memory(include_bytes!("../icon-128.png"))
        .expect("Failed to load icon")
        .into_rgba8();
    let (w, h) = img.dimensions();
    egui::IconData {
        rgba: img.into_raw(),
        width: w,
        height: h,
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(target_os = "linux")]
    FlintApp::setup_desktop_integration();
    let is_dark = detect_is_dark();
    let icon = load_icon();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([720.0, 400.0])
            .with_icon(icon),
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
