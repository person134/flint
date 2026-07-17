use std::process::Command;

use crate::message::UsbDevice;

pub fn detect_usb_devices() -> Vec<UsbDevice> {
    #[cfg(target_os = "linux")]
    {
        detect_linux()
    }
    #[cfg(target_os = "windows")]
    {
        detect_windows()
    }
    #[cfg(target_os = "macos")]
    {
        detect_macos()
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        Vec::new()
    }
}

#[cfg(target_os = "linux")]
fn detect_linux() -> Vec<UsbDevice> {
    let mut devices = Vec::new();

    let output = match Command::new("lsblk")
        .args(["-Jpo", "name,size,model,tran"])
        .output()
    {
        Ok(o) => o,
        Err(_) => return devices,
    };

    #[derive(serde::Deserialize)]
    struct LsblkDevices {
        blockdevices: Vec<LsblkDevice>,
    }
    #[derive(serde::Deserialize)]
    struct LsblkDevice {
        name: String,
        size: String,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        tran: Option<String>,
    }

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
        let size_bytes = parse_size(&dev.size).unwrap_or(0);
        let label = if let Some(model) = &dev.model {
            format!("{} ({} - {})", dev.name, model.trim(), dev.size)
        } else {
            format!("{} ({})", dev.name, dev.size)
        };
        devices.push(UsbDevice {
            path: dev.name.clone(),
            label,
            size: size_bytes,
        });
    }

    devices
}

#[cfg(target_os = "linux")]
fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim();
    if let Some(num) = s.strip_suffix('B') {
        let num = num.trim();
        if let Some(num) = num.strip_suffix('G') {
            num.trim().parse::<f64>().ok().map(|v| (v * 1_000_000_000.0) as u64)
        } else if let Some(num) = num.strip_suffix('M') {
            num.trim().parse::<f64>().ok().map(|v| (v * 1_000_000.0) as u64)
        } else if let Some(num) = num.strip_suffix('T') {
            num.trim().parse::<f64>().ok().map(|v| (v * 1_000_000_000_000.0) as u64)
        } else {
            s.parse::<u64>().ok()
        }
    } else {
        s.parse::<u64>().ok()
    }
}

#[cfg(target_os = "windows")]
fn detect_windows() -> Vec<UsbDevice> {
    let mut devices = Vec::new();
    if let Ok(output) = Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "Get-CimInstance Win32_DiskDrive -Filter 'InterfaceType=''USB''' | Select-Object DeviceID,Model,Size | ConvertTo-Csv -NoTypeInformation",
        ])
        .output()
    {
        let s = String::from_utf8_lossy(&output.stdout);
        for line in s.lines().skip(1) {
            let parts: Vec<&str> = line.split(',').collect();
            if parts.len() >= 3 {
                let dev_id = parts.first().unwrap_or(&"").trim().trim_matches('"');
                let model = parts.get(1).unwrap_or(&"").trim().trim_matches('"');
                let size_str = parts.get(2).unwrap_or(&"").trim().trim_matches('"');
                let size = size_str.parse::<u64>().unwrap_or(0);
                if !dev_id.is_empty() {
                    let label = format!("{} ({})", dev_id, model);
                    devices.push(UsbDevice {
                        path: dev_id.to_string(),
                        label,
                        size,
                    });
                }
            }
        }
    }
    devices
}

#[cfg(target_os = "macos")]
fn detect_macos() -> Vec<UsbDevice> {
    let mut devices = Vec::new();

    let output = match Command::new("diskutil")
        .args(["list", "external"])
        .output()
    {
        Ok(o) => String::from_utf8_lossy(&o.stdout).to_string(),
        Err(_) => return devices,
    };

    for line in output.lines() {
        let line = line.trim();
        if let Some(path) = line.strip_prefix("/dev/disk") {
            let disk_num: String = path.chars().take_while(|c| c.is_ascii_digit()).collect();
            if disk_num.is_empty() {
                continue;
            }
            let dev_path = format!("/dev/disk{}", disk_num);

            if let Ok(info) = Command::new("diskutil").args(["info", &dev_path]).output() {
                let info_str = String::from_utf8_lossy(&info.stdout);

                if info_str.contains("Internal:         No")
                    || info_str.contains("Internal:         NO")
                {
                    let mut model = String::from("Unknown");
                    let mut size: u64 = 0;

                    for info_line in info_str.lines() {
                        let info_line = info_line.trim();
                        if let Some(m) = info_line.strip_prefix("Device / Media Name:")
                            .or_else(|| info_line.strip_prefix("Media Name:"))
                        {
                            model = m.trim().to_string();
                        }
                        if let Some(s) = info_line.strip_prefix("Disk Size:") {
                            let s = s.trim();
                            if let Some(num_end) = s.find(" bytes") {
                                size = s[..num_end].trim().parse::<u64>().unwrap_or(0);
                            }
                        }
                    }

                    if info_str.contains("Whole:            Yes")
                        || info_str.contains("Virtual Whole:    Yes")
                    {
                        let human_size = if size > 0 {
                            format_size_mac(size)
                        } else {
                            String::from("Unknown")
                        };
                        let label = format!("{} ({} - {})", dev_path, model, human_size);
                        devices.push(UsbDevice {
                            path: dev_path,
                            label,
                            size,
                        });
                    }
                }
            }
        }
    }

    devices
}

#[cfg(target_os = "macos")]
fn format_size_mac(bytes: u64) -> String {
    if bytes >= 1_000_000_000_000 {
        format!("{:.1} TB", bytes as f64 / 1_000_000_000_000.0)
    } else if bytes >= 1_000_000_000 {
        format!("{:.1} GB", bytes as f64 / 1_000_000_000.0)
    } else if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    } else {
        format!("{} B", bytes)
    }
}
