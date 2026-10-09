//! Windowless commands for scripts and coding agents: `petal scan PATH` and
//! `petal findings [PATH]`, each with `--json`. They use the same scan as `--bench-scan`
//! and never open a window. The JSON schema is documented in the README; bump
//! `SCHEMA_VERSION` before removing or renaming any field.

use std::cmp::Reverse;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::findings::{self, Finding, Fix, Safety};
use crate::json::Json;
use crate::scan::{self, Kind, Progress, Tree, format_count, format_size};

pub const SCHEMA_VERSION: u64 = 1;
/// Levels of folders listed below the scanned folder.
pub const DEFAULT_DEPTH: usize = 2;
/// Largest items listed per folder; the rest are summed into one "other" entry.
pub const DEFAULT_TOP: usize = 20;

const USAGE: &str = "\
Usage:
  petal                     open the app
  petal PATH                open the app and scan PATH
  petal scan PATH [--json] [--depth N] [--top N]
  petal findings [PATH] [--json]

  scan        how much space PATH and the folders in it take
  findings    well-known space hogs under PATH (default: your home folder), whether
              they're safe to delete, and exactly what deleting them frees
  --json      print JSON instead of a summary (the README describes the schema)
  --depth N   levels of folders to list below PATH (default 2)
  --top N     largest items to list in each folder; the rest are added up into one
              \"other\" entry (default 20)
";

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Scan { path: PathBuf, json: bool, depth: usize, top: usize },
    Findings { path: Option<PathBuf>, json: bool },
    Help,
}

/// Run `petal <command> <args…>` and return the process's exit code: 0 on success, 1 if
/// the folder couldn't be scanned, 2 for invalid arguments.
pub fn main(command: &str, args: &[String]) -> i32 {
    match parse(command, args) {
        Ok(Command::Help) => {
            print_out(USAGE);
            0
        }
        Ok(command) => match run(command) {
            Ok(out) => {
                print_out(&out);
                0
            }
            Err(message) => {
                eprintln!("petal: {message}");
                1
            }
        },
        Err(message) => {
            eprintln!("petal: {message}\n\n{USAGE}");
            2
        }
    }
}

/// Write to stdout, without panicking if the reader has gone away (`petal … | head`).
fn print_out(text: &str) {
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(text.as_bytes()).and_then(|_| stdout.flush());
}

pub fn parse(command: &str, args: &[String]) -> Result<Command, String> {
    let scan = command == "scan";
    let (mut json, mut depth, mut top) = (false, DEFAULT_DEPTH, DEFAULT_TOP);
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut options_done = false;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if options_done || !arg.starts_with('-') || arg == "-" {
            paths.push(PathBuf::from(arg));
            continue;
        }
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) => (flag, Some(value)),
            None => (arg.as_str(), None),
        };
        match flag {
            "--" => options_done = true,
            "-h" | "--help" => return Ok(Command::Help),
            "--json" if inline.is_none() => json = true,
            "--depth" | "--top" if scan => {
                let value = inline.or_else(|| args.next().map(String::as_str)).ok_or(format!("{flag} needs a number"))?;
                let n: usize = value.parse().map_err(|_| format!("{flag} needs a whole number, not {value:?}"))?;
                if flag == "--depth" {
                    depth = n;
                } else if n == 0 {
                    return Err("--top needs a number of at least 1".into());
                } else {
                    top = n;
                }
            }
            _ => return Err(format!("unknown option {arg:?} for `petal {command}`")),
        }
    }
    if paths.len() > 1 {
        return Err(format!("`petal {command}` takes one folder, not {}", paths.len()));
    }
    let path = paths.pop();
    if scan {
        let path = path.ok_or("`petal scan` needs a folder to scan")?;
        Ok(Command::Scan { path, json, depth, top })
    } else {
        Ok(Command::Findings { path, json })
    }
}

/// Carry out a parsed command; the text to print, or what went wrong.
pub fn run(command: Command) -> Result<String, String> {
    match command {
        Command::Help => Ok(USAGE.to_string()),
        Command::Scan { path, json, depth, top } => {
            let root = folder(&path)?;
            let tree = scan::scan(&root, &Progress::default());
            Ok(if json { scan_json(&tree, depth, top).render() } else { scan_summary(&tree, top) })
        }
        Command::Findings { path, json } => {
            let path = match path {
                Some(path) => path,
                None => std::env::var_os("HOME")
                    .filter(|home| !home.is_empty())
                    .map(PathBuf::from)
                    .ok_or("HOME isn't set; name a folder: `petal findings PATH`")?,
            };
            let root = folder(&path)?;
            let tree = scan::scan(&root, &Progress::default());
            let found = resolve_findings(&tree);
            Ok(if json { findings_json(&tree.root_path, &found).render() } else { findings_summary(&tree.root_path, &found) })
        }
    }
}

/// The folder to scan, made absolute (without resolving symlinks, as the app does), or
/// why it can't be scanned.
fn folder(path: &Path) -> Result<PathBuf, String> {
    let absolute = std::path::absolute(path).map_err(|e| format!("{}: {e}", path.display()))?;
    // Drop `.` components and any trailing slash.
    let path: PathBuf = absolute.components().collect();
    let meta = std::fs::metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !meta.is_dir() {
        return Err(format!("{}: not a folder", path.display()));
    }
    std::fs::read_dir(&path).map_err(|e| format!("couldn't read {}: {e}", path.display()))?;
    Ok(path)
}

/// The JSON for `petal scan --json`.
pub fn scan_json(tree: &Tree, depth: usize, top: usize) -> Json {
    let root = &tree.nodes[Tree::ROOT];
    let mut folders = 0u64;
    let mut stack = vec![Tree::ROOT];
    while let Some(ix) = stack.pop() {
        for &child in &tree.nodes[ix].children {
            if tree.nodes[child].kind == Kind::Dir {
                folders += 1;
                stack.push(child);
            }
        }
    }
    let unreadable = tree
        .unreadable
        .iter()
        .map(|u| Json::Object(vec![("path", Json::lossy(u.path.as_os_str())), ("whole", Json::Bool(u.whole))]))
        .collect();
    let snapshots = tree
        .snapshots
        .iter()
        .map(|s| Json::Object(vec![("name", Json::Str(s.name.clone())), ("created", Json::Int(s.created.max(0) as u64))]))
        .collect();
    Json::Object(vec![
        ("schema_version", Json::Int(SCHEMA_VERSION)),
        ("petal_version", Json::Str(env!("CARGO_PKG_VERSION").to_string())),
        ("command", Json::Str("scan".into())),
        ("root", Json::lossy(tree.root_path.as_os_str())),
        ("size_bytes", Json::Int(root.size)),
        ("files", Json::Int(root.items)),
        ("folders", Json::Int(folders)),
        ("errors", Json::Int(tree.errors)),
        ("unreadable", Json::Array(unreadable)),
        ("cloud_only_folders", Json::Int(tree.cloud_only)),
        ("purgeable_bytes", tree.purgeable.map_or(Json::Null, Json::Int)),
        ("snapshots", Json::Array(snapshots)),
        ("depth", Json::Int(depth as u64)),
        ("top", Json::Int(top as u64)),
        ("tree", entry(tree, Tree::ROOT, depth, top)),
    ])
}

/// One node of the tree, with its children listed while `depth_left` lasts: the `top`
/// largest, then one "other" entry for the rest, so the children always add up.
fn entry(tree: &Tree, ix: usize, depth_left: usize, top: usize) -> Json {
    let node = &tree.nodes[ix];
    let (kind, path) = match node.kind {
        Kind::Dir => ("folder", Json::lossy(tree.path_of(ix).as_os_str())),
        Kind::File => ("file", Json::lossy(tree.path_of(ix).as_os_str())),
        // Space with no place on disk to open: another APFS volume, or what couldn't be read.
        Kind::Other => ("slice", Json::Null),
    };
    let mut fields = vec![
        ("name", Json::Str(node.name.to_string())),
        ("path", path),
        ("kind", Json::Str(kind.into())),
        ("size_bytes", Json::Int(node.size)),
        ("files", Json::Int(node.items)),
    ];
    if node.kind == Kind::Dir && depth_left > 0 {
        let in_children: u64 = node.children.iter().map(|&c| tree.nodes[c].size).sum();
        fields.push(("own_bytes", Json::Int(node.size.saturating_sub(in_children))));
        let shown = node.children.len().min(top);
        let mut children: Vec<Json> =
            node.children[..shown].iter().map(|&c| entry(tree, c, depth_left - 1, top)).collect();
        let rest = &node.children[shown..];
        if !rest.is_empty() {
            let size: u64 = rest.iter().map(|&c| tree.nodes[c].size).sum();
            let files: u64 = rest.iter().map(|&c| tree.nodes[c].items).sum();
            children.push(Json::Object(vec![
                ("name", Json::Str(smaller_items(rest.len()))),
                ("path", Json::Null),
                ("kind", Json::Str("other".into())),
                ("size_bytes", Json::Int(size)),
                ("files", Json::Int(files)),
                ("count", Json::Int(rest.len() as u64)),
            ]));
        }
        fields.push(("children", Json::Array(children)));
    }
    Json::Object(fields)
}

fn smaller_items(n: usize) -> String {
    if n == 1 { "1 smaller item".to_string() } else { format!("{} smaller items", format_count(n as u64)) }
}

/// The short text `petal scan` prints without `--json`.
fn scan_summary(tree: &Tree, top: usize) -> String {
    let root = &tree.nodes[Tree::ROOT];
    let mut out = format!(
        "{}: {} in {} files (allocated size)\n",
        tree.root_path.display(),
        format_size(root.size),
        format_count(root.items)
    );
    let shown = root.children.len().min(top);
    for &c in &root.children[..shown] {
        let node = &tree.nodes[c];
        let slash = if node.kind == Kind::Dir { "/" } else { "" };
        out.push_str(&format!("  {:>10}  {}{slash}\n", format_size(node.size), node.name));
    }
    let rest = &root.children[shown..];
    if !rest.is_empty() {
        let size: u64 = rest.iter().map(|&c| tree.nodes[c].size).sum();
        out.push_str(&format!("  {:>10}  {}\n", format_size(size), smaller_items(rest.len())));
    }
    if !tree.unreadable.is_empty() {
        let n = tree.unreadable.len();
        out.push_str(&format!("{} {} couldn't be read\n", format_count(n as u64), if n == 1 { "folder" } else { "folders" }));
    }
    out
}

/// A finding with its folders' paths and exactly what deleting all of them frees (none
/// for `git gc`, whose folders must never be deleted).
pub struct Resolved {
    pub finding: Finding,
    pub paths: Vec<PathBuf>,
    pub frees: Option<u64>,
}

/// Findings from a finished scan, largest saving first. Works out what each frees, as the
/// app does in the background (this walks their folders again).
fn resolve_findings(tree: &Tree) -> Vec<Resolved> {
    let found = findings::from_tree(tree, &findings::Bases::for_root(&tree.root_path));
    let mut resolved: Vec<Resolved> = found
        .into_iter()
        .map(|finding| {
            let paths: Vec<PathBuf> = finding.nodes.iter().map(|&ix| tree.path_of(ix)).collect();
            // `git gc` packs a repository's loose objects rather than deleting folders. The
            // others are worked out as the app does, so a pnpm store counts only what no
            // project links to.
            let frees = (finding.fix != Fix::GitGc).then(|| scan::frees_of(&paths));
            Resolved { finding, paths, frees }
        })
        .collect();
    resolved.sort_by_key(|r| Reverse(r.frees.unwrap_or(r.finding.size)));
    resolved
}

/// A stable machine name for a finding, from its title: "Xcode build files" is
/// `xcode_build_files`.
pub fn finding_id(title: &str) -> String {
    let mut id = String::new();
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            id.push(ch.to_ascii_lowercase());
        } else if !id.is_empty() && !id.ends_with('_') {
            id.push('_');
        }
    }
    while id.ends_with('_') {
        id.pop();
    }
    id
}

fn safety_name(safety: Safety) -> &'static str {
    match safety {
        Safety::Safe => "safe",
        Safety::Review => "review",
        Safety::ManageInApp => "manage_in_app",
    }
}

/// The JSON for `petal findings --json`.
pub fn findings_json(root: &Path, found: &[Resolved]) -> Json {
    let findings = found
        .iter()
        .map(|r| {
            let f = &r.finding;
            let (action, commands) = match f.fix {
                Fix::Trash => ("trash", Vec::new()),
                Fix::GitGc => (
                    "git_gc",
                    findings::git_gc_commands(&r.paths).lines().map(|line| Json::Str(line.to_string())).collect(),
                ),
                Fix::Command(command) => ("command", vec![Json::Str(command.to_string())]),
                Fix::InApp => ("in_app", Vec::new()),
            };
            // Only what Petal would move to the Trash has a saving to report: the rest is
            // cleared by a command or in its app, which free some other amount.
            let frees = if f.collectable() { r.frees } else { None };
            Json::Object(vec![
                ("id", Json::Str(finding_id(f.title))),
                ("title", Json::Str(f.title.to_string())),
                ("explanation", Json::Str(f.blurb.clone())),
                ("safety", Json::Str(safety_name(f.safety).into())),
                ("action", Json::Str(action.into())),
                ("collectable", Json::Bool(f.collectable())),
                ("paths", Json::Array(r.paths.iter().map(|p| Json::lossy(p.as_os_str())).collect())),
                ("allocated_bytes", Json::Int(f.allocated)),
                ("frees_bytes", frees.map_or(Json::Null, Json::Int)),
                ("commands", Json::Array(commands)),
            ])
        })
        .collect();
    Json::Object(vec![
        ("schema_version", Json::Int(SCHEMA_VERSION)),
        ("petal_version", Json::Str(env!("CARGO_PKG_VERSION").to_string())),
        ("command", Json::Str("findings".into())),
        ("root", Json::lossy(root.as_os_str())),
        ("min_size_bytes", Json::Int(findings::MIN_SIZE)),
        ("findings", Json::Array(findings)),
    ])
}

/// The short text `petal findings` prints without `--json`.
fn findings_summary(root: &Path, found: &[Resolved]) -> String {
    if found.is_empty() {
        return format!("No findings over {} in {}\n", format_size(findings::MIN_SIZE), root.display());
    }
    let mut out = format!("Findings in {} (what deleting each frees):\n", root.display());
    for r in found {
        let f = &r.finding;
        let label = match f.safety {
            Safety::Safe => "Safe",
            Safety::Review => "Review",
            Safety::ManageInApp => "Manage",
        };
        let size = r.frees.unwrap_or(f.size);
        out.push_str(&format!("  {label:<6}  {:>10}  {}: {}\n", format_size(size), f.title, f.blurb));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::Node;
    use std::fs;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_arguments() {
        assert_eq!(
            parse("scan", &args(&["/tmp", "--json"])),
            Ok(Command::Scan { path: "/tmp".into(), json: true, depth: DEFAULT_DEPTH, top: DEFAULT_TOP })
        );
        assert_eq!(
            parse("scan", &args(&["--depth", "4", "--top=5", "x"])),
            Ok(Command::Scan { path: "x".into(), json: false, depth: 4, top: 5 })
        );
        assert_eq!(parse("scan", &args(&["--", "-odd"])), Ok(Command::Scan { path: "-odd".into(), json: false, depth: 2, top: 20 }));
        assert_eq!(parse("findings", &args(&["--json"])), Ok(Command::Findings { path: None, json: true }));
        assert_eq!(parse("findings", &args(&["~/code"])), Ok(Command::Findings { path: Some("~/code".into()), json: false }));
        assert_eq!(parse("findings", &args(&["--help"])), Ok(Command::Help));

        assert!(parse("scan", &args(&[])).is_err(), "scan needs a path");
        assert!(parse("scan", &args(&["a", "b"])).is_err());
        assert!(parse("scan", &args(&["a", "--depth"])).is_err());
        assert!(parse("scan", &args(&["a", "--depth", "two"])).is_err());
        assert!(parse("scan", &args(&["a", "--top", "0"])).is_err());
        assert!(parse("scan", &args(&["a", "--jsonx"])).is_err());
        assert!(parse("scan", &args(&["a", "--json=yes"])).is_err());
        assert!(parse("findings", &args(&["--depth", "3"])).is_err(), "findings has no tree to limit");
    }

    /// A tree from (name, kind, size, parent) rows, the root first; folders' sizes are
    /// their own allocation plus their children's, as a scan gives them.
    fn tree(rows: &[(&str, Kind, u64, Option<usize>)]) -> Tree {
        let mut nodes: Vec<Node> = rows
            .iter()
            .map(|&(name, kind, size, parent)| Node {
                name: name.to_string().into(),
                size,
                kind,
                parent,
                children: Vec::new(),
                items: u64::from(kind == Kind::File),
            })
            .collect();
        for ix in (1..nodes.len()).rev() {
            let parent = nodes[ix].parent.unwrap();
            let (size, items) = (nodes[ix].size, nodes[ix].items);
            nodes[parent].size += size;
            nodes[parent].items += items;
        }
        for ix in 1..nodes.len() {
            let parent = nodes[ix].parent.unwrap();
            nodes[parent].children.push(ix);
        }
        for ix in 0..nodes.len() {
            scan::sort_children(&mut nodes, ix);
        }
        Tree { root_path: PathBuf::from("/data"), nodes, ..Default::default() }
    }

    /// Every listed folder's children plus its own allocation make up its size and file
    /// count; returns how many entries there are in all.
    fn check_adds_up(entry: &Json) -> usize {
        let size = entry.get("size_bytes").and_then(Json::as_u64).unwrap();
        let files = entry.get("files").and_then(Json::as_u64).unwrap();
        let Some(children) = entry.get("children").and_then(Json::as_array) else { return 1 };
        let own = entry.get("own_bytes").and_then(Json::as_u64).expect("own_bytes with children");
        let sum = |key: &str| children.iter().map(|c| c.get(key).and_then(Json::as_u64).unwrap()).sum::<u64>();
        assert_eq!(own + sum("size_bytes"), size, "sizes add up in {:?}", entry.get("name"));
        assert_eq!(sum("files"), files, "files add up in {:?}", entry.get("name"));
        1 + children.iter().map(check_adds_up).sum::<usize>()
    }

    #[test]
    fn scan_json_lists_the_largest_and_adds_up_the_rest() {
        let tree = tree(&[
            ("data", Kind::Dir, 100, None),
            ("big", Kind::Dir, 50, Some(0)),
            ("a.bin", Kind::File, 9_000, Some(1)),
            ("b.bin", Kind::File, 4_000, Some(1)),
            ("c.bin", Kind::File, 3_000, Some(1)),
            ("d.bin", Kind::File, 1_000, Some(1)),
            ("one.txt", Kind::File, 5_000, Some(0)),
            ("two.txt", Kind::File, 700, Some(0)),
            ("three.txt", Kind::File, 600, Some(0)),
            ("macOS", Kind::Other, 2_000, Some(0)),
        ]);
        let json = scan_json(&tree, 2, 2);
        assert_eq!(json.get("schema_version").and_then(Json::as_u64), Some(1));
        assert_eq!(json.get("petal_version").and_then(Json::as_str), Some(env!("CARGO_PKG_VERSION")));
        assert_eq!(json.get("command").and_then(Json::as_str), Some("scan"));
        assert_eq!(json.get("root").and_then(Json::as_str), Some("/data"));
        assert_eq!(json.get("size_bytes").and_then(Json::as_u64), Some(100 + 50 + 17_000 + 6_300 + 2_000));
        assert_eq!(json.get("files").and_then(Json::as_u64), Some(7));
        assert_eq!(json.get("folders").and_then(Json::as_u64), Some(1));
        assert_eq!(json.get("purgeable_bytes"), Some(&Json::Null));

        let root = json.get("tree").unwrap();
        assert_eq!(root.get("size_bytes"), json.get("size_bytes"));
        assert_eq!(root.get("own_bytes").and_then(Json::as_u64), Some(100));
        let children = root.get("children").and_then(Json::as_array).unwrap();
        let names: Vec<&str> = children.iter().map(|c| c.get("name").and_then(Json::as_str).unwrap()).collect();
        assert_eq!(names, ["big", "one.txt", "3 smaller items"]);
        let other = &children[2];
        assert_eq!(other.get("kind").and_then(Json::as_str), Some("other"));
        assert_eq!(other.get("path"), Some(&Json::Null));
        assert_eq!(other.get("count").and_then(Json::as_u64), Some(3));
        assert_eq!(other.get("size_bytes").and_then(Json::as_u64), Some(2_000 + 700 + 600));
        assert_eq!(other.get("files").and_then(Json::as_u64), Some(2));

        let big = &children[0];
        assert_eq!(big.get("path").and_then(Json::as_str), Some("/data/big"));
        assert_eq!(big.get("kind").and_then(Json::as_str), Some("folder"));
        assert_eq!(big.get("children").and_then(Json::as_array).map(|c| c.len()), Some(3));
        assert_eq!(check_adds_up(root), 1 + 3 + 3);

        // At the depth limit a folder is listed without its contents.
        let shallow = scan_json(&tree, 1, 20);
        let root = shallow.get("tree").unwrap();
        let children = root.get("children").and_then(Json::as_array).unwrap();
        assert_eq!(children.len(), 5, "no \"other\" entry when everything fits");
        assert!(children.iter().all(|c| c.get("children").is_none()));
        assert_eq!(children.iter().find(|c| c.get("kind").and_then(Json::as_str) == Some("slice")).unwrap().get("path"), Some(&Json::Null));
        check_adds_up(root);
        assert!(scan_json(&tree, 0, 20).get("tree").unwrap().get("children").is_none());

        let text = json.render();
        assert!(text.starts_with("{\n  \"schema_version\": 1,\n  \"petal_version\": "), "{text}");
    }

    fn finding(title: &'static str, safety: Safety, fix: Fix, size: u64) -> Finding {
        Finding {
            title,
            blurb: format!("About {title}"),
            safety,
            size,
            allocated: size,
            path: None,
            nodes: Vec::new(),
            pending: false,
            fix,
        }
    }

    #[test]
    fn findings_json_has_labels_savings_and_commands() {
        let found = [
            Resolved {
                finding: finding("App caches", Safety::Safe, Fix::Trash, 900),
                paths: vec!["/home/me/Library/Caches".into()],
                frees: Some(800),
            },
            Resolved {
                finding: finding("Unpacked Git data", Safety::Review, Fix::GitGc, 500),
                paths: vec!["/home/me/a/.git".into(), "/home/me/it's/.git".into()],
                frees: None,
            },
            Resolved {
                finding: finding("Docker disk image", Safety::ManageInApp, Fix::Command("docker system prune"), 400),
                paths: vec!["/home/me/Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw".into()],
                frees: Some(400),
            },
            Resolved {
                finding: finding("Claude Code history", Safety::ManageInApp, Fix::InApp, 300),
                paths: vec!["/home/me/.claude/projects".into()],
                frees: Some(300),
            },
        ];
        let json = findings_json(Path::new("/home/me"), &found);
        assert_eq!(json.get("schema_version").and_then(Json::as_u64), Some(1));
        assert_eq!(json.get("command").and_then(Json::as_str), Some("findings"));
        assert_eq!(json.get("min_size_bytes").and_then(Json::as_u64), Some(findings::MIN_SIZE));
        let list = json.get("findings").and_then(Json::as_array).unwrap();
        let caches = &list[0];
        assert_eq!(caches.get("id").and_then(Json::as_str), Some("app_caches"));
        assert_eq!(caches.get("safety").and_then(Json::as_str), Some("safe"));
        assert_eq!(caches.get("action").and_then(Json::as_str), Some("trash"));
        assert_eq!(caches.get("collectable"), Some(&Json::Bool(true)));
        assert_eq!(caches.get("explanation").and_then(Json::as_str), Some("About App caches"));
        assert_eq!(caches.get("allocated_bytes").and_then(Json::as_u64), Some(900));
        assert_eq!(caches.get("frees_bytes").and_then(Json::as_u64), Some(800));
        assert_eq!(caches.get("commands"), Some(&Json::Array(Vec::new())));
        let git = &list[1];
        assert_eq!(git.get("safety").and_then(Json::as_str), Some("review"));
        assert_eq!(git.get("action").and_then(Json::as_str), Some("git_gc"));
        assert_eq!(git.get("collectable"), Some(&Json::Bool(false)));
        assert_eq!(git.get("frees_bytes"), Some(&Json::Null));
        assert_eq!(
            git.get("commands"),
            Some(&Json::Array(vec![
                Json::Str("git -C '/home/me/a' gc".into()),
                Json::Str("git -C '/home/me/it'\\''s' gc".into())
            ]))
        );
        assert_eq!(git.get("paths").and_then(Json::as_array).map(|p| p.len()), Some(2));

        // Cleared with the tool's own command, never trashed: no saving to report.
        let docker = &list[2];
        assert_eq!(docker.get("id").and_then(Json::as_str), Some("docker_disk_image"));
        assert_eq!(docker.get("safety").and_then(Json::as_str), Some("manage_in_app"));
        assert_eq!(docker.get("action").and_then(Json::as_str), Some("command"));
        assert_eq!(docker.get("collectable"), Some(&Json::Bool(false)));
        assert_eq!(docker.get("frees_bytes"), Some(&Json::Null));
        assert_eq!(docker.get("allocated_bytes").and_then(Json::as_u64), Some(400));
        assert_eq!(docker.get("commands"), Some(&Json::Array(vec![Json::Str("docker system prune".into())])));

        // Managed in its app: nothing to trash or run.
        let history = &list[3];
        assert_eq!(history.get("safety").and_then(Json::as_str), Some("manage_in_app"));
        assert_eq!(history.get("action").and_then(Json::as_str), Some("in_app"));
        assert_eq!(history.get("collectable"), Some(&Json::Bool(false)));
        assert_eq!(history.get("frees_bytes"), Some(&Json::Null));
        assert_eq!(history.get("commands"), Some(&Json::Array(Vec::new())));
    }

    /// The action and command each catalog location gets in the JSON.
    #[test]
    fn catalog_commands_appear_in_json() {
        let found: Vec<Resolved> = findings::CATALOG
            .iter()
            .map(|c| Resolved { finding: finding(c.title, c.safety, c.fix, 100), paths: vec![c.path.into()], frees: Some(100) })
            .collect();
        let json = findings_json(Path::new("/home/me"), &found);
        let list = json.get("findings").and_then(Json::as_array).unwrap();
        let get = |id: &str, key: &str| {
            let entry = list.iter().find(|f| f.get("id").and_then(Json::as_str) == Some(id)).unwrap_or_else(|| panic!("{id}"));
            entry.get(key).cloned().unwrap()
        };
        let commands = |id: &str| get(id, "commands");
        assert_eq!(commands("homebrew_downloads"), Json::Array(vec![Json::Str("brew cleanup --prune=all".into())]));
        assert_eq!(commands("pnpm_store"), Json::Array(vec![Json::Str("pnpm store prune".into())]));
        assert_eq!(get("homebrew_downloads", "safety"), Json::Str("safe".into()));
        assert_eq!(get("pnpm_store", "safety"), Json::Str("review".into()));
        assert_eq!(get("codex_history", "action"), Json::Str("in_app".into()));
        assert_eq!(get("npm_cache", "action"), Json::Str("trash".into()));
        assert_eq!(get("npm_cache", "frees_bytes"), Json::Int(100));
        for entry in list {
            // Only trashed findings report a saving, and only they can be collected.
            let collectable = entry.get("collectable") == Some(&Json::Bool(true));
            assert_eq!(entry.get("action") == Some(&Json::Str("trash".into())), collectable, "{entry:?}");
            assert_eq!(entry.get("frees_bytes") != Some(&Json::Null), collectable, "{entry:?}");
        }
    }

    /// Ids are part of the JSON schema: renaming a finding mustn't change its id silently.
    /// A tool that keeps its data in more than one place (LM Studio, pnpm) has one id for
    /// all of them, but different findings never share one.
    #[test]
    fn finding_ids_are_stable_and_unique() {
        let ids: Vec<String> = findings::CATALOG.iter().map(|c| finding_id(c.title)).collect();
        assert_eq!(
            ids,
            [
                "trash",
                "downloads",
                "xcode_build_files",
                "xcode_archives",
                "ios_device_support",
                "ios_simulators",
                "iphone_ipad_backups",
                "docker_disk_image",
                "ollama_models",
                "lm_studio_models",
                "lm_studio_models",
                "hugging_face_models",
                "app_caches",
                "homebrew_downloads",
                "npm_cache",
                "pnpm_store",
                "pnpm_store",
                "pnpm_store",
                "cargo_registry",
                "gradle_caches",
                "movies",
                "chrome_update_leftovers",
                "mail",
                "claude_code_history",
                "codex_history",
            ]
        );
        assert_eq!(finding_id("node_modules"), "node_modules");
        assert_eq!(finding_id("Unpacked Git data"), "unpacked_git_data");
        assert_eq!(finding_id("Cargo build files"), "cargo_build_files");
        let mut titles: Vec<&str> = findings::CATALOG.iter().map(|c| c.title).collect();
        titles.extend(["node_modules", "Unpacked Git data", "Cargo build files"]);
        titles.sort();
        titles.dedup();
        let mut unique: Vec<String> = titles.iter().map(|t| finding_id(t)).collect();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), titles.len(), "two findings share an id");
    }

    #[test]
    fn refuses_missing_paths_and_files() {
        let dir = std::env::temp_dir().join(format!("petal-cli-errors-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("file.txt"), b"hi").unwrap();
        let scan = |path: PathBuf| run(Command::Scan { path, json: true, depth: 2, top: 20 });
        let missing = scan(dir.join("missing")).unwrap_err();
        assert!(missing.contains("missing") && !missing.contains('{'), "{missing}");
        assert!(scan(dir.join("file.txt")).unwrap_err().ends_with("not a folder"));
        assert!(run(Command::Findings { path: Some(dir.join("missing")), json: true }).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    /// A real scan through the command: files of known allocated sizes, a hard link (counted
    /// once) and an APFS clone (counted at its full allocation, as Finder does).
    #[test]
    fn scan_command_totals_match_the_files() {
        use std::os::unix::fs::MetadataExt;
        let dir = std::env::temp_dir().join(format!("petal-cli-scan-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let write = |path: &Path, bytes: usize| {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, vec![3u8; bytes]).unwrap();
        };
        write(&dir.join("big/a.bin"), 64 * 1024);
        write(&dir.join("big/nested/b.bin"), 32 * 1024);
        write(&dir.join("small \"quoted\"\\name.txt"), 100);
        fs::hard_link(dir.join("big/a.bin"), dir.join("link.bin")).unwrap();
        let clone = dir.join("big/clone.bin");
        let cloned = std::process::Command::new("cp")
            .arg("-c")
            .arg(dir.join("big/nested/b.bin"))
            .arg(&clone)
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if !cloned {
            // Not on APFS: a plain copy is counted the same way.
            fs::copy(dir.join("big/nested/b.bin"), &clone).unwrap();
        }

        let alloc = |p: &str| fs::symlink_metadata(dir.join(p)).unwrap().blocks() * 512;
        // The hard link and the file it links to are one file on disk.
        let file_bytes = alloc("big/a.bin") + alloc("big/nested/b.bin") + alloc("small \"quoted\"\\name.txt") + alloc("big/clone.bin");

        let out = run(Command::Scan { path: dir.clone(), json: true, depth: 10, top: 100 }).unwrap();
        assert!(out.starts_with("{\n  \"schema_version\": 1,"), "{out}");
        assert!(out.contains(r#""name": "small \"quoted\"\\name.txt""#), "{out}");

        let tree = scan::scan(&dir, &Progress::default());
        let json = scan_json(&tree, 10, 100);
        let total = json.get("size_bytes").and_then(Json::as_u64).unwrap();
        assert!(out.contains(&format!("\"size_bytes\": {total},")));
        assert_eq!(json.get("files").and_then(Json::as_u64), Some(5), "both names of the hard link are files");
        assert_eq!(json.get("folders").and_then(Json::as_u64), Some(2));
        assert_eq!(json.get("root").and_then(Json::as_str), Some(dir.to_str().unwrap()));
        let root = json.get("tree").unwrap();
        assert_eq!(check_adds_up(root), 1 + 3 + 3 + 1);

        // Everything not in a file is folders' own allocation, listed as own_bytes.
        fn own_bytes(entry: &Json) -> u64 {
            entry.get("own_bytes").and_then(Json::as_u64).unwrap_or(0)
                + entry.get("children").and_then(Json::as_array).map_or(0, |c| c.iter().map(own_bytes).sum())
        }
        assert_eq!(total, file_bytes + own_bytes(root));
        fs::remove_dir_all(&dir).unwrap();
    }
}
