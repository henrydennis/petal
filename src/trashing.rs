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
    let left = still_there(paths).map_err(|error| format!("{error} (Finder: {finder_error})"))?;
    if left.is_empty() {
        return Ok(());
    }
    let mut context = TrashContext::default();
    context.set_delete_method(DeleteMethod::NsFileManager);
    context.delete_all(left).map_err(|error| format!("{error} (Finder: {finder_error})"))
}

/// The paths that still exist. Only "not found" means gone: a path that can't be checked
/// (no permission, say) may well still be there, so that's an error.
fn still_there(paths: &[PathBuf]) -> Result<Vec<&PathBuf>, String> {
    let mut left = Vec::new();
    for path in paths {
        match path.symlink_metadata() {
            Ok(_) => left.push(path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("{}: {error}", path.display())),
        }
    }
    Ok(left)
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
    fn unreadable_paths_are_not_taken_for_gone() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("petal-still-there-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("locked")).unwrap();
        std::fs::write(dir.join("here"), b"x").unwrap();
        std::fs::write(dir.join("locked/inside"), b"x").unwrap();
        let (here, gone, inside) = (dir.join("here"), dir.join("gone"), dir.join("locked/inside"));

        assert_eq!(still_there(&[here.clone(), gone.clone()]).unwrap(), vec![&here]);
        std::fs::set_permissions(dir.join("locked"), std::fs::Permissions::from_mode(0o000)).unwrap();
        let result = still_there(&[here, gone, inside]).is_err();
        std::fs::set_permissions(dir.join("locked"), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(result, "a path behind a locked folder may still exist");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn script_quotes_paths() {
        let paths = [PathBuf::from("/a/plain"), PathBuf::from(r#"/b/say "hi"\now"#)];
        assert_eq!(
            finder_script(&paths),
            r#"tell application "Finder" to delete { POSIX file "/a/plain", POSIX file "/b/say \"hi\"\\now" }"#
        );
    }
}
