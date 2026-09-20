---
status: accepted
---

# Rustorr owns cache eviction at the storage seam, at torrent granularity

## Context

R1 defines a `seek-evicted` scenario, so Rustorr needs a cache with a
deliberate eviction policy. The R3 spike established that `librqbit 9.0.1`
exposes no public API for evicting an individual piece: `chunk_tracker`,
`bitv` and the piece bookkeeping are private modules, and the only public
lifecycle lever is `Session::delete` plus a re-add.

Two probes decided the shape of the seam.

**Torrent-scoped eviction works.** Run
`/tmp/rustorr-engine-spike/20260920T164027Z` deleted the torrent with its
files and re-added it; the re-fetched read returned the same bytes, with two
storage creations, two initializations and two completed pieces. The re-fetch
took about `20.1 s` against the local seeder.

**Storage-scoped eviction under a live torrent is unsafe.** Runs
`/tmp/rustorr-engine-spike/20260920T175558Z` and
`/tmp/rustorr-engine-spike/20260920T175634Z` read 262,144 bytes at offset
`4194304`, verified SHA-256
`210ba6b19ee6a72f875261cd3a41d030fad18470c0fc633ee61b1a7d84174795`, then
zeroed exactly that range on disk while the torrent stayed live, and read it
again through the same handle. Both runs returned
`8a39d2abd3999ab73c34db2476849cddf303ce389b35826850f9a700589b4a90` — the
zeroed bytes — in `1.2 ms` and `0.9 ms`. The engine did not error and did not
re-fetch: it trusts its in-memory have-bitfield and serves whatever the
storage returns.

That is the decisive constraint. Removing data underneath a live librqbit
torrent produces silent corruption, not a cache miss.

## Decision

1. **Rustorr owns the cache; the engine does not.** The cache lives above the
   engine at the public `StorageFactory` / `TorrentStorage` seam, which the
   spike already exercises end to end (`creates`, `inits`, `reads`, `writes`,
   `on_piece_completed`, `take`, `remove_file`).
2. **Rustorr keeps its own residency index.** Piece residency is recorded from
   `on_piece_completed` and the write path, so eviction policy never reads
   engine internals and survives an engine swap.
3. **Eviction granularity is the torrent, not the piece.** To evict, Rustorr
   stops and deletes the torrent from the session, drops its storage, and
   re-adds on the next request. Piece-level removal under a live torrent is
   forbidden by invariant, because it is proven to corrupt reads silently.
4. **The policy is a size-capped LRU over torrent-scoped entries with a pinned
   read window.** A torrent with any live view is pinned and never evicted;
   among unpinned entries the least-recently-read is evicted first until the
   cache is under its cap.
5. **`TorrentStorage::pread_exact` must fail loudly on a missing range.**
   Rustorr's storage implementation returns an error rather than zeros if its
   backing data is gone, so an invariant violation surfaces as a failed read
   instead of corrupt media.
6. **The escalation path is explicit.** If measurement in R5/R11 shows that
   torrent-scoped eviction is too coarse — for example that a 100-view working
   set thrashes on whole-torrent re-fetches — the remedy is an upstream change
   exposing piece invalidation. That, and only that, is the condition that
   turns the engine decision from `adopt` into `fork`.

## Consequences

- The measured torrent-scoped re-fetch cost of about `20.1 s` is the price of
  an eviction miss today, so cap sizing and read-window pinning matter more
  than eviction speed. R1's `seek-evicted` reference row is `8507.9 ms`; the
  two are not like-for-like operations (see
  [engine-r1-metric-mapping.md](../engine-r1-metric-mapping.md)), and the
  comparable number can only be produced by R5 against Rustorr's HTTP surface.
- R4 must expose the cache as an explicit component with its own residency
  index and an eviction entry point, not as an incidental property of the
  engine adapter.
- R5 must run the R1 `seek-evicted` scenario against Rustorr with a
  deterministic eviction trigger — something the R1 baseline could not do,
  since TorrServer exposes no deterministic eviction API.
- Rustorr's cache cap becomes a first-class configuration value with a
  documented default, since it now directly determines eviction frequency.

## Alternatives considered

- **Piece-level LRU at the storage layer under a live torrent.** Rejected: two
  probes show it silently returns wrong bytes.
- **Fork librqbit now to expose piece invalidation.** Rejected for R3: there
  is no evidence yet that torrent-scoped eviction is insufficient, and a fork
  is a standing maintenance cost. Kept as the named escalation in decision 6.
- **Delegate eviction to the engine.** Not possible in `9.0.1`; and it would
  couple Rustorr's product behavior to engine internals, which is what the
  adapter seam exists to prevent.
