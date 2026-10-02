//! Turns a path (plus what kind of item it is) into hashed sparse features.
//!
//! The model only ever sees these features, so everything it can learn is here: which
//! folder names appear and where (the last component, its parent, further up), the words
//! inside those names, neighbouring pairs, the file extension, and a few shapes such as
//! bundle IDs and version numbers.

pub const BITS: u32 = 16;
pub const BUCKETS: usize = 1 << BITS;
/// Bump whenever features change, so an old model file is rejected instead of misread.
pub const VERSION: u16 = 1;

/// Feature indices for one item. Duplicates are fine; they add up.
pub fn extract(path: &str, is_file: bool) -> Vec<u32> {
    let mut out = Vec::with_capacity(96);
    let mut add = |s: &str| out.push(hash(s));

    add("bias");
    add(if is_file { "kind:file" } else { "kind:folder" });

    let comps = components(path);
    let n = comps.len();
    if n == 0 {
        return out;
    }

    // Where the item lives: the first few components from the root.
    for depth in 1..=n.min(4) {
        add(&format!("pre{depth}:{}", comps[..depth].join("/")));
    }

    for (i, comp) in comps.iter().enumerate() {
        let from_end = n - 1 - i;
        let role = match from_end {
            0 => "last",
            1 => "parent",
            _ => "anc",
        };
        let shape = shape(comp);
        add(&format!("{role}:{shape}"));
        if shape == "word" {
            add(&format!("{role}:{comp}"));
            add(&format!("any:{comp}"));
        }
        for word in words(comp) {
            add(&format!("{role}w:{word}"));
            add(&format!("w:{word}"));
        }
        if i + 1 < n {
            add(&format!("bi:{}/{}", norm(comp), norm(&comps[i + 1])));
        }
    }

    // Character trigrams of the item's own name catch near-misses like "cachedata",
    // "gpucache" or "shadercache" that no word list will cover.
    let last = &comps[n - 1];
    let padded = format!("^{last}$");
    let chars: Vec<char> = padded.chars().collect();
    for w in chars.windows(3) {
        add(&format!("tri:{}", w.iter().collect::<String>()));
    }

    if is_file
        && let Some((_, ext)) = last.rsplit_once('.')
        && !ext.is_empty()
        && ext.len() <= 8
    {
        add(&format!("ext:{ext}"));
    }
    if let Some((_, ext)) = last.rsplit_once('.')
        && matches!(ext, "app" | "photoslibrary" | "pvm" | "utm" | "vmwarevm" | "mbox" | "xcarchive" | "musiclibrary" | "fcpbundle")
    {
        add(&format!("bundle:{ext}"));
    }

    // A pair of features for "inside a project": a build folder means little on its own
    // (`~/Documents/build` is someone's file), but a lot two levels under ~/code.
    if n >= 3 && matches!(comps[1].as_str(), "code" | "src" | "dev" | "projects" | "repos" | "work" | "git" | "github" | "developer") {
        add("in:projects");
        add(&format!("proj:{}", norm(last)));
        if n >= 4 {
            add(&format!("projparent:{}", norm(&comps[n - 2])));
        }
    }

    out
}

/// Lowercased components, with the home folder written as `~`. The startup disk's Data
/// volume is where Petal scans it, but it's the same place as `/`.
pub fn components(path: &str) -> Vec<String> {
    let lower = path.to_lowercase();
    let mut parts: Vec<&str> = lower.split('/').filter(|s| !s.is_empty()).collect();
    if parts.starts_with(&["system", "volumes", "data"]) {
        parts.drain(..3);
    }
    let mut comps = Vec::new();
    if parts.first() == Some(&"~") {
        comps.push("~".to_string());
        parts.remove(0);
    } else if parts.len() >= 2 && parts[0] == "users" {
        comps.push("~".to_string());
        parts.drain(..2);
    } else {
        comps.push("/".to_string());
    }
    comps.extend(parts.into_iter().map(String::from));
    comps
}

/// Splits a name into lowercase words on punctuation, spaces and digits.
fn words(comp: &str) -> Vec<String> {
    comp.split(|c: char| !c.is_alphabetic())
        .filter(|w| w.len() >= 2)
        .map(String::from)
        .collect()
}

/// A name with its variable parts (hashes, version numbers, UUIDs) blanked out, so
/// `Runner-abcdefgh` and `App-ijklmnop` share features.
fn norm(comp: &str) -> String {
    match shape(comp) {
        "word" => comp.to_string(),
        s => format!("<{s}>"),
    }
}

fn shape(comp: &str) -> &'static str {
    let alnum: String = comp.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    let digits = alnum.chars().filter(|c| c.is_ascii_digit()).count();
    if comp.len() >= 32 && digits * 4 >= alnum.len() && alnum.chars().all(|c| c.is_ascii_hexdigit()) {
        return "uuid";
    }
    if comp.starts_with("com.") || comp.starts_with("org.") || comp.starts_with("io.") || comp.starts_with("net.") || comp.starts_with("group.") {
        return "bundleid";
    }
    if !alnum.is_empty() && digits * 2 >= alnum.len() {
        return "num";
    }
    if let Some((_, tail)) = comp.rsplit_once('-')
        && tail.len() >= 20
        && tail.chars().all(|c| c.is_ascii_lowercase())
    {
        return "derived";
    }
    "word"
}

/// FNV-1a, folded into `BUCKETS`.
fn hash(s: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in s.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h & (BUCKETS as u32 - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_is_normalised() {
        assert_eq!(components("/Users/sam/Library/Caches"), ["~", "library", "caches"]);
        assert_eq!(components("~/Library/Caches"), ["~", "library", "caches"]);
        assert_eq!(components("/Applications/Xcode.app"), ["/", "applications", "xcode.app"]);
        assert_eq!(components("/System/Volumes/Data/Users/sam/Movies"), ["~", "movies"]);
        assert_eq!(components("/System/Volumes/Data"), ["/"]);
    }

    #[test]
    fn shapes() {
        assert_eq!(shape("com.spotify.client"), "bundleid");
        assert_eq!(shape("17.4 (21e219)"), "num");
        assert_eq!(shape("runner-abcdefghijklmnopqrstuv"), "derived");
        assert_eq!(shape("00008110-001a2b3c4d5e6f70a1b2c3d4"), "uuid");
        assert_eq!(shape("node_modules"), "word");
    }

    #[test]
    fn same_path_two_ways_same_features() {
        assert_eq!(extract("/Users/sam/code/x/target", false), extract("~/code/x/target", false));
    }
}
