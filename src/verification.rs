use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::message::Message;

pub fn verify_flash(
    iso_path: &str,
    dev_path: &str,
    size: u64,
    cancel: Arc<AtomicBool>,
    tx: mpsc::Sender<Message>,
) {
    if size == 0 {
        let _ = tx.send(Message::Log("Cannot verify: unknown size".to_string()));
        let _ = tx.send(Message::VerifyDone(false, "Verification skipped (unknown size)".to_string()));
        return;
    }

    let _ = tx.send(Message::Log("Verifying flash...".to_string()));
    let _ = tx.send(Message::Status("Verifying...".to_string()));
    let _ = tx.send(Message::VerifyProgress(0.0));

    let iso_hash = match hash_file(iso_path, size, cancel.clone(), &tx) {
        Some(h) => h,
        None => {
            let _ = tx.send(Message::Log("Verification cancelled or failed".to_string()));
            let _ = tx.send(Message::VerifyDone(false, "Verification failed".to_string()));
            return;
        }
    };

    let _ = tx.send(Message::Log(format!("Source SHA256: {}", hex::encode(&iso_hash))));
    let _ = tx.send(Message::VerifyProgress(0.5));

    let dev_hash = match hash_device(dev_path, size, cancel, &tx) {
        Some(h) => h,
        None => {
            let _ = tx.send(Message::Log("Verification cancelled or failed".to_string()));
            let _ = tx.send(Message::VerifyDone(false, "Verification failed".to_string()));
            return;
        }
    };

    let _ = tx.send(Message::Log(format!("Device SHA256: {}", hex::encode(&dev_hash))));
    let _ = tx.send(Message::VerifyProgress(1.0));

    if iso_hash == dev_hash {
        let _ = tx.send(Message::Log("Verification PASSED - hashes match".to_string()));
        let _ = tx.send(Message::VerifyDone(true, "Flash verified successfully!".to_string()));
    } else {
        let _ = tx.send(Message::Log("Verification FAILED - hashes differ".to_string()));
        let _ = tx.send(Message::VerifyDone(false, "Verification FAILED - data may be corrupted".to_string()));
    }
}

fn hash_file(path: &str, max_bytes: u64, cancel: Arc<AtomicBool>, _tx: &mpsc::Sender<Message>) -> Option<Vec<u8>> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1_048_576];
    let mut remaining = max_bytes;

    loop {
        if cancel.load(Ordering::SeqCst) {
            return None;
        }

        let to_read = buf.len().min(remaining as usize);
        if to_read == 0 {
            break;
        }

        let n = file.read(&mut buf[..to_read]).ok()?;
        if n == 0 {
            break;
        }

        hasher.update(&buf[..n]);
        remaining -= n as u64;
    }

    Some(hasher.finalize().to_vec())
}

fn hash_device(path: &str, max_bytes: u64, cancel: Arc<AtomicBool>, tx: &mpsc::Sender<Message>) -> Option<Vec<u8>> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1_048_576];
    let mut remaining = max_bytes;
    let total = max_bytes;

    loop {
        if cancel.load(Ordering::SeqCst) {
            return None;
        }

        let to_read = buf.len().min(remaining as usize);
        if to_read == 0 {
            break;
        }

        let n = file.read(&mut buf[..to_read]).ok()?;
        if n == 0 {
            break;
        }

        hasher.update(&buf[..n]);
        remaining -= n as u64;
        let _ = tx.send(Message::VerifyProgress(
            0.5 + 0.5 * ((total - remaining) as f64 / total as f64) as f32,
        ));
    }

    Some(hasher.finalize().to_vec())
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    #[test]
    fn verify_identical_files() {
        let dir = std::env::temp_dir().join("flint_test_verify");
        let _ = std::fs::create_dir_all(&dir);

        let path = dir.join("test.img");
        let mut f = std::fs::File::create(&path).unwrap();
        let data = b"hello world this is test data for verification";
        f.write_all(data).unwrap();
        drop(f);

        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        super::verify_flash(
            path.to_str().unwrap(),
            path.to_str().unwrap(),
            data.len() as u64,
            cancel,
            tx,
        );

        let mut results = Vec::new();
        while let Ok(msg) = rx.recv() {
            results.push(msg);
        }

        let has_ok = results.iter().any(|m| matches!(m, super::Message::VerifyDone(true, _)));
        assert!(has_ok, "Verification should pass for identical files");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
