use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;

use eframe::egui;

use crate::config;
use crate::devices;
use crate::flasher;
use crate::message::{Message, UsbDevice};

pub struct FlintApp {
    iso_path: String,
    usb_devices: Vec<UsbDevice>,
    selected_idx: usize,
    flashing: bool,
    verifying: bool,
    progress: f32,
    status: String,
    log: Vec<String>,
    rx: Option<mpsc::Receiver<Message>>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
    dark_mode: bool,
    show_log: bool,
    show_dialog: bool,
    dialog_message: String,
    verify_after_flash: bool,
    confirm_input: String,
}

impl FlintApp {
    pub fn new() -> Self {
        let devices = devices::detect_usb_devices();
        Self {
            iso_path: String::new(),
            usb_devices: devices,
            selected_idx: 0,
            flashing: false,
            verifying: false,
            progress: 0.0,
            status: "Ready".to_string(),
            log: vec!["flint ready".to_string()],
            rx: None,
            cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            dark_mode: config::detect_is_dark(),
            show_log: false,
            show_dialog: false,
            dialog_message: String::new(),
            verify_after_flash: true,
            confirm_input: String::new(),
        }
    }

    fn refresh_devices(&mut self) {
        self.usb_devices = devices::detect_usb_devices();
        if self.selected_idx >= self.usb_devices.len() {
            self.selected_idx = self.usb_devices.len().saturating_sub(1);
        }
        self.confirm_input.clear();
    }

    fn start_flash(&mut self) {
        if self.iso_path.is_empty() || self.usb_devices.is_empty() {
            return;
        }
        let idx = self.selected_idx;
        let iso_path = self.iso_path.clone();
        let dev_path = self.usb_devices[idx].path.clone();
        let dev_label = self.usb_devices[idx].label.clone();
        let verify = self.verify_after_flash;

        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        self.flashing = true;
        self.verifying = false;
        self.progress = 0.0;
        self.status = "Starting...".to_string();
        self.cancel.store(false, Ordering::SeqCst);
        let cancel = self.cancel.clone();

        thread::spawn(move || {
            flasher::start_flash_thread(iso_path, dev_path, dev_label, verify, cancel, tx);
        });
    }

    fn cancel_flash(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        self.status = "Cancelling...".to_string();
    }

    fn device_short_name(&self) -> String {
        if self.usb_devices.is_empty() || self.selected_idx >= self.usb_devices.len() {
            return String::new();
        }
        extract_device_short_name(&self.usb_devices[self.selected_idx].path)
    }

    fn can_flash(&self) -> bool {
        if self.iso_path.is_empty() || self.usb_devices.is_empty() {
            return false;
        }
        let short = self.device_short_name();
        !short.is_empty() && self.confirm_input.trim() == short
    }
}

fn extract_device_short_name(path: &str) -> String {
    path.rsplit('/')
        .next()
        .unwrap_or("")
        .trim_start_matches("\\\\.\\")
        .to_string()
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

fn warning_fill(visuals: &egui::Visuals) -> egui::Color32 {
    if visuals.dark_mode {
        egui::Color32::from_rgba_premultiplied(180, 60, 40, 30)
    } else {
        egui::Color32::from_rgba_premultiplied(255, 200, 180, 40)
    }
}

fn render_action_ui(ui: &mut egui::Ui, visuals: &egui::Visuals, app: &mut FlintApp) {
    let box_fill = box_fill_from(visuals.panel_fill);
    egui::Frame::default()
        .fill(box_fill)
        .corner_radius(6.0)
        .inner_margin(egui::Margin::symmetric(16, 14))
        .show(ui, |ui| {
            ui.set_min_height(ui.available_height());

            ui.vertical_centered(|ui| {
                ui.heading(egui::RichText::new("flint").size(20.0).strong());
                ui.label(
                    egui::RichText::new("Flash ISO images to USB drives")
                        .size(11.0)
                        .color(egui::Color32::GRAY),
                );
            });

            ui.add_space(14.0);

            egui::Frame::default()
                .inner_margin(egui::Margin::symmetric(4, 4))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.strong("ISO File");
                        if !app.iso_path.is_empty() {
                            if let Ok(meta) = std::fs::metadata(&app.iso_path) {
                                let size_mb = meta.len() as f64 / 1_048_576.0;
                                if size_mb > 0.0 {
                                    ui.label(
                                        egui::RichText::new(format!("({:.1} MB)", size_mb))
                                            .size(10.0)
                                            .color(egui::Color32::GRAY),
                                    );
                                }
                            }
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Browse").clicked() {
                                let dialog = rfd::FileDialog::new()
                                    .add_filter("ISO", &["iso"])
                                    .add_filter("Disk images", &["img", "raw"])
                                    .add_filter("Compressed", &["xz", "zip", "lzma"])
                                    .add_filter("All files", &["*"]);
                                if let Some(path) = dialog.pick_file() {
                                    app.iso_path = path.display().to_string();
                                    app.confirm_input.clear();
                                }
                            }
                        });
                    });
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        let w = ui.available_width();
                        ui.add(
                            egui::TextEdit::singleline(&mut app.iso_path)
                                .desired_width(w)
                                .hint_text("Select an ISO file..."),
                        );
                    });
                });

            ui.add_space(10.0);

            egui::Frame::default()
                .inner_margin(egui::Margin::symmetric(4, 4))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.strong("USB Device");
                        if let Some(dev) = app.usb_devices.get(app.selected_idx) {
                            if dev.size > 0 {
                                let size_gb = dev.size as f64 / 1_000_000_000.0;
                                ui.label(
                                    egui::RichText::new(format!("({:.1} GB)", size_gb))
                                        .size(10.0)
                                        .color(egui::Color32::GRAY),
                                );
                            }
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Refresh").clicked() {
                                app.refresh_devices();
                            }
                        });
                    });
                    ui.add_space(4.0);
                    if app.usb_devices.is_empty() {
                        ui.label(
                            egui::RichText::new("No USB devices detected")
                                .color(egui::Color32::GRAY),
                        );
                    } else {
                        let w = ui.available_width();
                        egui::ComboBox::from_id_salt("usb_device")
                            .selected_text(app.usb_devices[app.selected_idx].label.as_str())
                            .width(w)
                            .show_ui(ui, |ui| {
                                for (i, dev) in app.usb_devices.iter().enumerate() {
                                    ui.selectable_value(&mut app.selected_idx, i, &dev.label);
                                }
                            });

                        if app.selected_idx < app.usb_devices.len() {
                            let short = app.device_short_name();
                            if !short.is_empty() {
                                ui.add_space(8.0);

                                egui::Frame::default()
                                    .fill(warning_fill(visuals))
                                    .corner_radius(4)
                                    .inner_margin(egui::Margin::symmetric(8, 6))
                                    .show(ui, |ui| {
                                        ui.label(
                                            egui::RichText::new(
                                                "All data on this device will be destroyed!",
                                            )
                                            .size(11.0)
                                            .color(
                                                if visuals.dark_mode {
                                                    egui::Color32::LIGHT_RED
                                                } else {
                                                    egui::Color32::DARK_RED
                                                },
                                            )
                                            .strong(),
                                        );
                                        ui.add_space(4.0);
                                        ui.horizontal(|ui| {
                                            ui.label(
                                                egui::RichText::new(format!(
                                                    "Type \"{}\" to confirm:",
                                                    short
                                                ))
                                                .size(11.0),
                                            );
                                            ui.add(
                                                egui::TextEdit::singleline(
                                                    &mut app.confirm_input,
                                                )
                                                .desired_width(120.0)
                                                .hint_text(&short),
                                            );
                                        });
                                        let matched = app.confirm_input.trim() == short;
                                        if !app.confirm_input.is_empty() && !matched {
                                            ui.label(
                                                egui::RichText::new("Name does not match")
                                                    .size(10.0)
                                                    .color(egui::Color32::RED),
                                            );
                                        }
                                        if matched {
                                            ui.label(
                                                egui::RichText::new("Device confirmed")
                                                    .size(10.0)
                                                    .color(egui::Color32::GREEN),
                                            );
                                        }
                                    });

                                ui.add_space(4.0);

                                ui.horizontal(|ui| {
                                    ui.checkbox(
                                        &mut app.verify_after_flash,
                                        "Verify after flashing",
                                    );
                                    if app.verify_after_flash {
                                        ui.label(
                                            egui::RichText::new("(SHA256)")
                                                .size(10.0)
                                                .color(egui::Color32::GRAY),
                                        );
                                    }
                                });
                            }
                        }
                    }
                });

            ui.add_space(12.0);
            ui.separator();
            ui.add_space(8.0);

            ui.vertical_centered(|ui| {
                if app.flashing || app.verifying {
                    ui.horizontal(|ui| {
                        let bt = egui::Button::new("Cancel").min_size(egui::Vec2::new(100.0, 32.0));
                        if ui.add(bt).clicked() {
                            app.cancel_flash();
                        }
                        ui.add(egui::Spinner::default());
                    });

                    ui.add_space(6.0);
                    let bar = egui::ProgressBar::new(app.progress)
                        .show_percentage()
                        .animate(app.progress > 0.0 && app.progress < 1.0);
                    ui.add(bar);

                    ui.add_space(4.0);
                    ui.label(&app.status);
                } else {
                    let can_flash = app.can_flash();
                    let btn = egui::Button::new(egui::RichText::new("Start flashing").size(16.0))
                        .min_size(egui::Vec2::new(220.0, 40.0))
                        .fill(visuals.selection.bg_fill);
                    if ui.add_enabled(can_flash, btn).clicked() {
                        app.start_flash();
                    }

                    if !app.usb_devices.is_empty()
                        && !app.confirm_input.is_empty()
                        && app.confirm_input.trim() != app.device_short_name()
                    {
                        ui.label(
                            egui::RichText::new("Device name does not match")
                                .size(10.0)
                                .color(egui::Color32::RED),
                        );
                    } else if !app.usb_devices.is_empty()
                        && app.confirm_input.trim() != app.device_short_name()
                    {
                        ui.label(
                            egui::RichText::new("Type the device name to confirm")
                                .size(10.0)
                                .color(egui::Color32::GRAY),
                        );
                    }
                    if app.iso_path.is_empty() || app.usb_devices.is_empty() {
                        ui.label(
                            egui::RichText::new("Select an ISO file and USB device")
                                .size(10.0)
                                .color(egui::Color32::GRAY),
                        );
                    }
                }

                ui.add_space(6.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                    let term_btn = egui::Button::new(
                        egui::RichText::new(">_").monospace().size(14.0),
                    )
                    .min_size(egui::Vec2::new(30.0, 22.0))
                    .selected(app.show_log);
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
        .corner_radius(6.0)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_min_height(ui.available_height());
            ui.strong("Log");
            ui.add_space(4.0);
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
                    Message::Done(success, _verify_msg) => {
                        self.flashing = false;
                        self.verifying = false;
                        self.cancel.store(false, Ordering::SeqCst);
                        if success {
                            self.status = "Done!".to_string();
                            self.log.push("Flash completed successfully.".to_string());
                            self.dialog_message = "Flash completed successfully!".to_string();
                        } else {
                            self.status = "Failed!".to_string();
                            self.log.push("Flash failed or was cancelled.".to_string());
                            self.dialog_message =
                                "Flash failed!\nCheck the log for details.".to_string();
                        }
                        self.show_dialog = true;
                    }
                    Message::Log(line) => {
                        self.log.push(line);
                    }
                    Message::VerifyProgress(p) => {
                        self.verifying = true;
                        self.progress = p;
                    }
                    Message::VerifyDone(success, msg) => {
                        self.verifying = false;
                        self.flashing = false;
                        self.cancel.store(false, Ordering::SeqCst);
                        self.status = if success { "Verified!".to_string() } else { "Verify failed!".to_string() };
                        self.dialog_message = msg;
                        self.show_dialog = true;
                    }
                }
            }
        }

        let gap = 12.0;
        let margin = 8.0;

        if self.show_log {
            let aw = ui.available_width();
            let ah = ui.available_height();
            let half = (aw - 3.0 * gap) / 2.0;
            let bh = ah - 2.0 * margin;
            ui.add_space(margin);
            ui.horizontal(|ui| {
                ui.add_space(margin);
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
                ui.add_space(margin);
            });
        } else {
            let aw = ui.available_width();
            let ah = ui.available_height();
            let bw = aw - 2.0 * margin;
            let bh = ah - 2.0 * margin;
            ui.add_space(margin);
            ui.horizontal(|ui| {
                ui.add_space(margin);
                ui.allocate_ui_with_layout(
                    egui::vec2(bw, bh),
                    egui::Layout::top_down(egui::Align::Center),
                    |ui| render_action_ui(ui, &visuals, self),
                );
                ui.add_space(margin);
            });
        }

        if self.flashing || self.verifying {
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
                        ui.set_min_width(300.0);
                        ui.vertical_centered(|ui| {
                            ui.label(egui::RichText::new(&self.dialog_message).size(14.0));
                        });
                        ui.add_space(12.0);
                        ui.vertical_centered(|ui| {
                            if ui.button("OK").clicked() {
                                self.show_dialog = false;
                                self.confirm_input.clear();
                            }
                        });
                    });
                });
        }
    }
}
