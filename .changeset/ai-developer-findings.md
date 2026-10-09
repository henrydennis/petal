---
"petal": minor
---

New findings for AI tools and developers, and a third label, **Manage in app**, for things that belong to an app. These are shown and explained but never offered for the Trash.

- **Local AI models** from Ollama, LM Studio and Hugging Face are marked Review first. They're downloads you'd have to fetch again, and the explanation points to each tool's own way to remove them (`ollama rm <model>`, LM Studio's model list, Hugging Face's `hf cache` commands). For Hugging Face only the `hub` folder is included, because the folder above it also holds your login token.
- **Claude Code and Codex histories** (`~/.claude/projects`, `~/.codex/sessions`) are marked Manage in app. They're your conversations, kept so you can resume them.
- **Cargo `target` folders** are Safe to delete, but only when the folder sits right next to a `Cargo.toml`. Any other folder called `target` isn't reported, and, as with `node_modules`, nothing inside apps, `~/Library` or hidden folders is ever included.
- **Homebrew's downloads** are Safe to delete. Petal copies `brew cleanup --prune=all` for you rather than deleting the folder.
- **The pnpm store** is marked Review first. Your projects' `node_modules` share its files through hard links or clones, so Petal shows only what deleting the store would really free, and copies `pnpm store prune` for you.
- **Docker's disk image** replaces the old Docker finding. It's a sparse file, so Petal counts the space it really takes, not its much larger apparent size. It's marked Manage in app, and Petal copies `docker system prune` for you; Docker's own settings can also shrink it.
- The "safe to delete" total no longer counts a finding twice when one sits inside another (Homebrew's downloads are inside App caches).
