#!/usr/bin/env python3
"""Build a fictional home folder ("alex") for screenshots and demo recordings of Petal.

Sizes are real allocations (zero-filled files), so Petal shows true numbers.
Usage: scripts/make-demo-home.py <dir>   -> creates <dir>/alex (about 7.5 GB, 87k items)
"""
import os
import random
import sys

MB = 1 << 20
rng = random.Random(7)
CHUNK = b"\0" * MB


def big(path, mb):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as f:
        whole = int(mb)
        for _ in range(whole):
            f.write(CHUNK)
        f.write(b"\0" * int((mb - whole) * MB))


def small(path, kb=None):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    size = int((kb if kb is not None else rng.choice([1, 2, 3, 6, 9, 14, 30])) * 1024 * rng.uniform(0.6, 1.0))
    with open(path, "wb") as f:
        f.write(b"\0" * size)


def many(base, count, exts, depth=2, fanout=8, kb=None):
    """`count` small files spread over a tree of plausible-looking folders."""
    words = ["lib", "src", "dist", "esm", "cjs", "utils", "types", "core", "internal", "helpers", "components", "locale", "build", "test"]
    for i in range(count):
        parts = [rng.choice(words) + ("" if rng.random() < 0.7 else str(rng.randint(1, fanout))) for _ in range(rng.randint(0, depth))]
        name = f"{rng.choice(words)}{i}.{rng.choice(exts)}"
        small(os.path.join(base, *parts, name), kb)


NPM = ["react", "react-dom", "next", "typescript", "lodash", "@babel/core", "@babel/parser", "eslint", "prettier", "webpack",
       "esbuild", "vite", "rollup", "@types/node", "zod", "date-fns", "axios", "tailwindcss", "postcss", "sharp", "jest",
       "@swc/core", "framer-motion", "three", "chart.js", "rxjs", "graphql", "prisma", "@prisma/client", "ws"]


def node_modules(project, packages, files_each):
    for pkg in packages:
        many(os.path.join(project, "node_modules", pkg), files_each + rng.randint(-files_each // 3, files_each // 3), ["js", "d.ts", "json", "map", "cjs"])
    for pkg in rng.sample(packages, min(3, len(packages))):
        big(os.path.join(project, "node_modules", pkg, "bin", f"{pkg.split('/')[-1]}.node"), rng.uniform(8, 30))


def main():
    root = os.path.join(sys.argv[1], "alex")
    j = lambda *p: os.path.join(root, *p)

    # Trash
    big(j(".Trash", "old-portfolio-site.zip"), 410)
    big(j(".Trash", "Screen Recording 2026-08-14.mov"), 230)
    # Downloads
    big(j("Downloads", "Blender-4.5-arm64.dmg"), 310)
    big(j("Downloads", "Figma.dmg"), 185)
    big(j("Downloads", "Android Studio.dmg"), 270)
    big(j("Downloads", "dataset-2026.csv.zip"), 95)
    for i in range(40):
        small(j("Downloads", f"Invoice-{1040 + i}.pdf"), rng.uniform(60, 400))
    # Movies & pictures & music
    big(j("Movies", "Lisbon trip.mov"), 520)
    big(j("Movies", "Product demo (final).mp4"), 160)
    big(j("Movies", "Final Cut Projects", "Lisbon.fcpbundle", "Render Files", "render-0001.mov"), 140)
    for i in range(140):
        big(j("Pictures", "Photos Library.photoslibrary", "originals", f"{i % 16:X}", f"IMG_{4100 + i}.HEIC"), rng.uniform(1.6, 3.6))
    many(j("Pictures", "Photos Library.photoslibrary", "resources", "derivatives"), 1800, ["jpg"], depth=1, kb=40)
    for i in range(60):
        big(j("Music", "Music", "Media", f"Album {i // 12 + 1}", f"Track {i % 12 + 1:02}.m4a"), rng.uniform(4, 9))
    # Documents, Desktop
    for i in range(300):
        small(j("Documents", rng.choice(["Taxes", "Writing", "Receipts", "Notes", "Design"]), f"doc-{i}.{rng.choice(['pdf', 'pages', 'docx', 'key', 'md'])}"), rng.uniform(20, 900))
    for i in range(25):
        small(j("Desktop", f"Screenshot 2026-09-{i + 1:02} at 10.{i:02}.png"), rng.uniform(300, 2200))

    # Developer: projects with node_modules
    node_modules(j("Projects", "web-app"), NPM, 260)
    node_modules(j("Projects", "marketing-site"), rng.sample(NPM, 18), 220)
    node_modules(j("Projects", "api-server"), rng.sample(NPM, 12), 200)
    many(j("Projects", "web-app", "src"), 600, ["ts", "tsx", "css"])
    many(j("Projects", "api-server", "src"), 300, ["ts"])
    many(j("Projects", "ios-weather", "Weather"), 250, ["swift"])
    many(j("Projects", "web-app", ".git", "objects"), 2500, ["pack", "idx"], depth=1)
    big(j("Projects", "web-app", ".git", "objects", "pack", "pack-3f9a.pack"), 140)

    # Xcode & simulators
    for app, mb in [("Weather-bxhqnwzk", 620), ("Petal-cfjdyrmv", 380), ("Sketchbook-aqltwpse", 210)]:
        base = j("Library", "Developer", "Xcode", "DerivedData", app)
        many(os.path.join(base, "Build", "Intermediates.noindex"), 3500, ["o", "swiftmodule", "d", "dia"], depth=3)
        big(os.path.join(base, "Index.noindex", "DataStore", "records.db"), mb * 0.4)
        big(os.path.join(base, "Build", "Products", "Debug-iphonesimulator", "App.app", "App"), mb * 0.6)
    big(j("Library", "Developer", "Xcode", "iOS DeviceSupport", "iPhone17,1 26.0 (23A341)", "Symbols", "dyld_shared_cache.symbols"), 540)
    for udid, mb in [("3F2A9C1E-77B0-4E2B-9D8A-6C1F0B2E4A11", 420), ("A81D4C90-2B3F-4F6A-8E15-C3D9B7A1E5F2", 290)]:
        big(j("Library", "Developer", "CoreSimulator", "Devices", udid, "data", "Library", "dyld.cache"), mb)
        many(j("Library", "Developer", "CoreSimulator", "Devices", udid, "data", "Containers"), 1500, ["plist", "db", "png"])

    # Caches and app data
    big(j("Library", "Caches", "com.spotify.client", "Data", "storage.bnk"), 360)
    many(j("Library", "Caches", "Google", "Chrome", "Default", "Cache"), 4000, ["data"], depth=1, kb=30)
    many(j("Library", "Caches", "Homebrew", "downloads"), 120, ["tar.gz"], depth=0, kb=900)
    many(j("Library", "Caches", "com.apple.Safari", "WebKitCache"), 2000, ["blob"], depth=1, kb=12)
    big(j("Library", "Caches", "go-build", "trim.txt"), 1)
    many(j("Library", "Caches", "go-build"), 3000, ["a", "d"], depth=1, kb=24)
    many(j("Library", "Application Support", "Slack", "IndexedDB"), 900, ["ldb", "log"], depth=1, kb=60)
    many(j("Library", "Application Support", "Code", "User", "workspaceStorage"), 1200, ["json", "vscdb"], depth=2, kb=20)
    big(j("Library", "Application Support", "Code", "CachedExtensionVSIXs", "rust-analyzer.vsix"), 70)
    many(j("Library", "Mail", "V10"), 2600, ["emlx"], depth=2, kb=25)
    many(j("Library", "Containers", "com.apple.Notes", "Data"), 700, ["db", "png"], depth=2, kb=20)
    many(j("Library", "Preferences"), 400, ["plist"], depth=0, kb=4)

    # Package-manager caches
    many(j(".npm", "_cacache", "content-v2", "sha512"), 9000, ["0"], depth=2, kb=26)
    many(j(".cargo", "registry", "src", "index.crates.io-6f17d22bba15001f"), 6000, ["rs", "toml"], depth=3, kb=14)
    many(j(".gradle", "caches", "modules-2", "files-2.1"), 1200, ["jar", "pom"], depth=2, kb=120)
    small(j(".zshrc"), 2)
    small(j(".gitconfig"), 1)
    print(root)


main()
