//! Findings: well-known places where disk space piles up, with exact sizes and a plain
//! explanation of whether they're safe to delete. The hotspot pass scans these first, so
//! their sizes are exact within seconds of starting a scan.

use std::path::{Path, PathBuf};

use crate::scan::{Kind, Tree};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Safety {
    /// Rebuilt or re-downloaded automatically when needed.
    Safe,
    /// Worth a look first: it may hold things you want to keep.
    Review,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Base {
    Home,
    /// The per-user temporary folder under /var/folders.
    UserTemp,
}

pub struct Category {
    pub base: Base,
    /// Location relative to `base`.
    pub path: &'static str,
    pub title: &'static str,
    pub blurb: &'static str,
    pub safety: Safety,
}

/// In hotspot order: most often large and most valuable first.
pub const CATALOG: &[Category] = &[
    Category { base: Base::Home, path: ".Trash", title: "Trash", blurb: "Emptying the Trash frees this space", safety: Safety::Safe },
    Category { base: Base::Home, path: "Downloads", title: "Downloads", blurb: "Old installers and archives tend to pile up here", safety: Safety::Review },
    Category { base: Base::Home, path: "Library/Developer/Xcode/DerivedData", title: "Xcode build files", blurb: "Xcode rebuilds these when you next build", safety: Safety::Safe },
    Category { base: Base::Home, path: "Library/Developer/Xcode/Archives", title: "Xcode archives", blurb: "Keep the ones you need to symbolicate crash logs", safety: Safety::Review },
    Category { base: Base::Home, path: "Library/Developer/Xcode/iOS DeviceSupport", title: "iOS device support", blurb: "Re-downloaded when a device next connects", safety: Safety::Safe },
    Category { base: Base::Home, path: "Library/Developer/CoreSimulator/Devices", title: "iOS simulators", blurb: "Remove old ones with `xcrun simctl delete unavailable`", safety: Safety::Review },
    Category { base: Base::Home, path: "Library/Application Support/MobileSync/Backup", title: "iPhone & iPad backups", blurb: "Old device backups; manage them in Finder", safety: Safety::Review },
    Category { base: Base::Home, path: "Library/Containers/com.docker.docker", title: "Docker", blurb: "Images and volumes; prune them from Docker", safety: Safety::Review },
    Category { base: Base::Home, path: "Library/Caches", title: "App caches", blurb: "Apps rebuild their caches as needed", safety: Safety::Safe },
    Category { base: Base::Home, path: ".npm", title: "npm cache", blurb: "Re-downloaded on the next install", safety: Safety::Safe },
    Category { base: Base::Home, path: ".cargo/registry", title: "Cargo registry", blurb: "Re-downloaded on the next build", safety: Safety::Safe },
    Category { base: Base::Home, path: ".gradle/caches", title: "Gradle caches", blurb: "Re-downloaded on the next build", safety: Safety::Safe },
    Category { base: Base::Home, path: "Movies", title: "Movies", blurb: "Videos and editing projects", safety: Safety::Review },
    Category {
        base: Base::UserTemp,
        path: "X/com.google.Chrome.code_sign_clone",
        title: "Chrome update leftovers",
        blurb: "Old copies Chrome keeps while updating; quit Chrome first",
        safety: Safety::Safe,
    },
    Category { base: Base::Home, path: "Library/Mail", title: "Mail", blurb: "Messages and downloaded attachments", safety: Safety::Review },
];

/// Findings smaller than this aren't worth anyone's attention.
pub const MIN_SIZE: u64 = 50_000_000;

#[derive(Clone, Debug)]
pub struct Finding {
    pub title: &'static str,
    pub blurb: String,
    pub safety: Safety,
    pub size: u64,
    /// The folder, for a single-location finding.
    pub path: Option<PathBuf>,
    /// Every folder involved (one for a location, many for e.g. node_modules).
    pub nodes: Vec<usize>,
    /// `size` is still the allocated size; what deleting frees is being worked out in
    /// the background (`scan::frees_of`).
    pub pending: bool,
}

/// A hotspot's result as reported mid-scan.
#[derive(Clone, Debug)]
pub struct Early {
    pub category: usize,
    pub path: PathBuf,
    pub size: u64,
    /// Milliseconds after the scan started.
    pub at_ms: u64,
}

pub fn early_findings(early: &[Early]) -> Vec<Finding> {
    let mut findings: Vec<Finding> = early
        .iter()
        .filter(|e| e.size >= MIN_SIZE)
        .map(|e| {
            let category = &CATALOG[e.category];
            Finding {
                title: category.title,
                blurb: category.blurb.to_string(),
                safety: category.safety,
                size: e.size,
                path: Some(e.path.clone()),
                nodes: Vec::new(),
                pending: false,
            }
        })
        .collect();
    findings.sort_by(|a, b| b.size.cmp(&a.size));
    findings
}

/// Findings from a finished scan: the catalog locations, plus every `node_modules`.
/// Sizes start as allocated size, all `pending`: what deleting really frees depends on
/// APFS clone sharing, which the caller works out in the background (`scan::frees_of`).
pub fn from_tree(tree: &Tree, bases: &Bases) -> Vec<Finding> {
    from_tree_min(tree, bases, MIN_SIZE)
}

fn from_tree_min(tree: &Tree, bases: &Bases, min_size: u64) -> Vec<Finding> {
    let mut findings = Vec::new();
    {
        for category in CATALOG {
            let Some(path) = bases.locate(category) else { continue };
            let Some(ix) = tree.find(&path) else { continue };
            let size = tree.nodes[ix].size;
            if size >= min_size {
                findings.push(Finding {
                    title: category.title,
                    blurb: category.blurb.to_string(),
                    safety: category.safety,
                    size,
                    path: Some(path),
                    nodes: vec![ix],
                    pending: true,
                });
            }
        }
    }

    // node_modules anywhere, counting nested ones once.
    let mut modules = Vec::new();
    let mut stack = vec![Tree::ROOT];
    while let Some(ix) = stack.pop() {
        for &child in &tree.nodes[ix].children {
            let node = &tree.nodes[child];
            if node.kind != Kind::Dir {
                continue;
            }
            if node.name.as_ref() == "node_modules" {
                modules.push(child);
            } else {
                stack.push(child);
            }
        }
    }
    // Clone sharing isn't known here (pnpm installs them as clones), so start from the
    // allocated size and let the caller work out what deleting really frees.
    let size: u64 = modules.iter().map(|&ix| tree.nodes[ix].size).sum();
    if size >= min_size {
        findings.push(Finding {
            title: "node_modules",
            blurb: format!("In {} projects; reinstall with your package manager", modules.len()),
            safety: Safety::Safe,
            size,
            path: None,
            nodes: modules,
            pending: true,
        });
    }

    findings.sort_by(|a, b| b.size.cmp(&a.size));
    findings
}

/// Where the catalog's base folders are, as paths inside a scanned tree.
#[derive(Clone, Debug, Default)]
pub struct Bases {
    pub home: Option<PathBuf>,
    pub user_temp: Option<PathBuf>,
}

impl Bases {
    /// The real locations, as they appear under `tree_root`: as-is, or under the Data
    /// volume when scanning the startup disk.
    pub fn for_root(tree_root: &Path) -> Self {
        let within = |path: PathBuf| -> Option<PathBuf> {
            if path.starts_with(tree_root) {
                return Some(path);
            }
            let mapped = tree_root.join(path.strip_prefix("/").ok()?);
            mapped.exists().then_some(mapped)
        };
        Bases {
            home: std::env::var_os("HOME").map(PathBuf::from).and_then(within),
            user_temp: user_temp_dir().and_then(within),
        }
    }

    pub fn locate(&self, category: &Category) -> Option<PathBuf> {
        let base = match category.base {
            Base::Home => self.home.as_ref(),
            Base::UserTemp => self.user_temp.as_ref(),
        }?;
        Some(base.join(category.path))
    }
}

/// e.g. /private/var/folders/xy/abc123…0000gn: the parent of DARWIN_USER_DIR.
fn user_temp_dir() -> Option<PathBuf> {
    let mut buf = vec![0 as libc::c_char; 1024];
    let len = unsafe { libc::confstr(libc::_CS_DARWIN_USER_DIR, buf.as_mut_ptr(), buf.len()) };
    if len == 0 || len > buf.len() {
        return None;
    }
    let dir = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned();
    std::fs::canonicalize(dir).ok()?.parent().map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::{Progress, scan};

    fn write(path: &Path, bytes: usize) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![1u8; bytes]).unwrap();
    }

    #[test]
    fn finds_catalog_locations_and_node_modules_once() {
        let dir = std::env::temp_dir().join(format!("petal-findings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let home = dir.join("home");
        write(&home.join(".Trash/old.dmg"), 300_000);
        write(&home.join("Downloads/setup.pkg"), 200_000);
        write(&home.join("code/app/node_modules/a/index.js"), 100_000);
        // Nested node_modules must not be counted twice.
        write(&home.join("code/app/node_modules/b/node_modules/c/index.js"), 50_000);
        write(&home.join("code/lib/node_modules/d/index.js"), 70_000);

        let tree = scan(&dir, &Progress::default());
        let findings = from_tree_min(&tree, &Bases { home: Some(home.clone()), user_temp: None }, 0);
        let by_title = |title: &str| findings.iter().find(|f| f.title == title);

        let trash = by_title("Trash").expect("trash");
        assert_eq!(trash.size, tree.nodes[tree.find(&home.join(".Trash")).unwrap()].size);
        assert!(by_title("Downloads").is_some());

        let modules = by_title("node_modules").expect("node_modules");
        assert_eq!(modules.nodes.len(), 2, "nested node_modules counted once");
        let expected: u64 = ["code/app/node_modules", "code/lib/node_modules"]
            .iter()
            .map(|p| tree.nodes[tree.find(&home.join(p)).unwrap()].size)
            .sum();
        assert_eq!(modules.size, expected);
        // Largest first.
        assert!(findings.windows(2).all(|w| w[0].size >= w[1].size));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
