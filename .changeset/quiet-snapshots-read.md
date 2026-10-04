---
"petal": minor
---

Read protected folders as an administrator, see APFS snapshots, and see purgeable space.

- **Read Protected Folders as Administrator…** (File menu, or the card that appears when some folders couldn't be read) reads the folders that belong to macOS or other users, after macOS asks for an administrator's password. Only those folders are read again, and their sizes slot into the results in place, with no rescan. Petal never runs as root itself: a one-off helper reads names and sizes, hands them back and exits.
- **APFS snapshots.** On the startup disk, Petal lists the Data volume's snapshots (Time Machine, macOS updates, other apps) with their dates. APFS doesn't report how much space each snapshot holds, so when there are snapshots the remainder slice is called "Snapshots and unreadable". Time Machine's local snapshots can be deleted from the card (macOS asks for a password), and Petal then shows how much that freed.
- **Purgeable space**, the space macOS frees by itself when it needs room, is shown on the Disks screen and at the top of the results.
