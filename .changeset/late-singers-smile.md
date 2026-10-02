---
"petal": patch
---

Only a project's node_modules is listed as safe to delete. Petal no longer counts node_modules inside apps (installed or built locally, such as Cursor or T3 Code), editor extensions, global installs, tool runtimes or app data in Library, where deleting them breaks that software; it now needs a package.json beside the folder, so your package manager can put it back. No finding includes anything inside an app or other bundle.
