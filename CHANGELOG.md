# petal

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
