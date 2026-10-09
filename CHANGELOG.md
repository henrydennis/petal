# petal

## 0.5.0

### Minor Changes

- 0e0d603: New findings for AI tools and developers, and a third label, **Manage in app**, for things that belong to an app. These are shown and explained but never offered for the Trash.
  
  - **Local AI models** from Ollama, LM Studio and Hugging Face are marked Review first. They're downloads you'd have to fetch again, and the explanation points to each tool's own way to remove them (`ollama rm <model>`, LM Studio's model list, Hugging Face's `hf cache` commands). For Hugging Face only the `hub` folder is included, because the folder above it also holds your login token.
  - **Claude Code and Codex histories** (`~/.claude/projects`, `~/.codex/sessions`) are marked Manage in app. They're your conversations, kept so you can resume them.
  - **Cargo `target` folders** are Safe to delete, but only when the folder sits right next to a `Cargo.toml`. Any other folder called `target` isn't reported, and, as with `node_modules`, nothing inside apps, `~/Library` or hidden folders is ever included.
  - **Homebrew's downloads** are Safe to delete. Petal copies `brew cleanup --prune=all` for you rather than deleting the folder.
  - **The pnpm store** is marked Review first. Your projects' `node_modules` share its files through hard links or clones, so Petal shows only what deleting the store would really free, and copies `pnpm store prune` for you.
  - **Docker's disk image** replaces the old Docker finding. It's a sparse file, so Petal counts the space it really takes, not its much larger apparent size. It's marked Manage in app, and Petal copies `docker system prune` for you; Docker's own settings can also shrink it.
  - The "safe to delete" total no longer counts a finding twice when one sits inside another (Homebrew's downloads are inside App caches).
- f5e5b00: Read protected folders as an administrator, see APFS snapshots, and see purgeable space.
  
  - **Read Protected Folders as Administrator…** (File menu, or the card that appears when some folders couldn't be read) reads the folders that belong to macOS or other users, after macOS asks for an administrator's password. Only those folders are read again, and their sizes slot into the results in place, with no rescan. Petal never runs as root itself: a one-off helper reads names and sizes, hands them back and exits.
  - **APFS snapshots.** On the startup disk, Petal lists the Data volume's snapshots (Time Machine, macOS updates, other apps) with their dates. APFS doesn't report how much space each snapshot holds, so when there are snapshots the remainder slice is called "Snapshots and unreadable". Time Machine's local snapshots can be deleted from the card (macOS asks for a password), and Petal then shows how much that freed.
  - **Purgeable space**, the space macOS frees by itself when it needs room, is shown on the Disks screen and at the top of the results.
- c3e0669: Colour the chart by safety, and show how big a folder's smaller objects are.
  
  - **Colour: Safety** colours each folder a finding covers, and everything inside it: green for safe to delete, amber for review first, purple for manage in app. Everything else stays grey, because no finding says anything about it, which doesn't make it safe. A folder that only holds findings stays grey too. A legend explains the colours, and hovering a folder says which finding covers it. It works in the sunburst, icicle and treemap.
  - **Smaller objects.** The segment that stands for a folder's too-small files is now see-through, so it can't be mistaken for a file, and hovering it shows how many things it holds and exactly how much space they take together. The idea is from Taras Brizitsky's "Sunburst — An interactive guide".
- af5f7fd: Scan folders and list findings from the command line, with JSON output for scripts and coding agents.
  
  - **`petal scan PATH --json`** prints the folder's size, file and folder counts, unreadable folders and a tree of what's inside. `--depth N` and `--top N` keep the output short: the smaller items in each folder are added up into one "other" entry, so the sizes always add up.
  - **`petal findings --json`** scans your home folder (or `petal findings PATH --json`) and prints each finding with its id, whether it's safe to delete, worth a look first or managed in its app, its explanation, its folders, its allocated size and, for what can go in the Trash, exactly what deleting it frees. Findings cleared another way say so: Git repositories come with their `git gc` commands, Homebrew downloads, the pnpm store and Docker's disk image with the command that clears them, and "Manage in app" findings such as your Claude Code history with nothing to delete.
  - Without `--json`, both print a short summary. Neither opens a window. The README describes the JSON schema, which has a `schema_version` so scripts can rely on it.

## 0.4.1

### Patch Changes

- 69a2ea9: Hovering the chart stays put. While Petal followed changes on disk, every refresh (about once a second in a busy folder like your home folder) dropped the hover until you moved the mouse again. The hover now stays on, and follows whatever is under the pointer as the chart changes beneath it.

## 0.4.0

### Minor Changes

- 4e35367: Show the chart as an icicle or a treemap as well as a sunburst. A new "Chart: Sunburst | Icicle | Treemap" switch in the toolbar (or ⌘1, ⌘2, ⌘3, and the View menu) changes how the same folders are drawn. The icicle unrolls the sunburst into rows that fall from the folder you're in, one per level, with room for names. The treemap shows the folder's contents as boxes sized by the space they take; click one to go inside. Both work live during a scan and zoom like the sunburst, and colouring by kind, hover and the legend work the same in all three.

## 0.3.0

### Minor Changes

- 3569143: Move to Trash works on every Mac, Petal never offers parts of apps or tools for deletion, and a round of UI improvements.
  
  - Move to Trash no longer fails with "File name too long" when Petal is opened from Finder or the Dock on a Mac with a very long `PATH`. If Finder can't do the move, Petal falls back to moving the items itself.
  - A `node_modules` folder is offered only when it belongs to a project of yours that can reinstall it exactly (a `package.json` and a lockfile beside it). Nothing under `/System`, `/Library` or `/Applications` is offered, even when a scan starts from a link into one of them. The Collector refuses anything inside an app and says why; the whole app can still be collected.
  - New finding: **Unpacked Git data**. It flags repositories that have grown large with loose objects, and offers the `git gc` commands that pack them instead of a delete button.
  - The Collector shows what's already in it: rows and findings are marked **In Collector** with a check mark that takes them out again. Items with the same name are grouped into one chip ("node_modules ×619").
  - Moving to the Trash shows its progress, then confirms how many items moved and how much space it freed. Findings update straight away.
  - Folder rows have a **Copy Path** button. Buttons have tooltips with their shortcuts.
  - The usual window commands: Hide (⌘H), Hide Others, Show All, Close Window (⌘W), Minimize (⌘M) and Zoom. Double-clicking the toolbar's buttons no longer minimises the window.
  - The cloud-only and unreadable counts cover the whole scan, so they now appear once, at the top, instead of under every folder.

## 0.2.1

### Patch Changes

- bbf2bff: Smoother chart animation. While scanning, folders keep their place and the chart fills in as one piece instead of sectors sprouting, sliding past each other and leaving gaps; the chart no longer reacts to the mouse until the scan is done. When the scan finishes, the chart folds away smallest first as the results open behind it, largest first; the first chart opens out from the centre, opening a folder zooms the whole chart like a camera (and back out again), and folders ease into full colour when their total is final rather than flashing.

## 0.2.0

### Minor Changes

- c55c1b8: Colour the chart by kind. A new "Colour: Folder | Kind" switch in the toolbar colours the chart by what things are (apps, downloads and Trash, developer files, caches, photos, video and music, documents, app data) using a small model that runs on your Mac and reads only file paths. A legend names the kinds in view; hover one to highlight it.
- c55c1b8: Results stay up to date. While the results are open, Petal follows changes on disk and updates the chart within a second or two: delete something in Finder, empty the Trash or finish a download and the sizes change without a rescan. If macOS loses track of changes, the toolbar says the results are out of date.

### Patch Changes

- 95aaba0: Only a project's node_modules is listed as safe to delete. Petal no longer counts node_modules inside apps (installed or built locally, such as Cursor or T3 Code), editor extensions, global installs, tool runtimes or app data in Library, where deleting them breaks that software; it now needs a package.json beside the folder, so your package manager can put it back. No finding includes anything inside an app or other bundle.
