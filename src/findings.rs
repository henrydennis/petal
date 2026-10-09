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
    /// Belongs to an app that manages it (a disk image, your conversations): shown and
    /// explained, but never offered for the Trash. Its `fix` is never `Fix::Trash`.
    ManageInApp,
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
    pub fix: Fix,
    /// Its files are often hard links or clones of files elsewhere (pnpm's store and the
    /// projects installed from it), so its size says little about what deleting it frees
    /// until `scan::frees_of` has worked that out.
    pub shared: bool,
}

/// In hotspot order: most often large and most valuable first.
pub const CATALOG: &[Category] = &[
    Category { base: Base::Home, path: ".Trash", title: "Trash", blurb: "Emptying the Trash frees this space", safety: Safety::Safe, fix: Fix::Trash, shared: false },
    Category { base: Base::Home, path: "Downloads", title: "Downloads", blurb: "Old installers and archives tend to pile up here", safety: Safety::Review, fix: Fix::Trash, shared: false },
    Category { base: Base::Home, path: "Library/Developer/Xcode/DerivedData", title: "Xcode build files", blurb: "Xcode rebuilds these when you next build", safety: Safety::Safe, fix: Fix::Trash, shared: false },
    Category { base: Base::Home, path: "Library/Developer/Xcode/Archives", title: "Xcode archives", blurb: "Keep the ones you need to symbolicate crash logs", safety: Safety::Review, fix: Fix::Trash, shared: false },
    Category { base: Base::Home, path: "Library/Developer/Xcode/iOS DeviceSupport", title: "iOS device support", blurb: "Re-downloaded when a device next connects", safety: Safety::Safe, fix: Fix::Trash, shared: false },
    Category { base: Base::Home, path: "Library/Developer/CoreSimulator/Devices", title: "iOS simulators", blurb: "Remove old ones with `xcrun simctl delete unavailable`", safety: Safety::Review, fix: Fix::Trash, shared: false },
    Category { base: Base::Home, path: "Library/Application Support/MobileSync/Backup", title: "iPhone & iPad backups", blurb: "Old device backups; manage them in Finder", safety: Safety::Review, fix: Fix::Trash, shared: false },
    // Docker keeps everything in one sparse disk image, so it's counted at the space it
    // takes on disk, not its (much larger) apparent size. Deleting it wipes every image,
    // container and volume, so Docker is the place to shrink it.
    Category {
        base: Base::Home,
        path: "Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw",
        title: "Docker disk image",
        blurb: "Images, containers and volumes; `docker system prune` or Docker's settings shrink it",
        safety: Safety::ManageInApp,
        fix: Fix::Command("docker system prune"),
        shared: false,
    },
    // Local AI models: gigabytes each, and slow to download again.
    Category { base: Base::Home, path: ".ollama/models", title: "Ollama models", blurb: "Downloaded models you'd download again; remove one with `ollama rm <model>`", safety: Safety::Review, fix: Fix::Trash, shared: false },
    Category { base: Base::Home, path: ".lmstudio/models", title: "LM Studio models", blurb: "Downloaded models you'd download again; remove them in LM Studio", safety: Safety::Review, fix: Fix::Trash, shared: false },
    Category { base: Base::Home, path: ".cache/lm-studio/models", title: "LM Studio models", blurb: "Downloaded models you'd download again; remove them in LM Studio", safety: Safety::Review, fix: Fix::Trash, shared: false },
    // Only the hub: the folder above it also holds the Hugging Face login token.
    Category { base: Base::Home, path: ".cache/huggingface/hub", title: "Hugging Face models", blurb: "Downloaded models you'd download again; `hf cache` commands remove chosen ones", safety: Safety::Review, fix: Fix::Trash, shared: false },
    Category { base: Base::Home, path: "Library/Caches", title: "App caches", blurb: "Apps rebuild their caches as needed", safety: Safety::Safe, fix: Fix::Trash, shared: false },
    // Inside App caches (so never a hotspot of its own), but Homebrew has its own way to clear it.
    Category {
        base: Base::Home,
        path: "Library/Caches/Homebrew",
        title: "Homebrew downloads",
        blurb: "Fetched again when needed; `brew cleanup --prune=all` clears them",
        safety: Safety::Safe,
        fix: Fix::Command("brew cleanup --prune=all"),
        shared: false,
    },
    Category { base: Base::Home, path: ".npm", title: "npm cache", blurb: "Re-downloaded on the next install", safety: Safety::Safe, fix: Fix::Trash, shared: false },
    // pnpm installs packages into projects as hard links (or clones) of the files in its
    // store, so deleting the store frees only what no project uses.
    Category { base: Base::Home, path: "Library/pnpm/store", title: "pnpm store", blurb: PNPM_BLURB, safety: Safety::Review, fix: Fix::Command("pnpm store prune"), shared: true },
    Category { base: Base::Home, path: ".local/share/pnpm/store", title: "pnpm store", blurb: PNPM_BLURB, safety: Safety::Review, fix: Fix::Command("pnpm store prune"), shared: true },
    Category { base: Base::Home, path: ".pnpm-store", title: "pnpm store", blurb: PNPM_BLURB, safety: Safety::Review, fix: Fix::Command("pnpm store prune"), shared: true },
    Category { base: Base::Home, path: ".cargo/registry", title: "Cargo registry", blurb: "Re-downloaded on the next build", safety: Safety::Safe, fix: Fix::Trash, shared: false },
    Category { base: Base::Home, path: ".gradle/caches", title: "Gradle caches", blurb: "Re-downloaded on the next build", safety: Safety::Safe, fix: Fix::Trash, shared: false },
    Category { base: Base::Home, path: "Movies", title: "Movies", blurb: "Videos and editing projects", safety: Safety::Review, fix: Fix::Trash, shared: false },
    Category {
        base: Base::UserTemp,
        path: "X/com.google.Chrome.code_sign_clone",
        title: "Chrome update leftovers",
        blurb: "Old copies Chrome keeps while updating; quit Chrome first",
        safety: Safety::Safe,
        fix: Fix::Trash,
        shared: false,
    },
    Category { base: Base::Home, path: "Library/Mail", title: "Mail", blurb: "Messages and downloaded attachments", safety: Safety::Review, fix: Fix::Trash, shared: false },
    // Coding agents' conversation histories. Small, mostly, but they're the user's own.
    Category { base: Base::Home, path: ".claude/projects", title: "Claude Code history", blurb: "Your conversations, kept so you can resume them; Claude Code clears old ones itself", safety: Safety::ManageInApp, fix: Fix::InApp, shared: false },
    Category { base: Base::Home, path: ".codex/sessions", title: "Codex history", blurb: "Your Codex conversations, kept so you can resume them", safety: Safety::ManageInApp, fix: Fix::InApp, shared: false },
];

const PNPM_BLURB: &str = "Counts only files no project links to; `pnpm store prune` removes unused packages";

/// Findings smaller than this aren't worth anyone's attention.
pub const MIN_SIZE: u64 = 50_000_000;

/// What to do about a finding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fix {
    /// Collect its folders and move them to the Trash.
    Trash,
    /// Run `git gc` in each repository. Its folders are `.git` folders, which must never
    /// be collected: that would throw away the repository's history.
    GitGc,
    /// Run this command, which the owning tool provides for clearing the folder safely.
    /// The folder itself isn't offered for the Trash.
    Command(&'static str),
    /// Manage it in the app it belongs to; nothing to collect or run.
    InApp,
}

#[derive(Clone, Debug)]
pub struct Finding {
    pub title: &'static str,
    pub blurb: String,
    pub safety: Safety,
    pub size: u64,
    /// Allocated size of its folders when it was built; what `size` was worked out from.
    pub allocated: u64,
    /// The folder, for a single-location finding.
    pub path: Option<PathBuf>,
    /// Every folder involved (one for a location, many for e.g. node_modules).
    pub nodes: Vec<usize>,
    /// `size` is still the allocated size; what deleting frees is being worked out in
    /// the background (`scan::frees_of`).
    pub pending: bool,
    pub fix: Fix,
}

impl Finding {
    /// Whether its folders can go in the Collector (and from there to the Trash).
    pub fn collectable(&self) -> bool {
        self.fix == Fix::Trash && self.safety != Safety::ManageInApp
    }
}

/// How much the Safe findings hold, counting a finding inside another one (Homebrew's
/// downloads inside App caches) once.
pub fn safe_total(findings: &[Finding]) -> u64 {
    let safe: Vec<&Finding> = findings.iter().filter(|f| f.safety == Safety::Safe).collect();
    let nested = |f: &Finding| {
        let Some(path) = &f.path else { return false };
        safe.iter().any(|other| other.path.as_ref().is_some_and(|o| o != path && path.starts_with(o)))
    };
    safe.iter().filter(|f| !nested(**f)).map(|f| f.size).sum()
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
                allocated: e.size,
                path: Some(e.path.clone()),
                nodes: Vec::new(),
                // The hotspot pass reads it before the projects linking to it, so it was
                // given the shared files' space.
                pending: category.shared,
                fix: category.fix,
            }
        })
        .collect();
    findings.sort_by(|a, b| b.size.cmp(&a.size));
    findings
}

/// Folder extensions that make a macOS bundle: an app, or a part of one. What's inside
/// belongs to that app (Electron apps ship their own `node_modules`), so no finding may
/// include it, whatever its name.
const BUNDLE_EXTENSIONS: &[&str] = &[
    "app", "appex", "framework", "bundle", "plugin", "xpc", "kext", "systemextension", "dext", "qlgenerator",
    "mdimporter", "prefpane", "saver", "component", "vst", "vst3", "aaxplugin", "driver", "xcarchive",
    "photoslibrary", "musiclibrary", "fcpbundle", "pkg", "mpkg",
];

fn is_bundle(name: &str) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(stem, ext)| !stem.is_empty() && BUNDLE_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
}

/// Whether `ix`, or any folder above it in the tree, is a bundle.
fn inside_bundle(tree: &Tree, mut ix: usize) -> bool {
    loop {
        if is_bundle(tree.nodes[ix].name.as_ref()) {
            return true;
        }
        match tree.nodes[ix].parent {
            Some(parent) => ix = parent,
            None => return tree.root_path.components().any(|c| is_bundle(&c.as_os_str().to_string_lossy())),
        }
    }
}

/// Folders whose `node_modules` belong to something other than a project you can
/// reinstall: hidden folders (editor extensions in ~/.vscode, Node versions in ~/.nvm,
/// global installs), `Library` (app-managed data such as extensions in Application
/// Support) and `Applications`, besides bundles.
fn not_a_project_area(name: &str) -> bool {
    name.starts_with('.') || name == "Library" || name == "Applications" || is_bundle(name)
}

/// Findings from a finished scan: the catalog locations, plus the `node_modules` of every
/// project, the `target` folder of every Cargo project and every Git repository with a lot
/// of loose objects.
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
            if inside_bundle(tree, ix) {
                continue;
            }
            let size = tree.nodes[ix].size;
            if size >= min_size {
                findings.push(Finding {
                    title: category.title,
                    blurb: category.blurb.to_string(),
                    safety: category.safety,
                    size,
                    allocated: size,
                    path: Some(path),
                    nodes: vec![ix],
                    pending: true,
                    fix: category.fix,
                });
            }
        }
    }

    // Projects' node_modules, counting nested ones once; and Git repositories. Apps and
    // tools keep node_modules too (Electron apps in their bundles, editors' extensions
    // and CLIs under ~/Library and dot-folders); deleting those breaks them, and no
    // package manager puts them back, so the walk stays out of all of those, and out of
    // where apps and the system live.
    let off_limits = |path: &Path| bases.protected.iter().any(|p| path.starts_with(p));
    let mut modules = Vec::new();
    let mut targets = Vec::new();
    let mut repos = Vec::new();
    // Where the scan really is: it may have started from a symlink (into an app, say).
    let real_root = std::fs::canonicalize(&tree.root_path).unwrap_or_else(|_| tree.root_path.clone());
    let root_excluded = real_root.components().any(|c| not_a_project_area(&c.as_os_str().to_string_lossy()))
        || off_limits(&tree.root_path);
    let mut stack = if root_excluded { Vec::new() } else { vec![Tree::ROOT] };
    while let Some(ix) = stack.pop() {
        for &child in &tree.nodes[ix].children {
            let node = &tree.nodes[child];
            if node.kind != Kind::Dir {
                continue;
            }
            match node.name.as_ref() {
                "node_modules" => {
                    if is_project(tree, ix) {
                        modules.push(child);
                    }
                }
                // Plenty of things are called `target`; only one right next to a
                // Cargo.toml is Cargo's build output. Any other is walked like any folder.
                "target" if is_cargo_project(tree, ix) => targets.push(child),
                ".git" => {
                    let loose = loose_objects_size(tree, child);
                    if loose > 0 && loose >= min_size / 10 {
                        repos.push((child, loose));
                    }
                }
                name => {
                    if !(not_a_project_area(name) || (name == "System" && off_limits(&tree.path_of(child)))) {
                        stack.push(child);
                    }
                }
            }
        }
    }
    // Clone sharing isn't known here (pnpm installs them as clones), so start from the
    // allocated size and let the caller work out what deleting really frees.
    let size: u64 = modules.iter().map(|&ix| tree.nodes[ix].size).sum();
    if !modules.is_empty() && size >= min_size {
        findings.push(Finding {
            title: "node_modules",
            blurb: format!("In {} projects; reinstall with your package manager", modules.len()),
            safety: Safety::Safe,
            size,
            allocated: size,
            path: None,
            nodes: modules,
            pending: true,
            fix: Fix::Trash,
        });
    }

    // Cargo's build output: `cargo build` makes it again (and `cargo clean` deletes it).
    // Like node_modules, it can share data with clones, so savings are worked out later.
    let size: u64 = targets.iter().map(|&ix| tree.nodes[ix].size).sum();
    if !targets.is_empty() && size >= min_size {
        let count = targets.len();
        findings.push(Finding {
            title: "Cargo build files",
            blurb: format!(
                "In {count} Rust {}; `cargo build` rebuilds them, or run `cargo clean`",
                if count == 1 { "project" } else { "projects" }
            ),
            safety: Safety::Safe,
            size,
            allocated: size,
            path: None,
            nodes: targets,
            pending: true,
            fix: Fix::Trash,
        });
    }

    // Each `git add` of a changed file stores a whole new compressed copy as a loose
    // object, and Git only packs them (or drops the unused ones) once there are thousands
    // of them, so repositories of big data files can grow by tens of GB.
    repos.sort_by(|a, b| b.1.cmp(&a.1));
    let size: u64 = repos.iter().map(|&(_, loose)| loose).sum();
    if !repos.is_empty() && size >= min_size {
        let count = repos.len();
        findings.push(Finding {
            title: "Unpacked Git data",
            blurb: format!(
                "In {count} {}; `git gc` packs it",
                if count == 1 { "repo" } else { "repos" }
            ),
            safety: Safety::Review,
            size,
            allocated: size,
            path: Some(tree.path_of(repos[0].0)),
            nodes: repos.into_iter().map(|(ix, _)| ix).collect(),
            pending: false,
            fix: Fix::GitGc,
        });
    }

    findings.sort_by(|a, b| b.size.cmp(&a.size));
    findings
}

/// Lockfiles: with one next to `package.json`, reinstalling brings back exactly the same
/// `node_modules`.
const LOCKFILES: &[&str] =
    &["package-lock.json", "npm-shrinkwrap.json", "yarn.lock", "pnpm-lock.yaml", "bun.lock", "bun.lockb"];

/// Whether folder `ix` is a JavaScript project of the user's whose `node_modules` a
/// package manager can reinstall: it has a `package.json` and a lockfile, and the user
/// owns it (not an app's or another user's).
fn is_project(tree: &Tree, ix: usize) -> bool {
    has_file(tree, ix, &["package.json"]) && has_file(tree, ix, LOCKFILES) && owned_by_user(tree, ix)
}

/// Whether folder `ix` is a Rust project of the user's, whose `target` folder is Cargo's
/// build output: it has a `Cargo.toml`, and the user owns it.
fn is_cargo_project(tree: &Tree, ix: usize) -> bool {
    has_file(tree, ix, &["Cargo.toml"]) && owned_by_user(tree, ix)
}

/// Whether folder `ix` directly holds a file with one of these names.
fn has_file(tree: &Tree, ix: usize, names: &[&str]) -> bool {
    tree.nodes[ix].children.iter().any(|&c| tree.nodes[c].kind == Kind::File && names.contains(&tree.nodes[c].name.as_ref()))
}

/// Whether the user owns folder `ix` (not an app or another user).
fn owned_by_user(tree: &Tree, ix: usize) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(tree.path_of(ix)).is_ok_and(|m| m.uid() == unsafe { libc::getuid() })
}

/// Whether `ix` is inside a bundle (not the bundle itself, which is fine to trash whole).
pub fn is_inside_bundle(tree: &Tree, ix: usize) -> bool {
    let mut at = tree.nodes[ix].parent;
    while let Some(parent) = at {
        if is_bundle(&tree.nodes[parent].name) {
            return true;
        }
        at = tree.nodes[parent].parent;
    }
    false
}

/// How much of a `.git` folder is loose objects (`objects/00` to `objects/ff`): each one
/// compressed on its own, unlike the packs `git gc` makes.
pub fn loose_objects_size(tree: &Tree, git: usize) -> u64 {
    let Some(&objects) = tree.nodes[git].children.iter().find(|&&c| tree.nodes[c].name.as_ref() == "objects") else {
        return 0;
    };
    tree.nodes[objects]
        .children
        .iter()
        .filter(|&&c| {
            let name = tree.nodes[c].name.as_bytes();
            name.len() == 2 && name.iter().all(u8::is_ascii_hexdigit)
        })
        .map(|&c| tree.nodes[c].size)
        .sum()
}

/// Shell commands that run `git gc` in the repositories of these `.git` folders, one
/// per line.
pub fn git_gc_commands(git_dirs: &[PathBuf]) -> String {
    git_dirs
        .iter()
        .filter_map(|git| git.parent())
        .map(|repo| format!("git -C '{}' gc", repo.to_string_lossy().replace('\'', r"'\''")))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Where the catalog's base folders are, as paths inside a scanned tree.
#[derive(Clone, Debug, Default)]
pub struct Bases {
    pub home: Option<PathBuf>,
    pub user_temp: Option<PathBuf>,
    /// Where apps, the system and tools live; nothing under them is ever a finding.
    pub protected: Vec<PathBuf>,
}

/// Apps and the system: never searched for node_modules or anything else to delete.
/// (So are ~/Applications and ~/Library, where apps and tools keep their files.)
const PROTECTED: &[&str] = &["/Applications", "/System", "/Library"];

/// The startup disk's Data volume, which `/` shows (through firmlinks) when scanning it.
const DATA_VOLUME: &str = "/System/Volumes/Data";

impl Bases {
    /// The real locations, as they appear under `tree_root`: as-is, or under the Data
    /// volume when scanning the startup disk.
    pub fn for_root(tree_root: &Path) -> Self {
        Self::for_root_with_home(tree_root, std::env::var_os("HOME").map(PathBuf::from))
    }

    fn for_root_with_home(tree_root: &Path, home: Option<PathBuf>) -> Self {
        let within = |path: PathBuf| -> Option<PathBuf> {
            if path.starts_with(tree_root) {
                return Some(path);
            }
            let mapped = tree_root.join(path.strip_prefix("/").ok()?);
            mapped.exists().then_some(mapped)
        };
        let protected = PROTECTED
            .iter()
            .map(PathBuf::from)
            .chain(home.iter().flat_map(|home| [home.join("Applications"), home.join("Library")]))
            .filter_map(|path| protected_in_tree(tree_root, path, within))
            .collect();
        Bases { home: home.and_then(within), user_temp: user_temp_dir().and_then(within), protected }
    }

    pub fn locate(&self, category: &Category) -> Option<PathBuf> {
        let base = match category.base {
            Base::Home => self.home.as_ref(),
            Base::UserTemp => self.user_temp.as_ref(),
        }?;
        Some(base.join(category.path))
    }
}

/// A protected location as it appears in the tree at `tree_root`: the whole tree when the
/// scan is inside it (e.g. a scan of ~/Library/Application Support), else wherever `within`
/// finds it. The startup disk's Data volume counts as `/`, not as part of /System.
fn protected_in_tree(tree_root: &Path, path: PathBuf, within: impl Fn(PathBuf) -> Option<PathBuf>) -> Option<PathBuf> {
    // Compare where both really are: a scan can start from a symlink to ~/Library, say.
    let real = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let real_root = real(tree_root);
    let as_shown = match real_root.strip_prefix(DATA_VOLUME) {
        Ok(rest) => Path::new("/").join(rest),
        Err(_) => real_root,
    };
    if as_shown.starts_with(real(&path)) { Some(tree_root.to_path_buf()) } else { within(path) }
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
        write(&home.join("code/app/package.json"), 100);
        write(&home.join("code/app/package-lock.json"), 100);
        write(&home.join("code/app/node_modules/a/index.js"), 100_000);
        // Nested node_modules must not be counted twice.
        write(&home.join("code/app/node_modules/b/node_modules/c/index.js"), 50_000);
        write(&home.join("code/lib/package.json"), 100);
        write(&home.join("code/lib/pnpm-lock.yaml"), 100);
        write(&home.join("code/lib/node_modules/d/index.js"), 70_000);
        // Not projects anyone can reinstall: an Electron app's, an editor extension's, a
        // CLI's in ~/Library, and one without a lockfile.
        for app in ["Applications/Editor.app/Contents/Resources/app", "Applications/Chat.app/Contents/Resources/app.asar.unpacked"] {
            write(&home.join(app).join("package.json"), 100);
            write(&home.join(app).join("yarn.lock"), 100);
            write(&home.join(app).join("node_modules/e/index.js"), 90_000);
        }
        write(&home.join(".vscode/extensions/ext/package.json"), 100);
        write(&home.join(".vscode/extensions/ext/package-lock.json"), 100);
        write(&home.join(".vscode/extensions/ext/node_modules/f/index.js"), 90_000);
        write(&home.join("Library/Application Support/Tool/package.json"), 100);
        write(&home.join("Library/Application Support/Tool/bun.lock"), 100);
        write(&home.join("Library/Application Support/Tool/node_modules/g/index.js"), 90_000);
        write(&home.join("code/unlocked/package.json"), 100);
        write(&home.join("code/unlocked/node_modules/h/index.js"), 90_000);
        // A protected location (standing in for /Library), even with a proper project.
        write(&dir.join("Library/Tool/package.json"), 100);
        write(&dir.join("Library/Tool/package-lock.json"), 100);
        write(&dir.join("Library/Tool/node_modules/i/index.js"), 90_000);

        let tree = scan(&dir, &Progress::default());
        let findings = from_tree_min(&tree, &Bases { home: Some(home.clone()), user_temp: None, protected: vec![dir.join("Library"), home.join("Library")] }, 0);
        let by_title = |title: &str| findings.iter().find(|f| f.title == title);

        let trash = by_title("Trash").expect("trash");
        assert_eq!(trash.size, tree.nodes[tree.find(&home.join(".Trash")).unwrap()].size);
        assert!(by_title("Downloads").is_some());

        let modules = by_title("node_modules").expect("node_modules");
        assert_eq!(modules.nodes.len(), 2, "only projects' node_modules, nested ones counted once");
        let expected: u64 = ["code/app/node_modules", "code/lib/node_modules"]
            .iter()
            .map(|p| tree.nodes[tree.find(&home.join(p)).unwrap()].size)
            .sum();
        assert_eq!(modules.size, expected);
        // Largest first.
        assert!(findings.windows(2).all(|w| w[0].size >= w[1].size));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Only a project's node_modules is safe to delete. Apps (installed or built locally)
    /// ship their own, as do editor extensions, global installs and app-managed data;
    /// deleting those breaks the software, as one user found with Cursor and T3 Code.
    #[test]
    fn node_modules_only_in_projects() {
        let dir = std::env::temp_dir().join(format!("petal-modules-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let home = dir.join("home");
        let project = |path: &str| {
            write(&home.join(path).join("package.json"), 100);
            write(&home.join(path).join("package-lock.json"), 100);
            write(&home.join(path).join("node_modules/x/index.js"), 10_000);
        };
        project("code/site");
        project("code/monorepo/packages/web");
        // Not projects you can reinstall:
        project("Applications/Cursor.app/Contents/Resources/app");
        project("code/t3code/release/mac-arm64/T3 Code.app/Contents/Resources/app");
        project("code/t3code/release/mac-arm64/T3 Code.app/Contents/Resources/app.asar.unpacked");
        project("code/tool/Helper.framework/Resources");
        project(".vscode/extensions/someone.ext-1.2.3");
        project(".cursor/extensions/someone.ext-1.2.3");
        project("Library/Application Support/Claude/Claude Extensions/server");
        project(".config/raycast/extensions/abc");
        write(&home.join(".nvm/versions/node/v22.0.0/lib/node_modules/npm/index.js"), 10_000);
        write(&home.join("code/no-package-json/node_modules/x/index.js"), 10_000);

        let tree = scan(&dir, &Progress::default());
        let findings = from_tree_min(&tree, &Bases { home: Some(home.clone()), ..Bases::default() }, 0);
        let modules = findings.iter().find(|f| f.title == "node_modules").expect("node_modules");
        let mut found: Vec<PathBuf> = modules.nodes.iter().map(|&ix| tree.path_of(ix)).collect();
        found.sort();
        assert_eq!(found, [home.join("code/monorepo/packages/web/node_modules"), home.join("code/site/node_modules")]);
        assert_eq!(modules.blurb, "In 2 projects; reinstall with your package manager");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// No finding may include anything inside an app or other bundle: not node_modules,
    /// and not any catalog location, now or added later.
    #[test]
    fn nothing_inside_bundles() {
        for category in CATALOG {
            assert!(!category.path.split('/').any(is_bundle), "{} runs through a bundle", category.path);
        }
        assert!(is_bundle("Cursor.app") && is_bundle("T3 Code.app") && is_bundle("Electron Framework.framework"));
        assert!(!is_bundle("node_modules") && !is_bundle(".app") && !is_bundle("app") && !is_bundle("my.config"));

        // A scan whose root is inside an app finds nothing to delete there either.
        let dir = std::env::temp_dir().join(format!("petal-bundle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let app = dir.join("Some.app/Contents/Resources/app");
        write(&app.join("package.json"), 100);
        write(&app.join("node_modules/x/index.js"), 10_000);
        let tree = scan(&app, &Progress::default());
        let findings = from_tree_min(&tree, &Bases::default(), 0);
        assert!(findings.iter().all(|f| f.title != "node_modules"), "{findings:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn finds_loose_git_objects_but_not_packs() {
        let dir = std::env::temp_dir().join(format!("petal-git-findings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write(&dir.join("big/.git/objects/3a/1111"), 400_000);
        write(&dir.join("big/.git/objects/f0/2222"), 300_000);
        write(&dir.join("big/.git/objects/pack/pack-1.pack"), 900_000);
        write(&dir.join("packed/.git/objects/pack/pack-2.pack"), 900_000);
        write(&dir.join("small/.git/objects/ab/3333"), 100_000);

        let tree = scan(&dir, &Progress::default());
        let findings = from_tree_min(&tree, &Bases::default(), 0);
        let git = findings.iter().find(|f| f.fix == Fix::GitGc).expect("git finding");
        let big = tree.find(&dir.join("big/.git")).unwrap();
        let small = tree.find(&dir.join("small/.git")).unwrap();
        assert_eq!(git.nodes, vec![big, small], "largest first, packed repository left out");
        assert_eq!(git.size, loose_objects_size(&tree, big) + loose_objects_size(&tree, small));
        assert!(loose_objects_size(&tree, big) < tree.nodes[big].size, "packs don't count");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn knows_what_is_inside_a_bundle() {
        let dir = std::env::temp_dir().join(format!("petal-bundles-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write(&dir.join("Chat.app/Contents/Resources/app.asar"), 1000);
        write(&dir.join("code/Chat.app.md"), 1000);

        let tree = scan(&dir, &Progress::default());
        let ix = |p: &str| tree.find(&dir.join(p)).unwrap();
        assert!(!is_inside_bundle(&tree, ix("Chat.app")), "the whole app can go");
        assert!(is_inside_bundle(&tree, ix("Chat.app/Contents/Resources")));
        assert!(is_inside_bundle(&tree, ix("Chat.app/Contents/Resources/app.asar")));
        assert!(!is_inside_bundle(&tree, ix("code/Chat.app.md")));
        assert!(is_bundle("Sparkle.framework") && is_bundle("Helper.XPC") && !is_bundle("node_modules"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn protects_scans_inside_protected_folders() {
        let none = |_: PathBuf| None;
        let library = PathBuf::from("/Users/a/Library");
        // Scanning inside ~/Library: the whole scan is off limits.
        let root = Path::new("/Users/a/Library/Application Support");
        assert_eq!(protected_in_tree(root, library.clone(), none), Some(root.to_path_buf()));
        assert_eq!(protected_in_tree(Path::new("/Applications/Chat.app"), "/Applications".into(), none), Some("/Applications/Chat.app".into()));
        // Scanning a project elsewhere: nothing protected in it.
        assert_eq!(protected_in_tree(Path::new("/Users/a/code"), library, none), None);
        // The startup disk is `/`, not part of /System; /Library on it is found by `within`.
        let data = Path::new(DATA_VOLUME);
        assert_eq!(protected_in_tree(data, "/System".into(), none), None);
        let mapped = |p: PathBuf| Some(data.join(p.strip_prefix("/").unwrap()));
        assert_eq!(protected_in_tree(data, "/Library".into(), mapped), Some(data.join("Library")));
        assert_eq!(protected_in_tree(&data.join("Library/Caches"), "/Library".into(), none), Some(data.join("Library/Caches")));
    }

    #[test]
    fn scans_through_symlinks_into_protected_folders_find_no_node_modules() {
        let dir = std::env::temp_dir().join(format!("petal-symlink-roots-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let home = dir.join("home");
        let tool = home.join("Library/Application Support/Tool");
        let project = home.join("code/app");
        for folder in [&tool, &project] {
            write(&folder.join("package.json"), 100);
            write(&folder.join("package-lock.json"), 100);
            write(&folder.join("node_modules/a/index.js"), 90_000);
        }
        let link = |name: &str, target: &Path| {
            std::os::unix::fs::symlink(target, dir.join(name)).unwrap();
            dir.join(name)
        };
        let to_library = link("library-link", &home.join("Library"));
        let to_tool_parent = link("support-link", &home.join("Library/Application Support"));
        let to_project_parent = link("code-link", &home.join("code"));

        let node_modules = |root: &Path| {
            let tree = scan(root, &Progress::default());
            let bases = Bases::for_root_with_home(root, Some(home.clone()));
            from_tree_min(&tree, &bases, 0).into_iter().any(|f| f.fix == Fix::Trash && f.title == "node_modules")
        };
        assert!(!node_modules(&to_library), "a symlink to ~/Library is still ~/Library");
        assert!(!node_modules(&to_tool_parent), "nor is a symlink to a folder inside it");
        assert!(node_modules(&to_project_parent), "a symlink to a projects folder still finds them");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Allocated size of a file, as the scan counts it.
    fn allocated(path: &Path) -> u64 {
        use std::os::unix::fs::MetadataExt;
        std::fs::symlink_metadata(path).unwrap().blocks() * 512
    }

    #[test]
    fn finds_ai_model_stores_and_agent_histories() {
        let dir = std::env::temp_dir().join(format!("petal-ai-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let home = dir.join("home");
        write(&home.join(".ollama/models/blobs/sha256-1"), 300_000);
        write(&home.join(".ollama/history"), 100);
        write(&home.join(".lmstudio/models/org/model/model.gguf"), 200_000);
        write(&home.join(".cache/lm-studio/models/org/old/old.gguf"), 200_000);
        write(&home.join(".cache/huggingface/hub/models--org--model/blobs/abc"), 250_000);
        // The login token sits next to the hub and must stay out of the finding.
        write(&home.join(".cache/huggingface/token"), 100);
        write(&home.join(".claude/projects/-Users-me-code/session.jsonl"), 50_000);
        write(&home.join(".claude/settings.json"), 100);
        write(&home.join(".codex/sessions/2026/10/08/rollout.jsonl"), 50_000);

        let tree = scan(&dir, &Progress::default());
        let findings = from_tree_min(&tree, &Bases { home: Some(home.clone()), ..Bases::default() }, 0);
        let at = |path: &str| findings.iter().find(|f| f.path.as_deref() == Some(home.join(path).as_path()));

        for path in [".ollama/models", ".lmstudio/models", ".cache/lm-studio/models", ".cache/huggingface/hub"] {
            let finding = at(path).unwrap_or_else(|| panic!("{path}: {findings:?}"));
            assert_eq!(finding.safety, Safety::Review, "{path}: models take a long download to get back");
            assert!(finding.blurb.contains("download again"), "{path}: {}", finding.blurb);
            assert_eq!(finding.size, tree.nodes[tree.find(&home.join(path)).unwrap()].size);
        }
        assert!(at(".ollama/models").unwrap().blurb.contains("`ollama rm <model>`"));
        assert!(at(".cache/huggingface").is_none() && at(".ollama").is_none(), "only the model folders");

        for path in [".claude/projects", ".codex/sessions"] {
            let finding = at(path).unwrap_or_else(|| panic!("{path}: {findings:?}"));
            assert_eq!((finding.safety, finding.fix), (Safety::ManageInApp, Fix::InApp), "{path}");
            assert!(!finding.collectable(), "{path}: someone's conversations are never offered for the Trash");
            assert!(finding.blurb.contains("resume"), "{path}: {}", finding.blurb);
        }
        assert!(at(".claude").is_none() && at(".codex").is_none(), "settings and logins stay out of it");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// `target` is a common name. Only one right next to a Cargo.toml is Cargo's build
    /// output; deleting anything else called `target` could throw away someone's work.
    #[test]
    fn cargo_target_only_next_to_cargo_toml() {
        let dir = std::env::temp_dir().join(format!("petal-cargo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let home = dir.join("home");
        let crate_at = |path: &str| {
            write(&home.join(path).join("Cargo.toml"), 100);
            write(&home.join(path).join("target/debug/app"), 20_000);
        };
        crate_at("code/tool");
        // A workspace: one target at its root, none for its members.
        crate_at("code/workspace");
        write(&home.join("code/workspace/crates/core/Cargo.toml"), 100);
        write(&home.join("code/workspace/crates/core/src/lib.rs"), 100);
        // Not Cargo's: no Cargo.toml beside it, or one only further up or down.
        write(&home.join("code/site/package.json"), 100);
        write(&home.join("code/site/target/report.html"), 20_000);
        write(&home.join("code/rusty/Cargo.toml.orig"), 100);
        write(&home.join("code/rusty/target/debug/app"), 20_000);
        write(&home.join("code/nested/Cargo.toml"), 100);
        write(&home.join("code/nested/src/target/fixture.bin"), 20_000);
        write(&home.join("Documents/Cargo.toml/target/kept.txt"), 20_000);
        // A `target` that isn't Cargo's is walked like any folder, so a project in it counts.
        crate_at("code/site/target/rust-demo");
        // Inside apps, tools' own folders and protected locations: never.
        crate_at("Applications/Tool.app/Contents/Resources/engine");
        crate_at("code/game/build/Game.app/Contents/Resources/engine");
        crate_at(".rustup/toolchains/stable/lib/rustlib/src/rust");
        crate_at(".cargo/git/checkouts/dep-1234/abc");
        crate_at("Library/Application Support/Tool/engine");

        let tree = scan(&dir, &Progress::default());
        let bases = Bases { home: Some(home.clone()), user_temp: None, protected: vec![home.join("Library")] };
        let findings = from_tree_min(&tree, &bases, 0);
        let cargo = findings.iter().find(|f| f.title == "Cargo build files").expect("cargo finding");
        let mut found: Vec<PathBuf> = cargo.nodes.iter().map(|&ix| tree.path_of(ix)).collect();
        found.sort();
        assert_eq!(
            found,
            [home.join("code/site/target/rust-demo/target"), home.join("code/tool/target"), home.join("code/workspace/target")]
        );
        assert_eq!((cargo.safety, cargo.fix, cargo.pending), (Safety::Safe, Fix::Trash, true));
        assert!(cargo.collectable());
        assert_eq!(cargo.blurb, "In 3 Rust projects; `cargo build` rebuilds them, or run `cargo clean`");
        assert_eq!(cargo.size, found.iter().map(|p| tree.nodes[tree.find(p).unwrap()].size).sum::<u64>());

        // A scan that starts inside an app finds nothing there either.
        let engine = home.join("Applications/Tool.app/Contents/Resources/engine");
        let tree = scan(&engine, &Progress::default());
        assert!(from_tree_min(&tree, &Bases::default(), 0).iter().all(|f| f.title != "Cargo build files"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn homebrew_cache_points_to_brew_cleanup_and_counts_once() {
        let dir = std::env::temp_dir().join(format!("petal-brew-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let home = dir.join("home");
        write(&home.join("Library/Caches/Homebrew/downloads/abc--node.bottle.tar.gz"), 300_000);
        write(&home.join("Library/Caches/com.example.app/blob"), 100_000);

        let tree = scan(&dir, &Progress::default());
        let findings = from_tree_min(&tree, &Bases { home: Some(home.clone()), ..Bases::default() }, 0);
        let by_title = |title: &str| findings.iter().find(|f| f.title == title).unwrap_or_else(|| panic!("{title}: {findings:?}"));
        let brew = by_title("Homebrew downloads");
        assert_eq!(brew.path.as_deref(), Some(home.join("Library/Caches/Homebrew").as_path()));
        assert_eq!((brew.safety, brew.fix), (Safety::Safe, Fix::Command("brew cleanup --prune=all")));
        assert!(!brew.collectable(), "brew cleanup is the way to clear it");
        // Homebrew's downloads are part of App caches: the Safe total counts them once.
        let caches = by_title("App caches");
        assert!(caches.size > brew.size);
        assert_eq!(safe_total(&findings), caches.size);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// pnpm's store and the projects installed from it share files (hard links here; clones
    /// work the same way), so the store frees only what no project links to.
    #[test]
    fn pnpm_store_counts_only_what_deleting_frees() {
        let dir = std::env::temp_dir().join(format!("petal-pnpm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let home = dir.join("home");
        let store = home.join("Library/pnpm/store");
        let linked = store.join("v10/files/aa/linked");
        let unused = store.join("v10/files/bb/unused");
        write(&linked, 1_000_000);
        write(&unused, 100_000);
        let modules = home.join("code/app/node_modules");
        write(&home.join("code/app/package.json"), 100);
        write(&home.join("code/app/pnpm-lock.yaml"), 100);
        std::fs::create_dir_all(modules.join(".pnpm/pkg")).unwrap();
        std::fs::hard_link(&linked, modules.join(".pnpm/pkg/index.js")).unwrap();

        let tree = scan(&dir, &Progress::default());
        let findings = from_tree_min(&tree, &Bases { home: Some(home.clone()), ..Bases::default() }, 0);
        let pnpm = findings.iter().find(|f| f.title == "pnpm store").expect("pnpm store");
        assert_eq!(pnpm.path.as_deref(), Some(store.as_path()));
        assert_eq!((pnpm.safety, pnpm.fix), (Safety::Review, Fix::Command("pnpm store prune")));
        assert!(pnpm.pending && !pnpm.collectable());
        // What the app shows once worked out: only the file no project links to.
        let frees = crate::scan::frees_of(&[store.clone()]);
        assert!(frees >= allocated(&unused) && frees < allocated(&unused) + allocated(&linked), "frees {frees}");
        // Deleting the project too frees the shared file.
        assert!(crate::scan::frees_of(&[store.clone(), modules.clone()]) >= allocated(&unused) + allocated(&linked));

        // Mid-scan, the hotspot pass gave the store the shared files' space: show it as
        // still being worked out rather than as what it frees.
        let category = CATALOG.iter().position(|c| c.path == "Library/pnpm/store").unwrap();
        let trash = CATALOG.iter().position(|c| c.path == ".Trash").unwrap();
        let early = |category| Early { category, path: store.clone(), size: MIN_SIZE, at_ms: 1 };
        let shown = early_findings(&[early(category), early(trash)]);
        let by_title = |title: &str| shown.iter().find(|f| f.title == title).unwrap();
        assert!(by_title("pnpm store").pending && !by_title("Trash").pending);
        assert_eq!(by_title("pnpm store").fix, Fix::Command("pnpm store prune"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Docker.raw is sparse: its apparent size is the disk image's limit (often 64 GB or
    /// more), but only what Docker has written takes space.
    #[test]
    fn docker_disk_image_counts_allocated_space() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("petal-docker-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let home = dir.join("home");
        let image = home.join("Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw");
        std::fs::create_dir_all(image.parent().unwrap()).unwrap();
        let mut file = std::fs::File::create(&image).unwrap();
        file.write_all(&vec![1u8; 200_000]).unwrap();
        file.set_len(4 << 30).unwrap();
        drop(file);
        let apparent = std::fs::metadata(&image).unwrap().len();
        assert!(allocated(&image) < apparent / 100, "the test file should be sparse");

        let tree = scan(&dir, &Progress::default());
        let findings = from_tree_min(&tree, &Bases { home: Some(home.clone()), ..Bases::default() }, 0);
        let docker = findings.iter().find(|f| f.title == "Docker disk image").expect("docker");
        assert_eq!(docker.path.as_deref(), Some(image.as_path()));
        assert_eq!(docker.size, allocated(&image), "allocated, not apparent, size");
        assert_eq!(crate::scan::frees_of(&[image.clone()]), allocated(&image));
        // It's the app's data, so it's managed there, never trashed.
        assert_eq!((docker.safety, docker.fix), (Safety::ManageInApp, Fix::Command("docker system prune")));
        assert!(!docker.collectable());
        // And nothing else in Docker's container is offered instead.
        assert!(findings.iter().all(|f| f.path.as_deref() == Some(image.as_path()) || !f.path.as_ref().is_some_and(|p| p.starts_with(home.join("Library/Containers")))));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// "Manage in app" findings, and those with a command to run instead, never go in the
    /// Collector, so they can't be moved to the Trash from the findings.
    #[test]
    fn only_trash_findings_are_collectable() {
        for category in CATALOG {
            if category.safety == Safety::ManageInApp {
                assert_ne!(category.fix, Fix::Trash, "{} must not be offered for the Trash", category.path);
            }
            assert!(category.fix != Fix::GitGc, "{}: git gc is only for repositories", category.path);
        }
        let finding = |safety, fix| Finding {
            title: "x",
            blurb: String::new(),
            safety,
            size: 0,
            allocated: 0,
            path: None,
            nodes: vec![1],
            pending: false,
            fix,
        };
        assert!(finding(Safety::Safe, Fix::Trash).collectable());
        assert!(finding(Safety::Review, Fix::Trash).collectable());
        assert!(!finding(Safety::ManageInApp, Fix::Trash).collectable());
        assert!(!finding(Safety::Safe, Fix::Command("brew cleanup")).collectable());
        assert!(!finding(Safety::Review, Fix::GitGc).collectable());
        assert!(!finding(Safety::ManageInApp, Fix::InApp).collectable());
    }

    #[test]
    fn safe_total_counts_nested_findings_once() {
        let finding = |path: &str, safety, size| Finding {
            title: "x",
            blurb: String::new(),
            safety,
            size,
            allocated: size,
            path: Some(PathBuf::from(path)),
            nodes: Vec::new(),
            pending: false,
            fix: Fix::Trash,
        };
        let findings = [
            finding("/h/Library/Caches", Safety::Safe, 100),
            finding("/h/Library/Caches/Homebrew", Safety::Safe, 40),
            finding("/h/Library/CachesOld", Safety::Safe, 7),
            finding("/h/.Trash", Safety::Safe, 10),
            finding("/h/Downloads", Safety::Review, 1000),
            finding("/h/Downloads/big", Safety::Safe, 5),
        ];
        assert_eq!(safe_total(&findings), 100 + 7 + 10 + 5);
    }

    #[test]
    fn git_gc_commands_quote_paths() {
        let dirs = [PathBuf::from("/a/repo/.git"), PathBuf::from("/b/it's here/.git")];
        assert_eq!(git_gc_commands(&dirs), "git -C '/a/repo' gc\ngit -C '/b/it'\\''s here' gc");
    }
}
