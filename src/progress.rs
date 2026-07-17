#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn parse_dd_progress(line: &str) -> Option<u64> {
    let line = line.trim();
    if let Some(end) = line.find(" bytes (") {
        line[..end].parse::<u64>().ok()
    } else if let Some(end) = line.find(" bytes transferred") {
        line[..end].parse::<u64>().ok()
    } else if let Some(end) = line.find(" bytes copied") {
        line[..end].parse::<u64>().ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn parse_dd_progress_modern() {
        let line = "12345678 bytes (12 MB, 12 MiB) copied, 0.5 s, 24 MB/s";
        assert_eq!(super::parse_dd_progress(line), Some(12345678));
    }

    #[test]
    fn parse_dd_progress_classic() {
        let line = "12345678 bytes transferred";
        assert_eq!(super::parse_dd_progress(line), Some(12345678));
    }

    #[test]
    fn parse_dd_progress_copied() {
        let line = "12345678 bytes copied";
        assert_eq!(super::parse_dd_progress(line), Some(12345678));
    }

    #[test]
    fn parse_dd_progress_empty() {
        assert_eq!(super::parse_dd_progress(""), None);
    }

    #[test]
    fn parse_dd_progress_non_match() {
        let line = "0+1 records in";
        assert_eq!(super::parse_dd_progress(line), None);
    }
}
