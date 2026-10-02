//! Made-up, labelled paths for training and testing. Every label comes from the template
//! that produced the path; the names filling the templates (apps, projects, documents,
//! games...) are split so the test set only uses names the model never saw in training.
//! Names that appear in the hand-labelled set (`testset.rs`) are always held out.

use crate::model::Category::{self, *};

pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1)
    }
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
    fn chance(&mut self, p: f64) -> bool {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64 <= p
    }
    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            items.swap(i, self.below(i + 1));
        }
    }
    fn chars(&mut self, alphabet: &[u8], n: usize) -> String {
        (0..n).map(|_| *self.pick(alphabet) as char).collect()
    }
    fn hex(&mut self, n: usize) -> String {
        self.chars(b"0123456789ABCDEF", n)
    }
    fn lower(&mut self, n: usize) -> String {
        self.chars(b"abcdefghijklmnopqrstuvwxyz", n)
    }
    fn uuid(&mut self) -> String {
        format!("{}-{}-{}-{}-{}", self.hex(8), self.hex(4), self.hex(4), self.hex(4), self.hex(12))
    }
    fn year(&mut self) -> usize {
        2015 + self.below(11)
    }
    fn version(&mut self) -> String {
        format!("{}.{}.{}", 1 + self.below(20), self.below(10), self.below(20))
    }
    fn date(&mut self) -> String {
        format!("{}-{:02}-{:02}", self.year(), 1 + self.below(12), 1 + self.below(28))
    }
}

pub struct Item {
    pub path: String,
    pub is_file: bool,
    pub category: Category,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Split {
    Train,
    Test,
}

/// Name lists, already filtered to one side of the split.
pub struct Vocab {
    apps: Vec<(&'static str, &'static str)>,
    projects: Vec<&'static str>,
    games: Vec<&'static str>,
    topics: Vec<&'static str>,
    docs: Vec<&'static str>,
    events: Vec<&'static str>,
    tools: Vec<&'static str>,
}

impl Vocab {
    pub fn new(split: Split) -> Vocab {
        fn keep<T: Copy>(split: Split, items: &[T], name: impl Fn(T) -> &'static str) -> Vec<T> {
            items.iter().copied().filter(|&x| held_out(name(x)) == (split == Split::Test)).collect()
        }
        Vocab {
            apps: keep(split, APPS, |a| a.0),
            projects: keep(split, PROJECTS, |p| p),
            games: keep(split, GAMES, |g| g),
            topics: keep(split, TOPICS, |t| t),
            docs: keep(split, DOCS, |d| d),
            events: keep(split, EVENTS, |e| e),
            tools: keep(split, CACHE_TOOLS, |t| t),
        }
    }
}

/// About one name in five goes to the test side, plus every name the hand-labelled set
/// mentions, so neither test set rewards memorising names.
fn held_out(name: &str) -> bool {
    let lower = name.to_lowercase();
    let mentioned = crate::testset::PATHS.iter().any(|t| {
        let path = t.0.to_lowercase();
        path.split(['/', '.', ' ']).any(|c| c == lower) || (lower.len() >= 5 && path.contains(&lower))
    });
    let mut h: u32 = 0x811c_9dc5;
    for b in name.bytes() {
        h = (h ^ b as u32).wrapping_mul(0x0100_0193);
    }
    mentioned || h % 5 == 0
}

pub fn generate(split: Split, count: usize, seed: u64) -> Vec<Item> {
    let vocab = Vocab::new(split);
    let mut rng = Rng::new(seed);
    let total: usize = TEMPLATES.iter().map(|t| t.0).sum();
    (0..count)
        .map(|_| {
            let mut r = rng.below(total);
            let (_, category, make) = TEMPLATES
                .iter()
                .find(|t| {
                    if r < t.0 {
                        true
                    } else {
                        r -= t.0;
                        false
                    }
                })
                .unwrap();
            let (path, is_file) = make(&mut rng, &vocab);
            Item { path, is_file, category: *category }
        })
        .collect()
}

type Make = fn(&mut Rng, &Vocab) -> (String, bool);

fn dir(s: impl Into<String>) -> (String, bool) {
    (s.into(), false)
}

fn file(s: impl Into<String>) -> (String, bool) {
    (s.into(), true)
}

fn project(r: &mut Rng, v: &Vocab) -> String {
    let root = r.pick(&["~/code", "~/src", "~/dev", "~/Projects", "~/Developer", "~/repos", "~/work", "~/git", "~/Documents/GitHub"]);
    let name = r.pick(&v.projects);
    if r.chance(0.25) {
        let sub = r.pick(&["packages/web", "packages/api", "apps/mobile", "frontend", "backend", "ios", "android", "client", "server", "crates/core"]);
        format!("{root}/{name}/{sub}")
    } else {
        format!("{root}/{name}")
    }
}

fn app_support(r: &mut Rng, v: &Vocab) -> String {
    format!("~/Library/Application Support/{}", r.pick(&v.apps).0)
}

/// (weight, category, make)
const TEMPLATES: &[(usize, Category, Make)] = &[
    // ---- Caches & logs
    (6, Caches, |r, v| dir(format!("~/Library/Caches/{}", r.pick(&v.apps).1))),
    (3, Caches, |r, v| dir(format!("~/Library/Caches/{}", r.pick(&v.apps).0))),
    (4, Caches, |r, v| dir(format!("~/Library/Caches/{}", r.pick(&v.tools)))),
    (1, Caches, |r, _| dir(*r.pick(&["~/Library/Caches", "~/.cache", "~/Library/Logs", "/Library/Caches", "/Library/Logs", "/private/var/log"]))),
    (8, Caches, |r, v| {
        let cache = r.pick(&["Cache", "Code Cache", "GPUCache", "DawnCache", "DawnGraphiteCache", "ShaderCache", "GrShaderCache", "CachedData", "Crashpad/completed", "logs", "Service Worker/CacheStorage", "Service Worker/ScriptCache", "component_crx_cache", "Cache/Cache_Data", "media-cache"]);
        dir(format!("{}/{cache}", app_support(r, v)))
    }),
    (3, Caches, |r, _| {
        let browser = r.pick(&["Google/Chrome", "BraveSoftware/Brave-Browser", "Microsoft Edge", "Vivaldi", "Arc/User Data"]);
        let profile = r.pick(&["Default", "Profile 1", "Profile 2", "Guest Profile"]);
        let cache = r.pick(&["Cache", "Code Cache", "GPUCache", "Service Worker/CacheStorage", "Service Worker/ScriptCache"]);
        dir(format!("~/Library/Application Support/{browser}/{profile}/{cache}"))
    }),
    (3, Caches, |r, v| dir(format!("~/Library/Containers/{}/Data/Library/Caches", r.pick(&v.apps).1))),
    (3, Caches, |r, v| {
        let (name, bundle) = r.pick(&v.apps);
        dir(format!("~/Library/Logs/{}", if r.chance(0.5) { name } else { bundle }))
    }),
    (2, Caches, |r, _| dir(format!("~/Library/Logs/{}", r.pick(&["DiagnosticReports", "CoreSimulator", "JetBrains", "Homebrew", "zoom.us"])))),
    (3, Caches, |r, v| {
        let kind = r.pick(&["C", "T", "X"]);
        dir(format!("/private/var/folders/{}/{}{}/{kind}/{}", r.lower(2), r.lower(6), r.below(10_000), r.pick(&v.apps).1))
    }),
    (1, Caches, |r, v| dir(format!("/Library/Caches/{}", r.pick(&v.apps).1))),

    // ---- Developer files: code, build output, packages, SDKs, simulators, models
    (12, Developer, |r, v| {
        let build = r.pick(&["node_modules", "target", "build", "dist", ".next", ".nuxt", ".svelte-kit", ".venv", "venv", "__pycache__", ".pytest_cache", ".mypy_cache", ".ruff_cache", ".tox", ".gradle", "Pods", ".build", "DerivedData", ".parcel-cache", ".turbo", ".angular/cache", "coverage", "cmake-build-debug", "cmake-build-release", ".dart_tool", ".expo", "vendor/bundle", ".terraform", ".zig-cache", "_build", "elm-stuff", "bower_components", ".stack-work", "dist-newstyle", "node_modules/.cache", "app/build", "out"]);
        dir(format!("{}/{build}", project(r, v)))
    }),
    (8, Developer, |r, v| {
        let part = r.pick(&["", "/src", "/lib", "/.git", "/assets", "/docs", "/tests", "/public", "/scripts", "/migrations", "/src/cache", "/src/build", "/design", "/Sources", "/app/src/main"]);
        dir(format!("{}{part}", project(r, v)))
    }),
    (2, Developer, |r, v| file(format!("{}/{}", project(r, v), r.pick(&["Cargo.toml", "package.json", "README.md", "build.rs", "Makefile", "build.gradle", "cache.ts", "main.rs"])))),
    (2, Developer, |r, v| {
        let what = r.pick(&["data", "datasets", "checkpoints", "models", "recordings", "fixtures/large", "renders"]);
        dir(format!("{}/{what}", project(r, v)))
    }),
    (1, Developer, |r, _| dir(*r.pick(&["~/code", "~/src", "~/dev", "~/Projects", "~/Developer", "~/repos", "~/Documents/GitHub", "~/go", "~/.local/share/mise"]))),
    (2, Developer, |r, v| dir(format!("~/Library/Developer/Xcode/DerivedData/{}-{}", cap(r.pick(&v.projects)), r.lower(28)))),
    (2, Developer, |r, _| {
        let os = r.pick(&["iOS", "watchOS", "tvOS", "visionOS"]);
        dir(format!("~/Library/Developer/Xcode/{os} DeviceSupport/{}.{} ({}{}{})", 15 + r.below(4), r.below(6), 19 + r.below(4), r.chars(b"ABCDEFG", 1), 100 + r.below(400)))
    }),
    (2, Developer, |r, _| {
        dir(*r.pick(&[
            "~/Library/Developer", "~/Library/Developer/Xcode", "~/Library/Developer/Xcode/DerivedData", "~/Library/Developer/CoreSimulator",
            "~/Library/Developer/CoreSimulator/Caches", "/Library/Developer/CommandLineTools", "/opt/homebrew/Cellar", "/opt/homebrew/lib",
            "/usr/local/Cellar", "/Library/Developer/CoreSimulator/Volumes",
        ]))
    }),
    (8, Developer, |r, _| {
        dir(*r.pick(&[
            "~/.npm/_cacache", "~/.npm", "~/.cache/pip", "~/.cache/yarn", "~/.cache/pre-commit", "~/.cache/bazel", "~/.cache/go-build",
            "~/.cache/uv", "~/.cache/puppeteer", "~/.cache/node-gyp", "~/.cache/typescript", "~/.cache/deno", "~/.cargo/registry",
            "~/.cargo/registry/cache", "~/.cargo/registry/src", "~/.cargo/git", "~/.gradle/caches", "~/.gradle/wrapper/dists",
            "~/.m2/repository", "~/go/pkg/mod", "~/.pub-cache", "~/.cocoapods/repos", "~/Library/pnpm/store", "~/.bun/install/cache",
            "~/.yarn/berry/cache", "~/.nuget/packages", "~/.ivy2/cache", "~/.cache/bun", "~/.cache/zig", "~/.cache/ccache", "~/.cargo",
            "~/.rustup", "~/.gradle", "~/.m2", "~/.bun", "~/.pyenv/versions", "~/.nvm/versions/node", "~/miniconda3", "~/anaconda3/pkgs",
        ]))
    }),
    (2, Developer, |r, v| {
        let date = r.date();
        if r.chance(0.5) {
            dir(format!("~/Library/Developer/Xcode/Archives/{date}"))
        } else {
            dir(format!("~/Library/Developer/Xcode/Archives/{date}/{} {date}.xcarchive", cap(r.pick(&v.projects))))
        }
    }),
    (2, Developer, |r, _| {
        if r.chance(0.3) {
            dir("~/Library/Developer/CoreSimulator/Devices")
        } else {
            dir(format!("~/Library/Developer/CoreSimulator/Devices/{}", r.uuid()))
        }
    }),
    (2, Developer, |r, _| match r.below(3) {
        0 => dir("~/Library/Containers/com.docker.docker"),
        1 => dir("~/Library/Containers/com.docker.docker/Data/vms/0"),
        _ => file("~/Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw"),
    }),
    (4, Developer, |r, _| {
        let model = r.pick(&["meta-llama--Llama-3.1-8B", "mistralai--Mistral-7B-v0.3", "Qwen--Qwen2.5-7B", "google--gemma-2-9b", "openai--whisper-large-v3", "stabilityai--stable-diffusion-xl-base-1.0", "black-forest-labs--FLUX.1-dev"]);
        dir(match r.below(5) {
            0 => "~/.ollama/models".to_string(),
            1 => format!("~/.cache/huggingface/hub/models--{model}"),
            2 => format!("~/.cache/lm-studio/models/{}", model.replace("--", "/")),
            3 => "~/.cache/torch/hub".to_string(),
            _ => format!("~/.lmstudio/models/{}", model.replace("--", "/")),
        })
    }),
    (2, Developer, |r, _| {
        dir(match r.below(3) {
            0 => format!("~/.rustup/toolchains/{}-aarch64-apple-darwin", r.pick(&["stable", "nightly", "1.80.0", "1.85.1"])),
            1 => format!("~/Library/Android/sdk/system-images/android-{}", 28 + r.below(8)),
            _ => format!("~/.android/avd/Pixel_{}_API_{}.avd", 4 + r.below(5), 28 + r.below(8)),
        })
    }),

    // ---- Downloads & Trash
    (2, Downloads, |r, _| dir(*r.pick(&["~/Downloads", "~/.Trash"]))),
    (6, Downloads, |r, v| {
        let (name, _) = r.pick(&v.apps);
        let ext = r.pick(&["dmg", "pkg", "zip", "xip", "iso", "tar.gz", "mp4", "mov", "zip"]);
        file(format!("~/Downloads/{}-{}.{ext}", name.replace(' ', ""), r.version()))
    }),
    (3, Downloads, |r, v| file(format!("~/Downloads/{}.{}", doc_name(r, v), r.pick(&["zip", "pdf", "mp4", "csv", "tar.gz", "jpg", "heic", "docx"])))),
    (2, Downloads, |r, v| dir(format!("~/Downloads/{}", match r.below(3) {
        0 => doc_name(r, v),
        1 => format!("{}-{}", r.pick(&v.apps).0, r.version()),
        _ => r.pick(&v.topics).to_string(),
    }))),
    (3, Downloads, |r, v| match r.below(3) {
        0 => file(format!("~/.Trash/{}.{}", doc_name(r, v), r.pick(&["pdf", "zip", "dmg", "mov"]))),
        1 => dir(format!("~/.Trash/{}", r.pick(&v.projects))),
        _ => file(format!("~/.Trash/{}-{}.dmg", r.pick(&v.apps).0.replace(' ', ""), r.version())),
    }),

    // ---- Apps and games
    (5, Apps, |r, v| {
        let (name, _) = r.pick(&v.apps);
        dir(match r.below(4) {
            0 => format!("~/Applications/{name}.app"),
            1 => format!("/Applications/{name}.app/Contents"),
            _ => format!("/Applications/{name}.app"),
        })
    }),
    (4, Apps, |r, v| {
        let game = r.pick(&v.games);
        dir(match r.below(3) {
            0 => format!("~/Library/Application Support/Steam/steamapps/common/{game}"),
            1 => format!("/Applications/{game}.app"),
            _ => format!("~/Library/Application Support/Epic/{game}"),
        })
    }),
    (1, Apps, |r, _| dir(*r.pick(&["/Applications", "~/Applications", "/Applications/Utilities", "~/Library/Application Support/Steam/steamapps", "~/Applications/Chrome Apps.localized"]))),

    // ---- Photos, video & music
    (2, Media, |r, _| dir(*r.pick(&["~/Pictures/Photos Library.photoslibrary", "~/Pictures", "~/Movies", "~/Music", "~/Music/Music/Music Library.musiclibrary", "~/Music/Music/Media", "~/Pictures/Photos Library.photoslibrary/resources/derivatives"]))),
    (4, Media, |r, v| {
        let event = r.pick(&v.events);
        let year = r.year();
        dir(match r.below(4) {
            0 => format!("~/Pictures/{event} {year}"),
            1 => format!("~/Movies/{event} {year}"),
            2 => format!("~/Movies/{event} {year} RAW"),
            _ => format!("~/Music/{event}"),
        })
    }),
    (2, Media, |r, v| {
        let event = r.pick(&v.events);
        file(match r.below(4) {
            0 => format!("~/Pictures/{event} {}/IMG_{}.HEIC", r.year(), 1000 + r.below(9000)),
            1 => format!("~/Movies/{event}.mov"),
            2 => format!("~/Music/{event}/{:02} Track.m4a", 1 + r.below(15)),
            _ => format!("~/Movies/Screen Recording {}.mov", r.date()),
        })
    }),
    (2, Media, |r, v| dir(format!("~/Music/{}/{}", r.pick(&["Logic", "GarageBand", "Ableton", "Projects", "Audio Music Apps"]), r.pick(&v.events)))),
    (2, Media, |r, v| {
        let event = r.pick(&v.events);
        dir(match r.below(3) {
            0 => format!("~/Movies/{event}.fcpbundle/{event}/Render Files"),
            1 => format!("~/Movies/{event}.fcpbundle"),
            _ => "~/Desktop/Screenshots".to_string(),
        })
    }),
    (1, Media, |r, _| dir(*r.pick(&["/Library/Audio/Apple Loops", "/Library/Application Support/Logic", "/Library/Audio/Impulse Responses", "~/Music/Audio Music Apps"]))),

    // ---- Documents
    (8, Documents, |r, v| {
        let topic = r.pick(&v.topics);
        match r.below(3) {
            0 => dir(format!("~/Documents/{topic}")),
            1 => dir(format!("~/Documents/{topic}/{}", r.year())),
            _ => file(format!("~/Documents/{topic}/{}.{}", doc_name(r, v), r.pick(&["pdf", "docx", "pages", "key", "xlsx", "numbers", "txt", "md"]))),
        }
    }),
    (3, Documents, |r, v| match r.below(3) {
        0 => file(format!("~/Desktop/{}.{}", doc_name(r, v), r.pick(&["pdf", "docx", "pages", "key"]))),
        1 => dir(format!("~/Desktop/{}", r.pick(&v.topics))),
        _ => dir(format!("~/Library/Mobile Documents/com~apple~CloudDocs/{}", r.pick(&v.topics))),
    }),
    (1, Documents, |r, _| dir(*r.pick(&["~/Documents", "~/Desktop", "~/Library/Mobile Documents/com~apple~CloudDocs", "~/Library/CloudStorage/Dropbox", "~/Library/CloudStorage/GoogleDrive-me@example.com/My Drive"]))),

    // ---- App data, settings and backups
    (5, AppData, |r, v| {
        let data = r.pick(&["", "/IndexedDB", "/Local Storage", "/databases", "/Session Storage", "/User Data", "/Backups", "/profiles", "/Local Storage/leveldb", "/storage", "/data"]);
        dir(format!("{}{data}", app_support(r, v)))
    }),
    (2, AppData, |r, _| dir(*r.pick(&["~/Library/Application Support/Google/Chrome/Default", "~/Library/Application Support/Firefox/Profiles", "~/Library/Application Support/1Password", "~/Library/Group Containers/2BUA8C4S2C.com.1password"]))),
    (3, AppData, |r, v| {
        let (_, bundle) = r.pick(&v.apps);
        dir(match r.below(3) {
            0 => format!("~/Library/Containers/{bundle}"),
            1 => format!("~/Library/Containers/{bundle}/Data/Documents"),
            _ => format!("~/Library/Group Containers/group.{bundle}"),
        })
    }),
    (6, AppData, |r, _| {
        dir(*r.pick(&[
            "~/Library/Messages", "~/Library/Messages/Attachments", "~/Library/Mail/V10/MailData", "~/Library/Keychains", "~/Library/Preferences",
            "~/Library/Containers/com.apple.Notes", "~/Library/Group Containers/group.com.apple.notes", "~/Library/Application Support/AddressBook",
            "~/Library/Calendars", "~/Library/Application Support/CallHistoryDB", "~/Library/Application Support/Knowledge", "~/Library/Safari",
            "~/Library/Containers/com.apple.mail", "~/Library/Accounts", "~/.ssh", "~/.gnupg", "~/.config", "~/Library/Mail",
            "~/Library/Application Support/MobileSync", "~/Library/Application Support/MobileSync/Backup", "~/Library/Containers/com.apple.Safari",
        ]))
    }),
    (2, AppData, |r, _| file(*r.pick(&["~/Library/Messages/chat.db", "~/.zshrc", "~/.gitconfig", "~/Library/Keychains/login.keychain-db"]))),
    (1, AppData, |r, v| file(format!("~/Library/Preferences/{}.plist", r.pick(&v.apps).1))),
    (2, AppData, |r, _| dir(format!("~/Library/Application Support/MobileSync/Backup/{}-{}", r.hex(8), r.hex(16)))),
    (1, AppData, |r, _| dir(format!("~/Library/Mail/V10/{}/{}.mbox", r.uuid(), r.pick(&["INBOX", "Sent Messages", "Archive", "[Gmail]/All Mail"])))),
    (4, AppData, |r, _| {
        let os = r.pick(&["Windows 11", "Windows 10", "Ubuntu 24.04", "Ubuntu 22.04", "Debian 12", "Fedora 40", "macOS Sonoma", "Kali Linux"]);
        dir(match r.below(4) {
            0 => format!("~/Parallels/{os}.pvm"),
            1 => format!("~/Library/Containers/com.utmapp.UTM/Data/Documents/{os}.utm"),
            2 => format!("~/Virtual Machines.localized/{os}.vmwarevm"),
            _ => format!("~/VirtualBox VMs/{os}"),
        })
    }),

    // ---- System
    (5, System, |r, _| {
        dir(*r.pick(&[
            "/System", "/System/Library", "/Library", "/Library/Application Support", "/Library/Frameworks", "/Library/Updates",
            "/Library/Apple", "/Library/Fonts", "/Library/Extensions", "/Library/Printers", "/Library/Keychains", "/Library/Preferences",
            "/private/var/db", "/private/var/vm", "/private/var/protected", "/private/var/root", "/private/var/db/uuidtext",
            "/private/var/db/diagnostics", "/usr", "/usr/libexec", "/usr/standalone", "/bin", "/sbin", "/cores", "/Library/Application Support/Apple",
            "/Library/Application Support/com.apple.TCC", "/private/var/db/dyld", "/private/var/MobileSoftwareUpdate",
        ]))
    }),
    (1, System, |r, v| dir(format!("/Library/Application Support/{}", r.pick(&v.apps).0))),

    // ---- Mixed: folders that hold a bit of everything
    (5, Mixed, |r, v| {
        dir(match r.below(12) {
            0 => "/".to_string(),
            1 => "/Users".to_string(),
            2 => format!("/Users/{}", r.pick(&["sam", "alex.kim", "jo", "admin", "rlee", "maria"])),
            3 => format!("/Users/{}/Library", r.pick(&["sam", "alex.kim", "jo", "admin", "rlee", "maria"])),
            4 => "~/Library/Application Support".to_string(),
            5 => "~/Library/Containers".to_string(),
            6 => "~/Library/Group Containers".to_string(),
            7 => "/Users/Shared".to_string(),
            8 => "/private".to_string(),
            9 => "/private/var".to_string(),
            10 => format!("/Volumes/{}", r.pick(&["External", "Samsung T7", "Untitled", "Time Machine", "SanDisk", "Media", "Work SSD"])),
            _ => format!("/Volumes/{}/{}", r.pick(&["External", "Samsung T7", "Untitled", "SanDisk", "Work SSD"]), r.pick(&v.topics)),
        })
    }),
    (1, Mixed, |r, _| dir(*r.pick(&["~/Library", "~/.local", "~/Library/Application Support/Google", "~/Library/Application Support/Microsoft"]))),
];

fn doc_name(r: &mut Rng, v: &Vocab) -> String {
    let doc = r.pick(&v.docs);
    match r.below(3) {
        0 => doc.to_string(),
        1 => format!("{doc}-{}", r.year()),
        _ => format!("{doc} final v{}", 1 + r.below(5)),
    }
}

fn cap(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

const APPS: &[(&str, &str)] = &[
    ("Slack", "com.tinyspeck.slackmacgap"), ("Discord", "com.hnc.Discord"), ("Spotify", "com.spotify.client"), ("zoom.us", "us.zoom.xos"),
    ("Figma", "com.figma.Desktop"), ("Notion", "notion.id"), ("Code", "com.microsoft.VSCode"), ("Cursor", "com.todesktop.230313mzl4w4u92"),
    ("Obsidian", "md.obsidian"), ("Signal", "org.whispersystems.signal-desktop"), ("WhatsApp", "net.whatsapp.WhatsApp"),
    ("Telegram", "ru.keepcoder.Telegram"), ("Microsoft Teams", "com.microsoft.teams2"), ("Postman", "com.postmanlabs.mac"),
    ("Firefox", "org.mozilla.firefox"), ("Brave", "com.brave.Browser"), ("Arc", "company.thebrowser.Browser"), ("Linear", "com.linear"),
    ("Loom", "com.loom.desktop"), ("Raycast", "com.raycast.macos"), ("Dropbox", "com.getdropbox.dropbox"), ("Microsoft Word", "com.microsoft.Word"),
    ("Microsoft Excel", "com.microsoft.Excel"), ("Microsoft Outlook", "com.microsoft.Outlook"), ("Steam", "com.valvesoftware.steam"),
    ("Blender", "org.blenderfoundation.blender"), ("Unity", "com.unity3d.UnityEditor5.x"), ("Android Studio", "com.google.android.studio"),
    ("IntelliJ IDEA", "com.jetbrains.intellij"), ("PyCharm", "com.jetbrains.pycharm"), ("Sketch", "com.bohemiancoding.sketch3"),
    ("Adobe Photoshop", "com.adobe.Photoshop"), ("Adobe Premiere Pro", "com.adobe.PremierePro"), ("DaVinci Resolve", "com.blackmagic-design.DaVinciResolve"),
    ("Kindle", "com.amazon.Lassen"), ("Messenger", "com.facebook.archon"), ("Skype", "com.skype.skype"), ("Webex", "Cisco-Systems.Spark"),
    ("Evernote", "com.evernote.Evernote"), ("Todoist", "com.todoist.mac.Todoist"), ("Bear", "net.shinyfrog.bear"), ("Things", "com.culturedcode.ThingsMac"),
    ("Tower", "com.fournova.Tower3"), ("GitHub Desktop", "com.github.GitHubClient"), ("Insomnia", "com.insomnia.app"), ("TablePlus", "com.tinyapp.TablePlus"),
    ("Warp", "dev.warp.Warp-Stable"), ("iTerm2", "com.googlecode.iterm2"), ("Ghostty", "com.mitchellh.ghostty"), ("Zed", "dev.zed.Zed"),
    ("Sublime Text", "com.sublimetext.4"), ("OBS", "com.obsproject.obs-studio"), ("VLC", "org.videolan.vlc"), ("IINA", "com.colliderli.iina"),
    ("Audacity", "org.audacityteam.audacity"), ("Ableton Live", "com.ableton.live"), ("Affinity Photo", "com.seriflabs.affinityphoto2"),
    ("Canva", "com.canva.CanvaDesktop"), ("ChatGPT", "com.openai.chat"), ("Claude", "com.anthropic.claudefordesktop"), ("Perplexity", "ai.perplexity.mac"),
    ("Microsoft Edge", "com.microsoft.edgemac"), ("Opera", "com.operasoftware.Opera"), ("Vivaldi", "com.vivaldi.Vivaldi"), ("Mattermost", "Mattermost.Desktop"),
    ("Element", "im.riot.app"), ("Miro", "com.electron.realtimeboard"), ("Asana", "com.electron.asana"), ("ClickUp", "com.clickup.desktop-app"),
    ("Superhuman", "com.superhuman.electron"), ("Spark", "com.readdle.SparkDesktop"), ("Mimestream", "com.mimestream.Mimestream"), ("Tidal", "com.tidal.desktop"),
    ("Plex", "tv.plex.desktop"), ("Battle.net", "net.battle.app"), ("Epic Games Launcher", "com.epicgames.EpicGamesLauncher"),
    ("Minecraft", "com.mojang.minecraftlauncher"), ("Roblox", "com.roblox.RobloxPlayer"), ("Parallels Desktop", "com.parallels.desktop.console"),
    ("Logseq", "com.electron.logseq"), ("Anki", "net.ankiweb.dtop"), ("Calibre", "net.kovidgoyal.calibre"), ("Transmission", "org.m0k.transmission"),
    ("Bitwarden", "com.bitwarden.desktop"), ("NordVPN", "com.nordvpn.macos"), ("Grammarly", "com.grammarly.ProjectLlama"), ("DBeaver", "org.jkiss.dbeaver.core.product"),
];

const PROJECTS: &[&str] = &[
    "website", "petal", "api", "backend", "frontend", "dashboard", "blog", "portfolio", "landing-page", "chat-app", "todo", "notes-app", "game",
    "engine", "compiler", "parser", "cli", "bot", "scraper", "ml-experiments", "data-pipeline", "infra", "ios-app", "android-app", "app", "mobile",
    "shop", "admin", "auth-service", "payments", "analytics", "search", "indexer", "crawler", "renderer", "synth", "midi-tool", "raytracer",
    "kernel", "emulator", "wasm-demo", "rust-book", "advent-of-code", "leetcode", "homework", "thesis-code", "research", "sandbox", "playground",
    "scratch", "experiments", "prototype", "monorepo", "design-system", "components", "docs-site", "extension", "plugin", "weather", "budget-tracker",
    "photo-sorter", "invoice-gen", "home-automation", "keyboard-firmware", "discord-bot", "slack-bot", "chess", "tetris", "snake", "pong",
    "recipes", "habit-tracker", "markdown-editor", "static-site", "image-resizer", "video-tools", "tts", "llm-eval", "rag-demo", "agent",
];

const GAMES: &[&str] = &[
    "Baldurs Gate 3", "Cyberpunk 2077", "Stardew Valley", "Hades", "Civilization VI", "Disco Elysium", "Factorio", "Hollow Knight", "Celeste",
    "Dota 2", "Counter-Strike 2", "Death Stranding", "Resident Evil Village", "No Mans Sky", "Terraria", "Portal 2", "Balatro", "Hades II",
    "Lies of P", "Control", "Dave the Diver", "Frostpunk", "Satisfactory", "Rimworld", "Valheim", "Slay the Spire", "Outer Wilds", "Inscryption",
];

const TOPICS: &[&str] = &[
    "Taxes", "Work", "Personal", "School", "Finance", "Health", "Travel", "House", "Car", "Kids", "Recipes", "Writing", "Receipts", "Contracts",
    "Insurance", "Legal", "Clients", "Archive", "University", "Pension", "Mortgage", "Wedding Planning", "Job Applications", "Medical", "Bank Statements",
];

const DOCS: &[&str] = &[
    "invoice", "resume", "cv", "contract", "notes", "report", "budget", "receipt", "letter", "proposal", "plan", "essay", "thesis", "slides",
    "presentation", "lease", "passport-scan", "itinerary", "boarding-pass", "statement", "payslip", "quote", "agenda", "minutes", "manuscript",
    "chapter", "recipe", "cover-letter", "offer-letter", "tax-return", "birth-certificate", "portfolio", "dissertation", "syllabus", "timesheet",
];

const EVENTS: &[&str] = &[
    "Wedding", "Holiday", "Graduation", "Birthday", "Trip to Japan", "Family Reunion", "Recital", "Honeymoon", "Summer", "Christmas",
    "Road Trip", "Baby", "New Year", "Camping", "Skiing", "Lisbon", "Iceland", "Band Practice", "Album Sessions", "Podcast",
];

const CACHE_TOOLS: &[&str] = &[
    "Homebrew", "pip", "Yarn", "pnpm", "ms-playwright", "go-build", "JetBrains", "CocoaPods", "typescript", "node-gyp", "electron", "deno",
    "bazel", "puppeteer", "Cypress", "Google/Chrome", "SiriTTS", "com.apple.Safari", "com.apple.Music", "GeoServices", "pypoetry",
    "org.swift.swiftpm", "com.apple.dt.Xcode", "Mozilla/Firefox", "camoufox", "Arc", "esbuild", "prisma-nodejs", "vscode-cpptools",
];


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_do_not_share_names() {
        let train = Vocab::new(Split::Train);
        let test = Vocab::new(Split::Test);
        assert!(train.apps.iter().all(|a| !test.apps.contains(a)));
        assert!(train.projects.iter().all(|p| !test.projects.contains(p)));
        assert!(!test.apps.is_empty() && !test.projects.is_empty() && !test.games.is_empty());
    }

    #[test]
    fn hand_labelled_names_are_held_out() {
        let train = Vocab::new(Split::Train);
        for name in ["Spotify", "Slack", "Code", "Obsidian", "Claude", "Steam", "Microsoft Excel"] {
            assert!(!train.apps.iter().any(|a| a.0 == name), "{name} leaked into training");
        }
        for name in ["petal", "website", "dashboard", "app", "android-app", "ml-experiments", "scratch", "infra"] {
            assert!(!train.projects.contains(&name), "{name} leaked into training");
        }
    }
}
