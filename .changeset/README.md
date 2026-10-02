# Changesets

Each change worth a line in the release notes gets a changeset: a small Markdown file here
saying how big the change is (patch, minor or major) and what changed, for users.

```sh
npx changeset                                   # answer the prompts
npx changeset -m "What changed" --minor petal   # or in one go
```

On `main`, the "Changesets" workflow keeps a "Version Packages" pull request up to date. Merging
it bumps the version (in `package.json`, `Cargo.toml`, `Cargo.lock` and the README's download
link, via `scripts/sync-version.mjs`) and writes `CHANGELOG.md`. Then build, notarise and upload
the DMG for that version as before (`scripts/bundle.sh --dmg`).
