# Desktop exploration: 2026-10-26, resumed 2026-10-27

Eight confirmed findings: **one P1, five P2, two P3**. The main concern is E05: a direct archive-member editor can miss an external replacement during reload and overwrite that member without the stale-save confirmation.

Open [summary.html](summary.html) locally after downloading this directory. [findings.json](findings.json) contains reproduction steps, actual and expected results, source links pinned to the tested commit, fix directions and evidence for every finding. The GitHub issue contains the same actionable findings with embedded screenshots.

## Tested application

- Source: `90c6ecfe5ccc30e50a76e536b56fb551f924c36a`, including the startup and archive/wildcard fixes already merged in PRs #15 and #16.
- Optimized native Tauri application with its bundled frontend, built with `cargo build --release -p parallax-compare --features tauri/custom-protocol`.
- Binary SHA-256: `ec59e21f203daf7db09172a5d783c6ed215b2a84bbaa7d476457076e77182f6f`.
- Linux, WebKitGTK, Xvfb at 1400×900, isolated application profiles, disposable local fixtures. Frontend checks, all 15 frontend tests and production build passed. Build/check logs are included.
- Application source was not changed for this review.

## Timing and run integrity

The initial timer ran from **2026-10-26 19:40:18 UTC to 20:40:19 UTC**. The app and driver were last observed alive at **20:31:27 UTC**, and the first connection refusal followed at **20:31:30 UTC**. The cause of that termination is unconfirmed; the driver log has no crash diagnosis. The remaining connection-refused interval is excluded from application time and successful cycles.

A fresh native session started at **2026-10-27 02:58:14 UTC** for a further **15 minutes**. The final observed duration and counts are in [runtime-summary.json](runtime-summary.json). Together the sessions provide over an hour of observed app runtime, with an interruption; this was not an uninterrupted hour.

The initial session completed 1,304 mixed-use cycles. Another 69 attempts failed because the harness assumed the two-source form after a real PR comparison had changed the saved form mode. The resumed session initially encountered a native conflict-marker save dialog left open by a probe; four subsequent comparison attempts timed out until it was dismissed. Those harness problems are excluded from confirmed product findings. Corrected independent-merge measurements are in [resume/merge-independent-retry.log](resume/merge-independent-retry.log).

`initial/` and `resume/` contain timestamped successful/manual events, process samples while the app was alive, driver logs and compact failure accounting. Empty monitor samples after the initial termination are omitted. Console `.log` output abbreviates long events and has presentation whitespace normalized; JSONL events retain the full captured records. The raw initial timer's end does not establish that the application remained running.

## Coverage and observations

Repeated use exercised text navigation/search/status filters; folder wildcard filters, flat view and copies; Git branches and a real read-only GitHub PR; clean three-way merge saves; nested ZIP and solid 7z comparisons. Targeted checks covered external archive edits, encodings/BOMs/line endings, million-line files, long lines, 4,000 independent merge conflicts, DCM/PDX/ECUC, needs RST/JSON, Nix, SCORE configuration, FIDL, FlatBuffers, images and office documents.

The positive controls preserved UTF-8 BOM, UTF-16LE BOM and CRLF, mixed line endings, and missing final newlines. An unmappable CP1252 edit was rejected without changing the file. Continuous typing after Enter in the merge editor preserved the entered text. The PR comparison resolved its merge base/head and disabled copy actions. These checks do not establish exhaustive correctness for each format.

Illustrative observations, including WebDriver waits:

| Operation | Observed time |
| --- | --- |
| Open 100,000-line comparison | ~0.86 s |
| Open 1,000,000-line comparison | ~0.96 s |
| Sampled large-file scroll updates | ~20–130 ms |
| Open solid 7z comparison, 64 MiB per side | ~0.88 s |
| Open 4,000 independent merge conflicts | ~0.18 s |
| Choose one of those conflicts | ~0.21 s, including a 0.20 s harness wait |

Peak memory followed deliberate large-file/archive stress; these observations do not establish a leak. E06 is a correctness gap at a bounded matching threshold, rather than a recurrence of the earlier unbounded quadratic allocation. No real SMB/SharePoint share, Windows, macOS, or high-latency remote filesystem was tested.

E05 is related to the archive-write concerns in #12/H04 and #14, but concerns the **edited member's stale check and reload**: the unrelated externally edited member was preserved in this reproduction. This report does not establish which commit introduced any finding.

## Portable fixtures and GUI reproduction

Create a new disposable directory with Python's standard library:

```sh
python3 fixtures.py create /tmp/parallax-review-fixtures
```

Use the matching folder names in `findings.json` for manual reproductions. E07's sources are `cancel-form/left/changed.txt` and `cancel-form/right/changed.txt`, or the two parent folders. For E05, compare `archive-stale/left.txt` directly against `archive-stale/dirty.zip!/a.txt`, edit the right side, then run:

```sh
python3 fixtures.py replace /tmp/parallax-review-fixtures/archive-stale/dirty.zip
```

Return to the app and save. Check the ZIP's `a.txt` and `b.txt` with an independent archive reader. `--preserve-mtime` repeats the equal-size/equal-time control; the bug also occurs without that option. Use `clean.zip` for the automatic reload control and keep folder comparison tabs closed for this reproduction. Generate fresh fixtures for each copy test, since the copy operation changes its disposable destination. Filenames with trailing spaces are a Unix-specific test.

The `replays/` directory retains the native WebDriver reproductions for E01–E08. They use production GUI/API paths and create/edit only the disposable fixtures. Python 3, Pillow, a native `tauri-driver`/WebKitWebDriver setup and a running X11 desktop on `:99` are required. Start the driver on port 4510 with native port 4511 in an isolated application profile. For example, after building the bundled app:

```sh
export PARALLAX_REVIEW_BINARY=/absolute/path/to/parallax-compare
export PARALLAX_REVIEW_FIXTURES=/tmp/parallax-review-fixtures
export PARALLAX_REVIEW_LOGS=/tmp/parallax-review-replay-output
python3 replays/ui.py
python3 replays/archive-stale-ordinary.py
```

`ui.py` starts a fresh session through the driver and records its session ID under `PARALLAX_REVIEW_LOGS`. The endpoint defaults to `http://127.0.0.1:4510`; `PARALLAX_REVIEW_ENDPOINT` can override it. Other replay scripts share that session. Run one at a time, with a fresh fixture directory for repeat copy tests. E07 intentionally schedules Compare and Close in the same GUI event turn to reliably reproduce the pending-open race.
