# 60-minute native exploration — 26 October 2026

Tested `f246aa7c6d9c488f84ab5fa66e4f570f3faa972f` (published main), optimized Linux native GUI and CLI. Release v0.0.13 at e4b6ee6 has the same application source and dependency versions; only version/packaging/docs changed. This directory changes evidence only. See [summary.html](summary.html), [findings.json](findings.json), and [summary.json](summary.json).

The continuous run began at 09:58:31 UTC. Its completed duration/cycle totals are in summary.json. Earlier app runtime included an interaction pause and was excluded from that hour. The repeated-use loop ran between focused GUI scenarios; both used the real bundled native app. Corrected harness attempts (including saved PR form state) are retained in logs and are excluded from product findings. An attempted IPC timing wrapper could not replace the immutable native function; measurements cited in the report use external wall clocks, DOM ranges, and a bounded timestamped process trace. No memory-leak claim is inferred from RSS alone.

## Portable reproduction

Requirements: Python 3 and Git; the reviewed app. Linux/macOS is needed for the permission semantics. Run as an ordinary user for H05.

```sh
python3 create-fixtures.py --directory /tmp/parallax-review-fixtures
```

The directory must be new or empty. The script creates test data, unmerged index stages, and tree tags; it makes no commits and changes no identity settings. All writes and chmod commands below affect this disposable directory.

| Findings | Fixtures / actions |
| --- | --- |
| H01 | Merge `merge-editor/base.txt`, `local.txt`, `remote.txt` into `result.txt`; edit the first result line to `before`, Enter, type `ABC`, save. |
| H02 | Compare `editor/left.txt` and `right.txt`; leave a draft on the right, externally replace that file, save despite the detected-change banner. |
| H03 | Compare `empty/left.txt` and `right.txt`; try to start typing. |
| H04 | Compare `archive/reference.txt` against `archive/dest/target.zip!/a.txt`; edit a, keep unsaved. In a separate folder comparison copy `archive/source/target.zip` over `archive/dest/target.zip`. Save the old a draft; read b with Python zipfile. |
| H05 | `chmod 000 /tmp/parallax-review-fixtures/permissions/{left,right}/restricted`; compare those Git folders. Restore with `chmod 755 /tmp/parallax-review-fixtures/permissions/{left,right}/restricted` before cleanup. |
| H06 | Compare `needs/left/req.rst` and `needs/right/req.rst`, then parent folders. conf.py declares the extra scalar field. |
| H07 | Compare `yaml/left/data.yaml` and `yaml/right/data.yaml`, then parent folders. |
| H08/H09 | Open repository conflicts for `conflict-many`; 120 paths share 3 unique stage blobs. Close while syntax checking. |
| H10 | Compare `git-refs@short` with `git-refs@feature/a-very-long-reference-name-for-testing-side-specific-dropdown-and-the-path-layout`. These are tags pointing at a tree; no identity/commit is needed. A long branch in a normal repository exhibits the same layout issue. |
| H11 | Merge `merge-medium` files; drag input horizontal scrollbars fully right. |
| H12 | Merge `merge-long` files; inspect the base suffix beyond the 10k display window. |
| H13 | Compare `metadata` files, convert right to LF, replace first line with 100 x characters and save. |
| H14 | Compare editor files; select alpha in the right line input and use one native Ctrl+V to paste three lines. |

A direct production-core reproduction for H04 is in archive-probe.rs (requires the matching parallax_core release rlib). The normal GUI reproduction needs no Rust tooling. CLI false-equality outputs and exit codes are recorded in cli-results.json. A native trusted paste event and its saved bytes are in native-paste.log. Exact post-close process counts are in conflict-after-close-timed.json.

## Validation and scope

370 Rust tests passed (3 existing ignored), 12 frontend tests passed, Svelte check found zero errors/warnings, and release GUI/CLI builds succeeded. Selected earlier fixes passed: typed YAML keys/tags, CSV boundaries, symlink and executable conflict stages, empty merge typing, independent inner ZIP entry saves, annotated-tag history, and 4,000-conflict initialization. Million-line scrolling/search/edit/save passed. A public PR head/merge-base comparison and missing-PR error path were exercised through gh/git. No live enterprise GitHub host, Windows desktop, network share or SharePoint mount was available.

Original fixture generators/automation logs use disposable cloud paths. The portable generator above is the supported way to recreate cases elsewhere. Source links in the HTML point to the tested application commit, not this evidence commit.
