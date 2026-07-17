use std::process::Command;

pub fn detect_is_dark() -> bool {
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

        let home = std::env::var("HOME").unwrap_or_default();
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

    #[cfg(target_os = "macos")]
    {
        if let Ok(output) = Command::new("defaults")
            .args(["read", "-g", "AppleInterfaceStyle"])
            .output()
        {
            let s = String::from_utf8_lossy(&output.stdout);
            if s.trim().eq_ignore_ascii_case("Dark") {
                return true;
            }
        }
    }

    false
}

#[cfg(target_os = "linux")]
pub fn setup_desktop_integration() {
    let home = match std::env::var("HOME") {
        Ok(h) => h,
        Err(_) => return,
    };

    let desktop_path = format!("{}/.local/share/applications/flint.desktop", home);
    if std::fs::metadata(&desktop_path).is_ok() {
        return;
    }

    let img = match image::load_from_memory(include_bytes!("../icon-128.png")) {
        Ok(i) => i.into_rgba8(),
        Err(_) => return,
    };

    for &size in &[128, 64, 48, 32] {
        let scaled = image::imageops::resize(&img, size, size, image::imageops::FilterType::Lanczos3);
        let dir = format!("{}/.local/share/icons/hicolor/{}x{}/apps", home, size, size);
        let _ = std::fs::create_dir_all(&dir);
        let _ = image::DynamicImage::ImageRgba8(scaled).save(format!("{}/flint.png", dir));
    }

    let _ = std::process::Command::new("gtk-update-icon-cache")
        .args(["-f", "-t", &format!("{}/.local/share/icons/hicolor", home)])
        .output();

    let apps_dir = format!("{}/.local/share/applications", home);
    let _ = std::fs::create_dir_all(&apps_dir);
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

    let _ = std::process::Command::new("update-desktop-database")
        .arg(&apps_dir)
        .output();
}
