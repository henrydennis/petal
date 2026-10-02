# How Petal is fast, and how it stays honest

Petal was tuned in measured rounds: one change at a time, kept only if it beat the previous best on
an A/B benchmark *and* passed a correctness gate. This page summarises what worked, what didn't, and
how the numbers are checked. Figures are from an Apple Silicon MacBook (10 cores, APFS, internal SSD).

## The scan

**Where the time goes.** A full scan is almost entirely kernel time spent reading directory
metadata. Per-file attributes are essentially free once you ask for them in bulk; the cost is per
*folder*. On a whole disk the walk is bound by SSD metadata reads when the cache is cold, and by a
system-wide filesystem lock when it's warm.

| Change | Effect |
|---|---|
| `getattrlistbulk` per folder (names, types, allocated sizes, inode numbers, link counts in one call) instead of `readdir` + an `lstat` per entry | 1.16× |
| Open each folder with `openat(parent_fd, name)` instead of by full path (falls back to a full path on `EMFILE`) | 1.38× cumulative |
| Tally files inline; only subfolders become parallel (rayon) tasks; publish progress once per folder | ~1.17× more |
| 32 KB listing buffer instead of 256 KB | ~1.06× more |
| **Total vs. the first version** | **1.53×** on a 1.5 M-item source tree, **1.79×** on `/System/Library`, about half the CPU |

**Rejected, with reasons:**

- *Trusting `ATTR_DIR_ENTRYCOUNT` to skip the final listing call.* The byte-exact gate caught it:
  APFS reports 0 entries for firmlinked and sealed-volume folders (e.g. `/Applications`), so files
  silently went missing.
- *Skipping the "no more entries" call when a batch came back short.* Up to 4% faster, but it relies
  on undocumented batching behaviour; a failure would mean silently missing files.
- *More threads, several processes, `searchfs` (catalog search), Spotlight.* No gain (the lock is
  system-wide), or slower than the parallel walk, or blind to most of the disk (Spotlight skips
  `~/Library`, hidden folders and system areas).
- *Reading APFS clone information for every file during the scan.* 12–130% slower. Clone accounting
  moved off the scan path instead (see "Exact savings").

**Never downloads anything.** iCloud Drive's cloud-only ("dataless") folders are detected from their
flags and skipped, and the process sets `IOPOL_MATERIALIZE_DATALESS_FILES` off, so a scan can't stall
on, or trigger, a download.

## The live chart

The question here isn't "how fast is the scan" but "how soon does the picture look right".
`petal --bench-live <path>` samples the live totals every 100 ms and scores each sample against the
final result: when 50% and 90% of the bytes are on screen, when the top three folders are in their
final order, and when the top-level split stays within 5% of the end result.

What made the biggest difference:

1. **Live totals.** Each folder in the top four levels has an atomic counter that the walk adds to
   once per folder; the UI snapshots them ten times a second (~1–3 ms per snapshot).
2. **Tiers.** An *outline* pass lists the first two levels by name (done in ~10 ms), then a *hotspot*
   pass reads the folders that are usually huge (Trash, Downloads, Xcode, simulators, backups, Docker,
   caches, Movies, Mail…) before the main walk, which reuses those results rather than re-reading them.
   Hotspots are exact within ~2.5 s on a full disk.
3. **Final as you go.** A folder is marked final the moment the walk leaves it, and drawn in full
   colour; folders still counting are muted and labelled "≥ size". The chart is look-only until
   the scan finishes (nothing under the pointer changes as it grows). On a full disk, half the bytes sit in final folders by ~11 s of a ~24 s scan.
4. **An exact skeleton for the startup disk.** Scanning `/` reads only the Data volume; every other
   APFS volume in the container (macOS itself, Preboot, VM, Recovery, Update) gets an exact slice
   from `ATTR_VOL_SPACEUSED`, and whatever couldn't be read becomes an exact "Not readable" slice.
   The chart's total therefore equals the disk's used space to the byte.
5. **Motion.** Segments are matched across snapshots by folder path, and every edge follows the
   same critically damped spring (ω = 13/s, settling in about half a second), so the chart glides
   instead of jumping ten times a second. Measured on real frames at 120 Hz: the old chart's edge
   stood still in 93% of frames and then jumped up to 10.7°; now it moves every frame, at most 1.0°.
   The chart moves as one piece: new folders open up at the edge they share with a sibling,
   departed ones close up where their neighbours meet, and mid-scan folders keep the place they
   first appeared in rather than being re-sorted by size at each snapshot (the order lives on the
   live tree's nodes, so building a snapshot's view stays well under a millisecond), so no two
   segments ever slide across each other and no gaps open. When the results do re-sort (sizes
   changing on disk), a folder that changes place closes up where it was and opens where it goes.
   Changing folder moves a camera over the whole chart, and the first chart is revealed outward
   from the centre. When the scan finishes, its chart is re-sorted by size, which moves almost
   everything; rather than shuffle, the scan's chart folds away clockwise, smallest folder
   first, while the results open clockwise from the top into the room it leaves, at their true
   sizes, largest first (about a second).

Rejected: a breadth-first work queue (every live metric got worse: it explores structure before the
leaves where the bytes are) and dropping the barrier between hotspots and the main walk (the
hotspots lost their head start).

**Gates for the live chart:**

- The live totals must equal the finished tree exactly.
- `premature-final` must be 0: no folder may be shown as final and then change size. (This caught a
  real bug: a cloud-only folder deep in the tree marked an *ancestor* final.)
- Early findings must equal their final sizes (`findings wrong 0`).
- The total scan time must not regress (A/B with `bench/ab.sh`).

## After the scan: staying current

A full scan of a whole Mac re-reads about 3.4 GB of APFS metadata in 4 KB pieces (~880,000 reads),
even right after the previous scan: the kernel's metadata buffer cache is ~64 MB (`kern.nbuf`
16,384), so nothing stays cached between scans. Rather than rescanning, Petal follows changes.

- The FSEvents position is noted **before** the walk starts; once the results are up, a stream from
  that position (`src/watch.rs`) reports every folder that changed since, including during the scan.
- Once a second, the changed folders are read again in the background, one listing each (new
  subfolders are walked in full; macOS's "rescan below here" flag rescans that folder whole), and
  the size change is carried up to the root. Focus, the Collector, findings and the startup disk's
  volume slices are kept in step.
- Measured with whole-disk results open for a minute: 52 updates, median **1.2 ms** each, slowest
  later update 266 ms. The first update catches up on changes made during the scan while findings'
  savings are still being worked out, and takes ~10 s.
- Working out a finding's savings walks its folders again (seconds, for caches), and caches change
  constantly. So a finding keeps its worked-out savings while its allocated size moves by under 1%
  (or 64 MB); a real change, such as emptying the Trash, works them out again.
- If macOS loses track (events dropped, the watched folder moved), the toolbar says the results are
  out of date and a rescan is needed.

**Gate:** `applied_changes_match_a_fresh_scan` adds, grows, renames and deletes files and folders,
applies the changes folder by folder, and requires the tree to equal a fresh scan exactly.

## Progress and time left

The bar counts items (files + folders) against the volume's object count, which `statfs` reports
instantly and exactly. Bytes would be a poor measure: folders that need Full Disk Access hide far more
bytes than items, so a bytes bar would stall around 80%. "About N s left" comes from an item rate
smoothed over 3 s and is shown only after 1.5 s; in tests it errs on the long side by 0–3 s.

## Exact savings

Petal shows sizes as allocated blocks, like Finder's "on disk". What deleting something *frees* is
different on APFS, where files can be clones sharing blocks, or hard links. So when you collect items
(or when findings resolve), `scan::frees_of` walks just those folders in the background with clone
and link reporting on:

- a family of pure clones is freed only if every member is selected;
- a partially cloned file frees its private bytes (looked up lazily);
- a hard-linked file is freed only if every link is selected.

`cargo test -- --ignored clone_accounting_matches_apfs` checks this against reality: it builds a
throwaway APFS disk image (no admin needed), predicts what deleting each item frees, deletes it and
measures. Prediction equals the space actually freed in every case.

## Reproducing

```sh
cargo build --release
./target/release/petal --bench-scan ~/Library 6          # median of 6 headless scans
./target/release/petal --bench-live / 3                  # live-chart metrics (grant Full Disk Access for /)
./bench/ab.sh old/petal new/petal 8 ~/Library            # interleaved A/B with a result-fingerprint check
cargo test                                               # unit tests
cargo test -- --ignored                                  # plus the APFS disk-image test
```

`bench/ab.sh` alternates the two binaries (flipping the order each round) so both see the same
background load, and refuses to report a speed-up if the two results differ.
