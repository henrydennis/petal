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

/// What to do about a finding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fix {
    /// Collect its folders and move them to the Trash.
    Trash,
    /// Run `git gc` in each repository. Its folders are `.git` folders, which must never
    /// be collected: that would throw away the repository's history.
    GitGc,
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
                pending: false,
                fix: Fix::Trash,
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
/// project and every Git repository with a lot of loose objects.
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
                    fix: Fix::Trash,
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
    use std::os::unix::fs::MetadataExt;
    let has = |names: &[&str]| {
        tree.nodes[ix].children.iter().any(|&c| tree.nodes[c].kind == Kind::File && names.contains(&tree.nodes[c].name.as_ref()))
    };
    has(&["package.json"])
        && has(LOCKFILES)
        && std::fs::metadata(tree.path_of(ix)).is_ok_and(|m| m.uid() == unsafe { libc::getuid() })
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

    #[test]
    fn git_gc_commands_quote_paths() {
        let dirs = [PathBuf::from("/a/repo/.git"), PathBuf::from("/b/it's here/.git")];
        assert_eq!(git_gc_commands(&dirs), "git -C '/a/repo' gc\ngit -C '/b/it'\\''s here' gc");
    }
}
