---
"petal": minor
---

Scan folders and list findings from the command line, with JSON output for scripts and coding agents.

- **`petal scan PATH --json`** prints the folder's size, file and folder counts, unreadable folders and a tree of what's inside. `--depth N` and `--top N` keep the output short: the smaller items in each folder are added up into one "other" entry, so the sizes always add up.
- **`petal findings --json`** scans your home folder (or `petal findings PATH --json`) and prints each finding with its id, whether it's safe to delete or worth a look first, its explanation, its folders, its allocated size and exactly what deleting it frees. Git repositories come with their `git gc` commands instead.
- Without `--json`, both print a short summary. Neither opens a window. The README describes the JSON schema, which has a `schema_version` so scripts can rely on it.
