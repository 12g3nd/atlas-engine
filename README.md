<div align="center">

# Atlas Engine

**Write locally. Link by identity. Restore with confidence.**

A small Rust storage engine extracted from an offline desktop notebook.

</div>

---

- **Durable writing** — SQLite WAL, full synchronous commits, stale-revision rejection.
- **A connected collection** — UUID links, backlinks, local FTS5 search, retained history.
- **Portable recovery** — consistent SQLite snapshots, canonical JSON, attachment hashes, staged restore.
- **Exact text** — poetry whitespace and editor blocks survive backup round trips.

```sh
cargo test --locked
cargo run --locked --example roundtrip
```

The example creates a temporary library, rejects a stale write, and verifies a complete backup/restore round trip. Tests use fictional data only.

Experimental API; this is the storage core, without the desktop UI or network features. Libraries and exports are **not encrypted**. Hosts must serialize repository access; backup restores select a saved state rather than merge timelines.
