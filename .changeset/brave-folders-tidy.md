---
"petal": minor
---

Move to Trash works on every Mac, Petal never offers parts of apps or tools for deletion, and a round of UI improvements.

- Move to Trash no longer fails with "File name too long" when Petal is opened from Finder or the Dock on a Mac with a very long `PATH`. If Finder can't do the move, Petal falls back to moving the items itself.
- A `node_modules` folder is offered only when it belongs to a project of yours that can reinstall it exactly (a `package.json` and a lockfile beside it). Nothing under `/System`, `/Library` or `/Applications` is offered, even when a scan starts from a link into one of them. The Collector refuses anything inside an app and says why; the whole app can still be collected.
- New finding: **Unpacked Git data**. It flags repositories that have grown large with loose objects, and offers the `git gc` commands that pack them instead of a delete button.
- The Collector shows what's already in it: rows and findings are marked **In Collector** with a check mark that takes them out again. Items with the same name are grouped into one chip ("node_modules ×619").
- Moving to the Trash shows its progress, then confirms how many items moved and how much space it freed. Findings update straight away.
- Folder rows have a **Copy Path** button. Buttons have tooltips with their shortcuts.
- The usual window commands: Hide (⌘H), Hide Others, Show All, Close Window (⌘W), Minimize (⌘M) and Zoom. Double-clicking the toolbar's buttons no longer minimises the window.
- The cloud-only and unreadable counts cover the whole scan, so they now appear once, at the top, instead of under every folder.
