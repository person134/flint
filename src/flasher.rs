use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;

use crate::message::Message;
use crate::progress;
use crate::verification;

pub enum ImageType {
    Raw,
    Xz,
    Zip,
}

pub fn detect_image_type(path: &str) -> ImageType {
    let lower = path.to_lowercase();
    if lower.ends_with(".xz") || lower.ends_with(".lzma") {
        ImageType::Xz
    } else if lower.ends_with(".zip") {
        ImageType::Zip
    } else {
        ImageType::Raw
    }
}

pub fn start_flash_thread(
    iso_path: String,
    dev_path: String,
    dev_label: String,
    verify: bool,
    cancel: Arc<AtomicBool>,
    tx: mpsc::Sender<Message>,
) {
    let img_type = detect_image_type(&iso_path);

    let flash_ok = match img_type {
        ImageType::Raw => flash_raw(&iso_path, &dev_path, &dev_label, &cancel, &tx),
        ImageType::Xz => flash_xz(&iso_path, &dev_path, &dev_label, &cancel, &tx),
        ImageType::Zip => flash_zip(&iso_path, &dev_path, &dev_label, &cancel, &tx),
    };

    if !flash_ok || cancel.load(Ordering::SeqCst) {
        let _ = tx.send(Message::Done(false, None));
        return;
    }

    if verify {
        let iso_size = get_image_size(&iso_path);
        if let Some(size) = iso_size {
            verification::verify_flash(&iso_path, &dev_path, size, cancel, tx);
            return;
        }
        let _ = tx.send(Message::Log("Verification skipped: unknown uncompressed size".to_string()));
    }

    let _ = tx.send(Message::Done(true, None));
}

fn get_image_size(path: &str) -> Option<u64> {
    match detect_image_type(path) {
        ImageType::Raw => std::fs::metadata(path).ok().map(|m| m.len()),
        ImageType::Zip => {
            let file = std::fs::File::open(path).ok()?;
            let mut archive = zip::ZipArchive::new(file).ok()?;
            if archive.len() == 0 {
                return None;
            }
            archive.by_index(0).ok().map(|e| e.size())
        }
        ImageType::Xz => None,
    }
}

fn flash_raw(
    iso_path: &str,
    dev_path: &str,
    dev_label: &str,
    cancel: &Arc<AtomicBool>,
    tx: &mpsc::Sender<Message>,
) -> bool {
    #[cfg(target_os = "linux")]
    return flash_raw_linux(iso_path, dev_path, dev_label, cancel, tx);
    #[cfg(target_os = "windows")]
    return flash_raw_windows(iso_path, dev_path, dev_label, cancel, tx);
    #[cfg(target_os = "macos")]
    return flash_raw_macos(iso_path, dev_path, dev_label, cancel, tx);
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        send_log(tx, "Unsupported platform".to_string());
        false
    }
}

#[cfg(target_os = "linux")]
fn flash_raw_linux(
    iso_path: &str,
    dev_path: &str,
    dev_label: &str,
    cancel: &Arc<AtomicBool>,
    tx: &mpsc::Sender<Message>,
) -> bool {
    let total = Command::new("stat")
        .args(["-c", "%s", iso_path])
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8(o.stdout)
                .ok()
                .and_then(|s| s.trim().parse::<u64>().ok())
        })
        .unwrap_or(0);

    if total == 0 {
        send_log(tx, "Failed to get ISO file size".to_string());
        return false;
    }

    let total_mb = total as f64 / 1_048_576.0;
    send_log(tx, format!("ISO: {} ({:.1} MB)", iso_path, total_mb));
    send_log(tx, format!("Device: {} ({})", dev_label, dev_path));

    let root = is_root_linux();
    send_log(tx, format!("Running as admin: {}", root));

    let mut child = if root {
        match Command::new("dd")
            .arg(format!("if={}", iso_path))
            .arg(format!("of={}", dev_path))
            .args(["bs=4M", "status=progress", "conv=fsync", "iflag=fullblock"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                send_log(tx, format!("Failed to start dd: {}", e));
                return false;
            }
        }
    } else {
        match Command::new("pkexec")
            .args([
                "dd",
                &format!("if={}", iso_path),
                &format!("of={}", dev_path),
                "bs=4M",
                "status=progress",
                "conv=fsync",
                "iflag=fullblock",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                send_log(tx, format!("Failed to start dd via pkexec: {}", e));
                return false;
            }
        }
    };

    let stderr = child.stderr.take().expect("stderr not captured");
    let reader = BufReader::new(stderr);

    for line_result in reader.lines() {
        if cancel.load(Ordering::SeqCst) {
            let _ = child.kill();
            let _ = child.wait();
            send_log(tx, "Cancelled by user".to_string());
            return false;
        }

        let line = match line_result {
            Ok(l) => l,
            Err(_) => break,
        };

        let trimmed = line.trim().to_string();
        if trimmed.is_empty() {
            continue;
        }

        if let Some(bytes) = progress::parse_dd_progress(&trimmed) {
            let pct = bytes as f64 / total as f64;
            let done_mb = bytes as f64 / 1_048_576.0;
            let _ = tx.send(Message::Progress(bytes, total));
            let _ = tx.send(Message::Status(format!(
                "Flashing... {:.1}% ({:.1} / {:.1} MB)",
                pct * 100.0, done_mb, total_mb
            )));
        }
        let _ = tx.send(Message::Log(trimmed));
    }

    child.wait().map(|s| s.success()).unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn is_root_linux() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<u32>().ok())
        == Some(0)
}

#[cfg(target_os = "windows")]
fn flash_raw_windows(
    iso_path: &str,
    dev_path: &str,
    dev_label: &str,
    cancel: &Arc<AtomicBool>,
    tx: &mpsc::Sender<Message>,
) -> bool {
    let total = std::fs::metadata(iso_path).map(|m| m.len()).unwrap_or(0);
    if total == 0 {
        send_log(tx, "Failed to get ISO file size".to_string());
        return false;
    }

    let total_mb = total as f64 / 1_048_576.0;
    send_log(tx, format!("ISO: {} ({:.1} MB)", iso_path, total_mb));
    send_log(tx, format!("Device: {} ({})", dev_label, dev_path));

    let mut inp = match std::fs::File::open(iso_path) {
        Ok(f) => f,
        Err(e) => {
            send_log(tx, format!("Failed to open ISO: {}", e));
            return false;
        }
    };

    let mut out = match std::fs::OpenOptions::new().write(true).open(dev_path) {
        Ok(f) => f,
        Err(e) => {
            send_log(tx, format!("Failed to open device (run as admin): {}", e));
            return false;
        }
    };

    let mut buf = vec![0u8; 4 * 1024 * 1024];
    let mut written: u64 = 0;

    loop {
        if cancel.load(Ordering::SeqCst) {
            send_log(tx, "Cancelled by user".to_string());
            return false;
        }

        let n = match inp.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                send_log(tx, format!("Read error: {}", e));
                return false;
            }
        };

        if let Err(e) = out.write_all(&buf[..n]) {
            send_log(tx, format!("Write error: {}", e));
            return false;
        }

        written += n as u64;
        let pct = written as f64 / total as f64;
        let done_mb = written as f64 / 1_048_576.0;
        let _ = tx.send(Message::Progress(written, total));
        let _ = tx.send(Message::Status(format!(
            "Flashing... {:.1}% ({:.1} / {:.1} MB)",
            pct * 100.0, done_mb, total_mb
        )));
    }

    true
}

#[cfg(target_os = "macos")]
fn flash_raw_macos(
    iso_path: &str,
    dev_path: &str,
    dev_label: &str,
    cancel: &Arc<AtomicBool>,
    tx: &mpsc::Sender<Message>,
) -> bool {
    let total = Command::new("stat")
        .args(["-f", "%z", iso_path])
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8(o.stdout)
                .ok()
                .and_then(|s| s.trim().parse::<u64>().ok())
        })
        .unwrap_or(0);

    if total == 0 {
        send_log(tx, "Failed to get ISO file size".to_string());
        return false;
    }

    let total_mb = total as f64 / 1_048_576.0;
    send_log(tx, format!("ISO: {} ({:.1} MB)", iso_path, total_mb));
    send_log(tx, format!("Device: {} ({})", dev_label, dev_path));

    let raw_dev = dev_path.replace("/dev/disk", "/dev/rdisk");

    let _ = Command::new("diskutil").args(["unmountDisk", dev_path]).output();
    send_log(tx, "Disk unmounted".to_string());

    if !is_root_macos() {
        send_log(tx, "Running as root is recommended on macOS".to_string());
    }

    let mut child = match Command::new("dd")
        .arg(format!("if={}", iso_path))
        .arg(format!("of={}", raw_dev))
        .args(["bs=4m", "status=progress"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            send_log(tx, format!("Failed to start dd: {}", e));
            let _ = Command::new("diskutil").args(["eject", dev_path]).output();
            return false;
        }
    };

    let stderr = child.stderr.take().expect("stderr not captured");
    let reader = BufReader::new(stderr);

    for line_result in reader.lines() {
        if cancel.load(Ordering::SeqCst) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = Command::new("diskutil").args(["eject", dev_path]).output();
            send_log(tx, "Cancelled by user".to_string());
            return false;
        }

        let line = match line_result {
            Ok(l) => l,
            Err(_) => break,
        };

        let trimmed = line.trim().to_string();
        if trimmed.is_empty() {
            continue;
        }

        if let Some(bytes) = progress::parse_dd_progress(&trimmed) {
            let pct = bytes as f64 / total as f64;
            let done_mb = bytes as f64 / 1_048_576.0;
            let _ = tx.send(Message::Progress(bytes, total));
            let _ = tx.send(Message::Status(format!(
                "Flashing... {:.1}% ({:.1} / {:.1} MB)",
                pct * 100.0, done_mb, total_mb
            )));
        }
        let _ = tx.send(Message::Log(trimmed));
    }

    let success = child.wait().map(|s| s.success()).unwrap_or(false);
    let _ = Command::new("diskutil").args(["eject", dev_path]).output();
    success
}

#[cfg(target_os = "macos")]
fn is_root_macos() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<u32>().ok())
        == Some(0)
}

fn flash_xz(
    iso_path: &str,
    dev_path: &str,
    _dev_label: &str,
    cancel: &Arc<AtomicBool>,
    tx: &mpsc::Sender<Message>,
) -> bool {
    let total_compressed = std::fs::metadata(iso_path).map(|m| m.len()).unwrap_or(0);
    send_log(
        tx,
        format!(
            "XZ: {} ({:.1} MB compressed)",
            iso_path,
            total_compressed as f64 / 1_048_576.0
        ),
    );

    let file = match std::fs::File::open(iso_path) {
        Ok(f) => f,
        Err(e) => {
            send_log(tx, format!("Failed to open file: {}", e));
            return false;
        }
    };

    let mut out = match open_device_for_write(dev_path) {
        Ok(f) => f,
        Err(e) => {
            send_log(tx, format!("Failed to open device: {}", e));
            return false;
        }
    };

    let progress_reader = ProgressReader {
        inner: BufReader::new(file),
        total: total_compressed,
        read_so_far: 0,
        tx: tx.clone(),
        cancel: cancel.clone(),
    };

    let mut counted_reader = BufReader::new(progress_reader);

    match lzma_rs::xz_decompress(&mut counted_reader, &mut out) {
        Ok(()) => {
            send_log(tx, "XZ decompression done.".to_string());
            true
        }
        Err(e) => {
            if cancel.load(Ordering::SeqCst) {
                send_log(tx, "Cancelled by user".to_string());
            } else {
                send_log(tx, format!("Decompression error: {}", e));
            }
            false
        }
    }
}

struct ProgressReader<R> {
    inner: R,
    total: u64,
    read_so_far: u64,
    tx: mpsc::Sender<Message>,
    cancel: Arc<AtomicBool>,
}

impl<R: Read> Read for ProgressReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.cancel.load(Ordering::SeqCst) {
            return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "cancelled"));
        }
        let n = self.inner.read(buf)?;
        self.read_so_far += n as u64;
        if self.total > 0 {
            let pct = (self.read_so_far as f64 / self.total as f64).min(1.0);
            let _ = self
                .tx
                .send(Message::Status(format!("Decompressing... {:.1}%", pct * 100.0)));
        }
        Ok(n)
    }
}

fn flash_zip(
    iso_path: &str,
    dev_path: &str,
    _dev_label: &str,
    cancel: &Arc<AtomicBool>,
    tx: &mpsc::Sender<Message>,
) -> bool {
    let file = match std::fs::File::open(iso_path) {
        Ok(f) => f,
        Err(e) => {
            send_log(tx, format!("Failed to open ZIP: {}", e));
            return false;
        }
    };

    let mut archive = match zip::ZipArchive::new(file) {
        Ok(a) => a,
        Err(e) => {
            send_log(tx, format!("Failed to read ZIP archive: {}", e));
            return false;
        }
    };

    if archive.len() == 0 {
        send_log(tx, "ZIP archive is empty".to_string());
        return false;
    }

    let entry_idx = find_best_zip_entry(&mut archive);
    let mut entry = match archive.by_index(entry_idx) {
        Ok(e) => e,
        Err(e) => {
            send_log(tx, format!("Failed to read ZIP entry: {}", e));
            return false;
        }
    };

    let uncompressed_size = entry.size();
    let entry_name = entry.name().to_string();
    send_log(
        tx,
        format!(
            "Extracting from ZIP: {} ({} MB)",
            entry_name,
            uncompressed_size as f64 / 1_048_576.0
        ),
    );

    let mut out = match open_device_for_write(dev_path) {
        Ok(f) => f,
        Err(e) => {
            send_log(tx, format!("Failed to open device: {}", e));
            return false;
        }
    };

    let mut buf = vec![0u8; 1_048_576];
    let mut written: u64 = 0;

    loop {
        if cancel.load(Ordering::SeqCst) {
            send_log(tx, "Cancelled by user".to_string());
            return false;
        }

        let n = match entry.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                send_log(tx, format!("Extraction error: {}", e));
                return false;
            }
        };

        if let Err(e) = out.write_all(&buf[..n]) {
            send_log(tx, format!("Write error: {}", e));
            return false;
        }

        written += n as u64;

        if uncompressed_size > 0 {
            let pct = written as f64 / uncompressed_size as f64;
            let _ = tx.send(Message::Progress(written, uncompressed_size));
            let _ = tx.send(Message::Status(format!(
                "Extracting... {:.1}% ({:.1} / {:.1} MB)",
                pct * 100.0,
                written as f64 / 1_048_576.0,
                uncompressed_size as f64 / 1_048_576.0
            )));
        }
    }

    send_log(tx, format!("Extraction done. {} bytes written.", written));
    true
}

fn find_best_zip_entry(archive: &mut zip::ZipArchive<std::fs::File>) -> usize {
    let mut best = 0;
    let mut best_size = 0;

    for i in 0..archive.len() {
        if let Ok(entry) = archive.by_index(i) {
            let name = entry.name().to_lowercase();
            let size = entry.size();
            if size > best_size
                && (name.ends_with(".iso")
                    || name.ends_with(".img")
                    || name.ends_with(".raw")
                    || name.ends_with(".bin"))
            {
                best = i;
                best_size = size;
            }
            if best == 0 && size > best_size {
                best = i;
                best_size = size;
            }
        }
    }

    best
}

fn send_log(tx: &mpsc::Sender<Message>, msg: String) {
    let _ = tx.send(Message::Log(msg));
}

fn open_device_for_write(path: &str) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new().write(true).open(path)
}
