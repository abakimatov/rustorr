---
status: accepted
---

# Keep the BitTorrent engine behind a mechanically checked crate boundary

## Context

Rustorr needs the engine, cache, persistent state and HTTP surface to evolve
at different speeds without importing `librqbit` types into client-facing
code. ADR 0003 requires that isolation; ADR 0004 requires a Rustorr-owned
storage seam for cache eviction.

## Decision

- `rustorr-domain` contains IO-free identifiers, ranges and errors.
- Only `rustorr-engine` has direct dependencies on `librqbit` and
  `librqbit-core`. Its public port is expressed in Rustorr domain types.
- `rustorr-cache` owns `PieceStore`, residency, pins and torrent-scoped LRU.
  `rustorr-engine` adapts librqbit's storage traits to that port; cache never
  imports engine or librqbit.
- `rustorr-state` owns SQLite state; `rustorr-http` maps crate errors to HTTP;
  `rustorr-server` is the sole composition root for concrete implementations.
- `tools/r4/check-boundaries.py`, run by `tools/r4.sh check`, rejects an
  unapproved workspace edge and any direct librqbit dependency outside engine.

## Consequences

The checked graph prevents accidental engine leakage and makes an engine swap
local to one crate. The cache's soft cap remains deliberately soft: a pinned
torrent cannot be evicted. R5 coordinates `Session::delete` before
`Cache::remove`; piece-level eviction stays forbidden by ADR 0004.

## Alternatives considered

- Expose librqbit types from HTTP or cache: rejected, as it violates ADR 0003
  and makes a future engine replacement cross-cutting.
- Let the engine own storage and eviction: rejected, because it cannot safely
  invalidate a live piece in librqbit 9.0.1.
