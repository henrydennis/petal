---
"petal": minor
---

Scan folders and list findings from the command line, with JSON output for scripts and coding agents.

- **`petal scan PATH --json`** prints the folder's size, file and folder counts, unreadable folders and a tree of what's inside. `--depth N` and `--top N` keep the output short: the smaller items in each folder are added up into one "other" entry, so the sizes always add up.
- **`petal findings --json`** scans your home folder (or `petal findings PATH --json`) and prints each finding with its id, whether it's safe to delete, worth a look first or managed in its app, its explanation, its folders, its allocated size and, for what can go in the Trash, exactly what deleting it frees. Findings cleared another way say so: Git repositories come with their `git gc` commands, Homebrew downloads, the pnpm store and Docker's disk image with the command that clears them, and "Manage in app" findings such as your Claude Code history with nothing to delete.
- Without `--json`, both print a short summary. Neither opens a window. The README describes the JSON schema, which has a `schema_version` so scripts can rely on it.
