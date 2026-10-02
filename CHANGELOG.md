# petal

## 0.2.0

### Minor Changes

- c55c1b8: Colour the chart by kind. A new "Colour: Folder | Kind" switch in the toolbar colours the chart by what things are (apps, downloads and Trash, developer files, caches, photos, video and music, documents, app data) using a small model that runs on your Mac and reads only file paths. A legend names the kinds in view; hover one to highlight it.
- c55c1b8: Results stay up to date. While the results are open, Petal follows changes on disk and updates the chart within a second or two: delete something in Finder, empty the Trash or finish a download and the sizes change without a rescan. If macOS loses track of changes, the toolbar says the results are out of date.

### Patch Changes

- 95aaba0: Only a project's node_modules is listed as safe to delete. Petal no longer counts node_modules inside apps (installed or built locally, such as Cursor or T3 Code), editor extensions, global installs, tool runtimes or app data in Library, where deleting them breaks that software; it now needs a package.json beside the folder, so your package manager can put it back. No finding includes anything inside an app or other bundle.
