//! Moving items to the Trash.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use trash::TrashContext;
use trash::macos::{DeleteMethod, TrashContextExtMacos};

/// Move `paths` to the Trash through Finder, so "Put Back" works, falling back to
/// NSFileManager when Finder can't do it (no Automation permission, say).
///
/// Not `trash::delete_all`: that starts `osascript` with a `PATH` search, and an app opened from
/// Finder or the Dock gets launchd's `PATH`, which can be anything. One entry longer than
/// `PATH_MAX` makes the search fail with "File name too long (os error 63)".
pub fn move_to_trash(paths: &[PathBuf]) -> Result<(), String> {
    let finder_error = match via_finder(paths) {
        Ok(()) => return Ok(()),
        Err(error) => error,
    };
    // Finder may have moved some of them before it failed.
    let left: Vec<&PathBuf> = paths.iter().filter(|p| p.symlink_metadata().is_ok()).collect();
    if left.is_empty() {
        return Ok(());
    }
    let mut context = TrashContext::default();
    context.set_delete_method(DeleteMethod::NsFileManager);
    context.delete_all(left).map_err(|error| format!("{error} (Finder: {finder_error})"))
}

fn via_finder(paths: &[PathBuf]) -> Result<(), String> {
    // The script goes in on stdin rather than as an argument, so no selection is too big.
    let mut child = Command::new("/usr/bin/osascript")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;
    let written = child.stdin.take().expect("piped stdin").write_all(finder_script(paths).as_bytes());
    let output = child.wait_with_output().map_err(|error| error.to_string())?;
    written.map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

fn finder_script(paths: &[PathBuf]) -> String {
    let files: Vec<String> = paths.iter().map(|p| format!("POSIX file \"{}\"", applescript_escape(p))).collect();
    format!("tell application \"Finder\" to delete {{ {} }}", files.join(", "))
}

fn applescript_escape(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_quotes_paths() {
        let paths = [PathBuf::from("/a/plain"), PathBuf::from(r#"/b/say "hi"\now"#)];
        assert_eq!(
            finder_script(&paths),
            r#"tell application "Finder" to delete { POSIX file "/a/plain", POSIX file "/b/say \"hi\"\\now" }"#
        );
    }
}
