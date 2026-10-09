---
"petal": minor
---

Colour the chart by safety, and show how big a folder's smaller objects are.

- **Colour: Safety** colours each folder a finding covers, and everything inside it: green for safe to delete, amber for review first, purple for manage in app. Everything else stays grey, because no finding says anything about it, which doesn't make it safe. A folder that only holds findings stays grey too. A legend explains the colours, and hovering a folder says which finding covers it. It works in the sunburst, icicle and treemap.
- **Smaller objects.** The segment that stands for a folder's too-small files is now see-through, so it can't be mistaken for a file, and hovering it shows how many things it holds and exactly how much space they take together. The idea is from Taras Brizitsky's "Sunburst — An interactive guide".
