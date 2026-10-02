#!/usr/bin/env node
// After `changeset version` bumps package.json, carry the new version to everything else
// that names it: Cargo.toml (which scripts/bundle.sh reads for the app's Info.plist),
// Cargo.lock, and the README's download link.
//
//   npm run version     # changeset version && node scripts/sync-version.mjs

import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("..", import.meta.url));
const version = JSON.parse(readFileSync(`${root}package.json`, "utf8")).version;

/** Replace every match of `pattern` in `file`, failing loudly if there's none. */
function update(file, pattern, replace) {
  const path = `${root}${file}`;
  const text = readFileSync(path, "utf8");
  if (!pattern.test(text)) {
    console.error(`sync-version: nothing to update in ${file} (${pattern}); has its format changed?`);
    process.exit(1);
  }
  writeFileSync(path, text.replace(pattern, replace));
}

// The [package] version: the first `version = "…"` line, before any dependency tables.
update("Cargo.toml", /^version = "[^"]*"/m, `version = "${version}"`);
update("Cargo.lock", /(\[\[package\]\]\nname = "petal"\nversion = )"[^"]*"/, `$1"${version}"`);
update("README.md", /Petal \d+\.\d+\.\d+( \(DMG)/g, `Petal ${version}$1`);
update("README.md", /Petal-\d+\.\d+\.\d+\.dmg/g, `Petal-${version}.dmg`);

console.log(`sync-version: Cargo.toml, Cargo.lock and README.md now say ${version}`);
