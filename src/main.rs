mod message;
mod progress;
mod config;
mod devices;
mod verification;
mod flasher;
mod app;

use app::FlintApp;
use config::detect_is_dark;

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
    config::setup_desktop_integration();

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
