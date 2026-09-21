# R4 architecture

Status: `accepted 2026-09-21`.

## Components and ownership

| Component | Owns | Does not own |
| --- | --- | --- |
| `rustorr-domain` | hashes, indexes, ranges, domain errors | IO and serialization |
| `rustorr-engine` | live librqbit session, peer transport, DHT file | cache policy, product state |
| `rustorr-cache` | piece bytes, residency, LRU and pins | engine lifecycle |
| `rustorr-state` | `rustorr.db`, schema and product records | live torrents |
| `rustorr-http` | routes, DTOs, HTTP error mapping | concrete implementations |
| `rustorr-server` | config and startup/shutdown composition | reusable policy |

The permitted dependency graph is enforced by `tools/r4.sh boundaries`; its
normative decision is [ADR 0005](adr/0005-crate-boundaries-and-engine-isolation.md).

## Runtime lifecycle

Startup installs signal handlers, binds HTTP, creates the data directory,
opens SQLite, constructs cache, then starts the engine. Shutdown stops HTTP,
then engine, then releases cache and state. HTTP drain is bounded by
`RUSTORR_SHUTDOWN_GRACE` (5 seconds by default); the outer Tokio runtime has a
separate two-second shutdown deadline.

HTTP layers are ordered outer-to-inner as tracing, future access controls
(CORS/WAF/auth), then panic catching. This keeps an R6 access layer outside
the panic response and visible to tracing.

## Data directory

```
<data>/
  rustorr.db             SQLite source of truth
  engine/dht.json        disposable librqbit DHT routing cache
  engine/scratch/        engine transient files
  cache/<hash>/<file>    Rustorr-owned piece storage
```

`rustorr.db` uses WAL and schema/application identifiers (ADR 0006). Cache
eviction is torrent-scoped: the coordinator in R5 must delete a session before
removing the cache entry, because deleting bytes beneath a live torrent can
silently corrupt reads (ADR 0004).
