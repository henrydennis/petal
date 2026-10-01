//! First run: scan straight away, and help the user grant Full Disk Access so the
//! scan can include the folders macOS keeps private.

use std::path::PathBuf;

/// System Settings › Privacy & Security › Full Disk Access.
pub const FULL_DISK_ACCESS_SETTINGS: &str = "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";

/// Whether this process can read folders that need Full Disk Access. macOS's own
/// privacy database is always present and only readable with that access.
pub fn has_full_disk_access() -> bool {
    match std::fs::File::open("/Library/Application Support/com.apple.TCC/TCC.db") {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // Unusual system layout: fall back to another protected folder.
            std::env::var_os("HOME")
                .map(|home| std::fs::read_dir(PathBuf::from(home).join("Library/Safari")).is_ok())
                .unwrap_or(false)
        }
        Err(_) => false,
    }
}

/// Where Petal keeps its small bits of state. `PETAL_STATE_DIR` overrides it (for tests).
fn state_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("PETAL_STATE_DIR") {
        return Some(PathBuf::from(dir));
    }
    Some(PathBuf::from(std::env::var_os("HOME")?).join("Library/Application Support/Petal"))
}

fn first_run_marker() -> Option<PathBuf> {
    state_dir().map(|dir| dir.join("first-run-done"))
}

/// True until the first scan has been started.
pub fn is_first_run() -> bool {
    first_run_marker().is_some_and(|marker| !marker.exists())
}

pub fn mark_first_run_done() {
    if let Some(marker) = first_run_marker() {
        if let Some(dir) = marker.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(marker, b"");
    }
}
