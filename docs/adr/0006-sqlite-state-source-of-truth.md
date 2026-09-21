---
status: accepted
---

# SQLite is Rustorr's persistent source of truth

## Context

Catalog entries, settings and viewed state must survive a restart and be
portable independently of librqbit's internal persistence format. Rustorr
also needs a future online-backup path.

## Decision

`rustorr-state` stores state in `<data-dir>/rustorr.db` through bundled
`rusqlite`. It owns migrations, sets a Rustorr-specific `application_id`,
uses `user_version`, WAL mode and the SQLite default synchronous policy.
Unknown application IDs and newer schemas are rejected before mutation.

Librqbit persistence and fast-resume are disabled. Its transient session,
peers and runtime handles are reconstructed on startup; DHT routing data at
`<data-dir>/engine/dht.json` is engine-owned and disposable.

## Consequences

The database is the only persistent authority for Rustorr product data, which
makes R9 backup/restore a SQLite concern rather than an engine-format concern.
WAL needs an operational check on network filesystems in R9. `synchronous` is
not tuned prematurely; durability/throughput measurements decide that later.

## Alternatives considered

- Persist librqbit fast-resume as application state: rejected because its
  contract was only evidenced for the same output directory and couples state
  to the engine.
- JSON files per feature: rejected because atomic multi-table migration,
  integrity checking and online backup would be rebuilt ad hoc.
