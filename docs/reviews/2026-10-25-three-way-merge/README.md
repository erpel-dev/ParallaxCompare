# Three-way merge exploration — 2026-10-25

Evidence from a 30 min 5 s exploration of [4c5ada6](https://github.com/erpel-dev/ParallaxCompare/commit/4c5ada6c3cd4158c3d581e4c921bb58f06d63181). The application source was not changed. Nine findings have reproduction steps, observed/expected behavior, fix directions and suggested regression checks in the accompanying issue.

- [HTML report](summary.html)
- [Machine-readable findings](findings.json)
- [Recorded run and test counts](summary.json)
- [Screenshots](screenshots)
- [Backend timings](backend-performance-confirmed.json), [GUI timings](phase6-results.json), [fresh-process memory](fresh-memory-results.json)
- [Git mergetool outcomes](mergetool-results.json), [repeated-use cycles](soak-results.json), [event timeline](runtime.jsonl)

## Create minimal fixtures

```sh
python3 docs/reviews/2026-10-25-three-way-merge/create-fixtures.py --directory /tmp/parallax-merge-repro
```

The directory must be new or empty. The generator requires Python 3 and Git, writes about 45 MB, creates no commits, and does not change an existing repository. M02/M03 use real unmerged index stages without needing a user identity. The test platform was Linux; symlink and executable-bit behavior is platform dependent.

For file fixtures, choose **New comparison → Merge three files…** and paste the local, base, remote and result paths from `manifest.json`. For `M02-M03-mode-conflicts`, choose **Resolve conflicts in a repository…**.

| Finding | Fixture | Key action |
|---|---|---|
| M01 · P1 | `M01-external-save` | Click Save result. |
| M02 · P1 | `M02-M03-mode-conflicts` (open as repository conflicts) | Stage resolved files and inspect git ls-files -s link. |
| M03 · P2 | `M02-M03-mode-conflicts` (open as repository conflicts) | Stage the resolved file and inspect git ls-files -s script.sh. |
| M04 · P3 | `M04-empty-result` | Try to add text to the empty result pane. |
| M05 · P2 | `M05-wide-lines` | Try to inspect the changed suffix in Local/Base/Remote. |
| M06 · P2 | `M06-mergiraf` (plus an installed mergiraf) | The version check succeeds, but the merge fails. |
| M07 · P2 | `M07-500-conflicts` through `M07-4000-conflicts` | Repeat for 500, 1000, 2000 and 4000 conflicts using the optimized build. |
| M08 · P2 | `M08-million-identical` | Run merge_structured with the built-in engine, which the GUI runs by default. |
| M09 · P2 | `M09-partial-syntax` | Focus the merge and press Alt+Left to take the local side. |

The original automation logs and harnesses use the original disposable workspace paths and a local WebDriver session. Use `create-fixtures.py` for portable manual reproductions. Those logs include a few corrected harness attempts; they are retained as evidence and are not treated as product failures. The HTML report and issue distinguish confirmed defects from timing/alignment differences and unproven memory-leak hypotheses.
