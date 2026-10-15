# Verification

Verified on native Windows, 2026-10-15, using fictional temporary libraries.

| Check | Result |
|---|---|
| `cargo fmt --check` | PASS |
| `cargo clippy --offline --locked --all-targets -- -D warnings` | PASS |
| `cargo test --offline --locked` | PASS — 24 tests |
| `cargo run --offline --locked --example roundtrip` | PASS — committed revision retained, stale write rejected, exact staged restore |
| Publication-file privacy check | PASS — explicit text-file allowlist; no original Git history, personal author identity, local user paths, screenshots, binaries, credentials matching tested patterns, databases or backup bundles |

Tests cover real SQLite/filesystem persistence, migration rollback, backup hash/path validation, JSON reconstruction, attachments, poem whitespace, history, Trash and privacy-aware search. Desktop and network tests are outside this extraction. This is an experimental library, not a packaged desktop release or a claim of an exhaustive security audit.
