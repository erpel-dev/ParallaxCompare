# Folder wildcard test — 26 October 2026

Tested published main d30edf0167f72d8837cabc98cb629272f6e781b1 in the optimized native Linux GUI. See summary.html, summary.json and findings.json. No application code changes.

Create fresh portable data:

```sh
python3 create-fixtures.py --directory /tmp/parallax-filter-fixtures --large
```

The directory must be new or empty. Only Python 3 and Git are needed; no commits or identity changes are made. The --large flag adds two 3,500-file trees.

- W01/W02: compare patterns/left with patterns/right. Try *.{java,kt}, ?.txt and ä.txt.
- W03: compare archive-warning/left with right, set *.txt, select bundle.zip and Copy right. Inspect the confirmation and hidden.bin with Python zipfile.
- W04: compare nested-copy/left with right, set *.txt, select wrapper and Copy right. Inspect normal.txt and the matching keep.txt inside bundle.zip.
- W05: compare git@base and git@same, enter missing*.txt, then Expand / Flat.
- W06: compare live/left and live/right with *.txt; externally modify existing.bin and create new.bin, then compare the incremental result to a full Refresh.

Normal filtered-directory copy is in copy-control. Full raw runtime/metrics and harnesses are compressed in runtime-evidence.tar.gz. Logs retain the corrected Exclude-label selector attempt; it is not an application finding. Twenty rapid pattern-change checks passed. All 12 frontend tests and the Svelte check passed. The large-tree timings are samples, not universal performance bounds.
