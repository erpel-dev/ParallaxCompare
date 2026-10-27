# Fix verification for 0.0.14

All eight findings E01–E08 passed the bundled native GUI replay. The tests also checked that finishing a comparison in the background preserves the currently selected tab.

For E05, ordinary replacements and replacements preserving both size and mtime were tested. Saving asks before replacement; cancelling preserves the external member, explicitly replacing preserves unrelated member changes, and clean tabs reload current content. The screenshots show the new confirmation and the fresh reload after an equal-size/equal-time replacement.

The source tested on branch commit `6e02aec` is identical to merged source `e0610402530c47120055dae609480e62eb5ec521` for the application, crates, dependency/version files and release notes. `results.json` records the GUI checks and binary hash. Local validation passed 402 Rust tests, 19 frontend tests, strict Clippy, formatting, Svelte checks, the frontend production build and the optimized native build. Three pre-existing Rust tests remain ignored. Linux and Windows CI passed for PR #18.

The GUI checks ran on Linux/WebKitGTK with an isolated profile and disposable local fixtures. No real network-share or Windows GUI test was performed.
