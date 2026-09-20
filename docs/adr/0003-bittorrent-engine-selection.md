---
status: accepted
---

# Adopt librqbit 9.0.1 as the Rustorr BitTorrent engine, behind an adapter

Decision: **adopt**, pinned at `librqbit = 9.0.1`, used only through Rustorr's
own engine adapter, with one named fork trigger and two carried performance
questions.

## Context

Rustorr isolated the BitTorrent implementation behind an engine-adapter spike
(`tools/engine-spike/`) before creating the production workspace. This ADR was
held at `proposed` until the R3 gates closed. All of them are now addressed:
capability evidence, a peer-exchange probe, a cache-eviction seam, and an
explicit rule for comparing engine measurements to the R1 floors.

The spike does not implement Rustorr HTTP routes, the production cache, or
player integration. Adoption applies to the engine choice only.

## Evidence behind the decision

All runs are bounded, Dockerised against the R1 tracker and seeder, with raw
artifacts under `/tmp/rustorr-engine-spike/` (intentionally not committed).
Details are in [`engine-spike.md`](../engine-spike.md).

**Streaming functionality.** Metadata and file mapping, positional reads,
seek, cancellation, three concurrent views in one session, magnet metadata
resolution, custom `StorageFactory` callbacks, session delete/re-add/re-fetch,
and same-output persistence fast-resume all pass with matching SHA-256
digests.

**Transport capabilities.** DHT bootstrap discovery reached
`routing_table_size=58` against an isolated bootstrap node
(`dht-two-peer-20260921T172000Z/client.json`). A uTP-only probe completed a
correct read with `live_utp=1`, `live_tcp=0`
(`20260920T171757Z/probe.json`).

**Peer exchange (the gate that was open).** The two-peer harness
(`tools/engine-spike/run.sh pex`) puts a tracker-connected middle peer between
the seeder and a client that has trackers and DHT disabled at session level,
LSD disabled, and exactly one initial peer — the middle. In both runs
(`pex-20260920T175048Z`, `pex-20260920T175326Z`) the client discovered three
addresses it was never given and fetched `8,126,464` bytes / 31 pieces from
the PEX-discovered Transmission seeder at `172.18.0.3:6881`, against only
`262,144` bytes from its initial peer, returning the expected digest
`210ba6b19ee6a72f875261cd3a41d030fad18470c0fc633ee61b1a7d84174795`.
A negative control with the same isolation and no initial peer
(`pex-control-20260920T174652Z`) found no peers at all and failed on the
initialization deadline, which is what makes the discovery attributable to
peer exchange.

**Cache eviction.** Covered by [ADR 0004](0004-cache-eviction-seam.md).
Torrent-scoped eviction via delete and re-add works; evicting data underneath
a live torrent makes the engine silently serve the removed bytes, so Rustorr
owns the cache at the storage seam and evicts at torrent granularity.

**Performance.** Engine timings are repeatability evidence, never parity
evidence; the rule and the per-scenario comparison are in
[`engine-r1-metric-mapping.md`](../engine-r1-metric-mapping.md). Under R1's own
seek and magnet protocols the engine is well inside every comparable floor. A
defect in the spike's own `initialized_ms` field, which measured total run
time, was found and fixed while doing that comparison.

## Why adopt rather than fork or reject

Every capability Rustorr needs for R4 is present and evidenced in the pinned
release, through public APIs, without patching. No blocking defect was found.
Forking now would take on standing maintenance cost with no measurement
justifying it, and rejecting would discard a candidate that passed every
functional gate.

## Conditions attached to this decision

1. **Adapter isolation is mandatory.** Engine types do not appear in Rustorr's
   HTTP, cache or player layers. The spike's shape — a narrow adapter over
   `Session`, `ManagedTorrent` and `TorrentStorage` — is the production shape.
2. **The fork trigger is piece invalidation.** If R5/R11 measurement shows
   torrent-scoped eviction is too coarse, the remedy is an upstream change
   exposing piece invalidation. That is the single named condition that turns
   `adopt` into `fork`.
3. **Session-level options must be set explicitly.** `9.0.1` accepts
   `AddTorrentOptions::disable_trackers` but never reads it; only
   `SessionOptions::disable_trackers` clears the tracker list. The first PEX
   run was invalidated by exactly this, and was only caught by the negative
   control. The adapter must assert its isolation settings rather than trust
   per-torrent options, and this is worth reporting upstream.
4. **Two performance questions are carried into R4**, not closed here: the
   first read after a magnet resolution (`20597.9 ms`) and the torrent-scoped
   re-fetch (`~20.1 s`), which are close enough to suggest a shared cause in
   peer re-acquisition. Neither has an R1 floor. Seek and magnet metadata were
   re-measured under R1's own protocol and came in at `500.2 / 505.2 ms`
   against a `4512.8 ms` floor and `2855.9 ms` against a `6058.5 ms` floor, so
   the earlier "slower than TorrServer" reading was a measurement artefact,
   not a property of the engine. Parity is still only declarable through the
   Rustorr HTTP surface.
5. **The version stays pinned.** `librqbit = 9.0.1` with the committed
   `Cargo.lock`; upgrades are a deliberate change with a re-run of the spike
   gates.
