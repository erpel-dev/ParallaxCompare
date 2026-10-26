# Archive performance on network shares — investigation

Tested production core at 711b4137ad45fa0a2d0d764ca1b4b5cf2a757fb7. No application code was changed. The user reported intermittent archive slowness on network shares; the exact OS, protocol and operation were not yet supplied.

## What was measured

The benchmark wraps the real LocalFs in a VFS adapter that delays **only stat calls**, then uses the unmodified ArchiveMount and comparison engine. Four Rayon workers; 100/1,000 tiny deflated text entries; a second comparison reuses the parsed archives. The data remains local. This models metadata latency only: it is **not** an SMB/NFS/SharePoint throughput, cache, failure or Windows benchmark. Calls are VFS operations, not counted SMB packets. Cold/open costs are separate.

| Entries | Simulated stat delay | Mode | Warm time | Backing stat calls |
|---:|---:|---|---:|---:|
| 100 | 2 ms | default | 0.1729 s | 306 |
| 1000 | 0 ms | default | 0.0051 s | 3006 |
| 1000 | 2 ms | default | 1.6104 s | 3006 |
| 1000 | 10 ms | default | 7.6544 s | 3006 |
| 1000 | 2 ms | equal | 0.0141 s | 6 |
| 1000 | 2 ms | bytes | 0.0138 s | 6 |
| 1000 | 2 ms | verify | 1.5956 s | 3006 |

`default`: differing text entries with normal line-ending/content options. `equal`: matching entries can use stored CRC32/size. `verify`: matching entries with checksum verification enabled. `bytes`: differing entries with both line-ending equivalence and semantic-content comparison disabled. The bytes mode has different comparison semantics and is an experimental control, not a recommendation to change defaults.

## Confirmed overhead

1. **Repeated backing metadata calls even with warm archive caches.** ArchiveMount.resolve stats the archive before checking its cached ArchiveFs. Each changed entry's stream/open and on-disk detection revisit that path. For 1,000 changed entries the second comparison performs 3,006 backing stat calls (2,003 left, 1,003 right). The asymmetry comes from short-circuit evaluation of the on_disk check. With 2 ms/call this takes about1.6s; 10ms/call about7.7s. Matching CRC shortcut cases need only six such calls. Sources: [mount.rs:86](https://github.com/erpel-dev/ParallaxCompare/blob/711b4137ad45fa0a2d0d764ca1b4b5cf2a757fb7/crates/parallax-core/src/vfs/mount.rs#L86), [compare.rs:675](https://github.com/erpel-dev/ParallaxCompare/blob/711b4137ad45fa0a2d0d764ca1b4b5cf2a757fb7/crates/parallax-core/src/compare.rs#L675).
2. **Windows opens copy the entire archive into memory.** `Data::from_vec(data.to_vec())` is used instead of retaining the map, because mapped files cannot be replaced in the existing write path. An uncached archive on a share can therefore require reading its whole compressed payload before listing entries, even when the desired information is metadata. This is code-confirmed and not measured on Windows. Source: [mount.rs:128](https://github.com/erpel-dev/ParallaxCompare/blob/711b4137ad45fa0a2d0d764ca1b4b5cf2a757fb7/crates/parallax-core/src/vfs/mount.rs#L128).

Additional costs depend on workflow: compressed TAR indexing reads/decompresses its stream, a solid 7z member read can decode its block, full Refresh discards archive caches, and saving compares the current container bytes for stale-write protection before rewriting it. Those safeguards and format costs must not be removed casually. ZIP indexing also visits local headers through by_index_raw; actual remote traffic depends on OS caching/readahead and was not measured.

## Fix direction

- Resolve archive/container identity and type once per comparison operation; carry the resolved provider/entry handle through local-path and stream operations instead of restatting each backing archive. Invalidate at explicit refresh/change boundaries. Preserve same-size/same-time replacement detection and all stale-write checks at save.
- For Windows, use a file-backed random-access archive reader or a versioned local snapshot/cache with progress and cancellation. Do not simply retain a live map and break archive replacement.
- Reuse parsed archive indexes when content/version remains valid and coalesce watcher refreshes. Show stages (opening/indexing, reading entries, verifying, saving) so network waits are identifiable.
- Regression coverage: metadata-call counts should stay approximately constant as entry count grows for a warm container; compare results must match current behavior; test nested archives, atomic replacement, equal size/mtime changes, concurrent tabs, explicit refresh, Windows save/rename, cache cleanup and cancellation.

## Reproduce

```sh
python3 create-fixtures.py --directory /tmp/parallax-network-fixtures
# Compile against the exact production core artifact from cargo build --release -p parallax-core:
rustc --edition=2024 -O probe.rs --extern parallax_core=/path/to/libparallax_core-HASH.rlib -L dependency=/path/to/release/deps -o probe
RAYON_NUM_THREADS=4 ./probe /tmp/parallax-network-fixtures 1000 2 default
RAYON_NUM_THREADS=4 ./probe /tmp/parallax-network-fixtures 1000 10 default
RAYON_NUM_THREADS=4 ./probe /tmp/parallax-network-fixtures 1000 2 equal
```

Counter order is stat/list/read/local_path. Each command prints cold and warm rounds. No files are written by the comparison probe. Building the reviewed production core succeeded; focused benchmark comparisons returned the expected equal/different counts. No GUI or real network-share test is claimed for this investigation.
