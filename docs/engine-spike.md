# R3 engine spike

Status: `all R3 gates closed; ADR 0003 records adopt`

For a new-session continuation point, start with
[`docs/r3-continuation.md`](r3-continuation.md).

The spike is isolated under `tools/engine-spike/` and currently targets
`librqbit` `9.0.1`. It does not implement Rustorr HTTP routes or production
cache behavior. The probe records metadata, file mapping, positional reads,
concurrent views, seek latency, cancellation, persistence configuration and
custom storage read/write callbacks.

## Reproducible run

The Docker overlay reuses the R1 fixture, tracker and seeder:

```sh
tools/engine-spike/run.sh doctor
tools/engine-spike/run.sh config
tools/engine-spike/run.sh build
tools/engine-spike/run.sh probe --offset 0 --length 262144
tools/engine-spike/run.sh probe --offset 4194304 --length 262144 --custom-storage
tools/engine-spike/run.sh probe --magnet 'magnet:?xt=urn:btih:68c3ccdd52b2925f4f97e2f61ea248e728e54cea&dn=single&tr=http%3A%2F%2Ftracker%3A6969%2Fannounce' --disable-dht
tools/engine-spike/run.sh probe --views 3 --disable-dht
tools/engine-spike/run.sh probe --utp-only --disable-dht
tools/engine-spike/run.sh probe --custom-storage --delete-and-refetch
tools/engine-spike/run.sh probe --disable-dht --custom-storage --evict-after-read \
  --offset 4194304 --length 262144
tools/engine-spike/run.sh probe --disable-dht --warmup-offset 0 \
  --offset 4194304 --length 262144
tools/engine-spike/run.sh pex --offset 4194304 --length 262144 --pex-observe-ms 90000
tools/engine-spike/run.sh down
```

Raw results are written outside the repository to
`${RUSTORR_ENGINE_SPIKE_RUN_ROOT:-/tmp/rustorr-engine-spike}`. The runner
stores the R1 scenario matrix beside each probe so future matrix aggregation
can use the same scenario IDs and parity floors.

## Current gates

- Metadata and file mapping must be available after initialization.
- Returned bytes must match the expected SHA-256 when supplied.
- Seek and positional reads must complete without failed requests.
- `--cancel-after-ms` must drop the stream without keeping the process alive.
- `--persistence PATH` enables the candidate's session persistence path for
  restart experiments.
- `--custom-storage` must produce storage callbacks.
- `--views N` must complete N concurrent positional reads in one session and
  return the expected digest for every view.
- `--delete-and-refetch` must delete and re-add the torrent, then return the
  same bytes after a fresh fetch. This is a session delete/re-add gate, not a
  piece-level eviction proof: `librqbit 9.0.1` exposes no public API for
  evicting individual pieces behind its chunk tracker.
- `--evict-after-read` zeroes the read range on disk under a live torrent and
  reads it again. This gate is diagnostic, not pass/fail: it records what the
  engine does when Rustorr-owned storage loses data.
- `run.sh pex` must show a client with session-level trackers and DHT disabled
  discovering, and fetching from, a peer it was never given. The matching
  no-initial-peer control must fail to find any peer.

The decision is recorded in [ADR 0003](adr/0003-bittorrent-engine-selection.md):
`adopt`, pinned at `librqbit = 9.0.1`, behind the Rustorr adapter.

## Runtime evidence 2026-09-20

All probes ran in Docker with the R1 tracker/seeder, DHT disabled and bounded
initialization/read timeouts. Raw output is under
`/tmp/rustorr-engine-spike/` and is intentionally not committed.

- Known torrent, default filesystem storage: two fresh independent runs
  (`20260920T163634Z` and `20260920T164148Z`) returned the expected
  65,536-byte SHA-256
  `31e67ed8a3c058d5d68dfac1cd83c24b6ade45ffea0f7833d8eaa3951eae643b`;
  each run observed one live TCP peer and one downloaded piece.
- Seek/read: a fresh read at offset `4194304` returned the expected SHA-256
  `210ba6b19ee6a72f875261cd3a41d030fad18470c0fc633ee61b1a7d84174795`.
- Three concurrent views each returned the expected bytes and observed the
  seeded peer; initialization/read completed in about `9.92 s` per view.
- Custom storage: the same known-torrent read succeeded with recording
  callbacks (`creates=1`, `inits=1`, `takes=1`, `writes=20`, `reads=6`,
  `completed_pieces=1`).
- Delete/re-fetch: delete plus re-add succeeded and returned the same hash;
  counters showed two storage creations, two initializations and two completed
  pieces. The re-fetch took about `20.1 s`, so it is functional evidence, not
  yet a performance acceptance point.
- Cancellation: a read cancelled after `1 ms` returned a cancelled result in
  about `2 ms` and the process exited cleanly.
- Magnet metadata: a magnet with the local tracker resolved metadata and
  returned the same expected digest; the whole run — add, resolve and read —
  took about `25.2 s`. Resolution on its own was measured separately later at
  `2855.9 ms`; see the state-matched re-measurement below.
- Persistence restart: two processes reused the same persistence directory;
  when the second process reused the original output path, initialization
  restored `262144` bytes and the read took `3.7 ms` with no live peer. This
  proves a useful fast-resume path for the same output location, not a general
  cache eviction policy.

The baseline seeder healthcheck intentionally requires the two known R1
fixtures used by this spike. The Unicode fixture remains `0% None` in
Transmission and is not used as an R3 runtime input.

## Runtime evidence 2026-09-21

The capability probe at
`/tmp/rustorr-engine-spike/20260920T170225Z/probe.json` ran with DHT enabled,
the local tracker and `--enable-utp-listener`. It reported DHT enabled with
zero routing-table entries (expected for the isolated network without a
bootstrap node), bound a TCP+uTP listener at `0.0.0.0:46849`, and returned the
expected read digest. The peer used TCP; `live_utp=0`, so this is listener
evidence, not an actual uTP peer-transfer result.

The uTP-only probe at
`/tmp/rustorr-engine-spike/20260920T171757Z/probe.json` forced TCP off and
completed the same read with the expected digest. Its peer snapshot reported
`live_utp=1` and `live_tcp=0`; actual uTP transfer is therefore evidenced for
the local Transmission seeder.

The two-process DHT bootstrap/client probe is under
`/tmp/rustorr-engine-spike/dht-two-peer-20260921T172000Z/`. The client used
the bootstrap node at `r3-dht-bootstrap-1720:39000`, finished with
`routing_table_size=58` and `outstanding_requests=41`, and returned the
expected digest. This closes DHT enablement and isolated bootstrap discovery;
it does not prove peer exchange.

The persisted-session relocation probe is under
`/tmp/rustorr-engine-spike/persistence-restore-20260921T170500Z/`. After the
first process wrote `output-a`, the output directory was moved to `output-b`
before the second process reused the persistence folder. Both reads were
correct, but the second process took `8772.6 ms`, saw a live TCP peer and
downloaded a piece again. This does not establish general persisted piece
restore; same-output fast-resume remains the only positive persistence result.

The repeat matrix is under
`/tmp/rustorr-engine-spike/repeat-20260921T170700Z/`:

The `initialized ms` column below is mislabelled: the field was computed at
the end of the run, so it reports total run time, not initialization. It was
fixed afterwards; see the state-matched re-measurement further down.

| scenario | repeats | total ms (labelled "initialized") | read ms | result |
| --- | ---: | ---: | ---: | --- |
| cold known torrent, one view | 2 | 5426.1, 8814.7 | 5424.3, 8813.4 | both expected digests |
| seek at 4194304, one view | 2 | 8770.4, 8778.0 | 8768.8, 8776.1 | both expected digests; cold session, so not R1's seek protocol |
| concurrent views in one session | 2 | 8766.0, 4236.3 | 8763.2–8764.1, 4233.4–4234.7 | all 6 digests correct |

These engine read timings are not HTTP Range or decoded-player startup
measurements and are not substituted for the R1 TorrServer floors. They are
repeatability evidence for the adapter path. A Docker resource sample for the
three-view run is in
`/tmp/rustorr-engine-spike/repeat-20260921T170700Z/resource-three/`: RSS
`6.16–6.52 MiB`, CPU `0.22–1.78%`, and final network I/O
`641 kB / 7.22 kB`.

Source inspection confirms that librqbit 9.0.1 implements `ut_pex` and exposes
uTP/DHT stats. The spike still has no public PEX message counter, so the
peer-exchange gate below uses peer discovery and per-peer transfer counters as
the observable outcome instead.

## Runtime evidence 2026-09-21 — peer exchange

`run.sh pex` builds a three-node topology on the R1 network: the Transmission
seeder, a `pex-middle` librqbit peer that knows the tracker and listens on a
fixed port `46900`, and a `pex-client` that has DHT off, LSD off, trackers
disabled at session level, and exactly one initial peer — `pex-middle`. Any
other address the client ends up holding can only have come over PEX. The
middle peer is throttled (`--upload-limit-bps 65536`,
`--download-limit-bps 131072`) so that it is still actively downloading, and
therefore still has a live outgoing peer to advertise, while the client is
attached. The probe flags behind this are `--initial-peers HOST:PORT,...`,
`--disable-trackers`, `--listen-port`, `--upload-limit-bps`,
`--download-limit-bps`, `--pex-observe-ms`, `--wait-complete-ms` and
`--result-file`; `rustorr-engine-spike --help` lists every flag.

| run | discovered peers | bytes from PEX-discovered seeder | bytes from the initial peer | digest |
| --- | ---: | ---: | ---: | --- |
| `pex-20260920T175048Z` | 3 | 8,126,464 (31 pieces) | 262,144 (1 piece) | expected |
| `pex-20260920T175326Z` | 3 | 8,126,464 (31 pieces) | 262,144 (1 piece) | expected |

The negative control `pex-control-20260920T174652Z` ran the same client with
no initial peer and failed with `deadline has elapsed`, having found no peers
at all. That control is what makes the result attributable: an earlier
attempt, `pex-20260920T174321Z`, looked like a PEX success but was invalid,
because `librqbit 9.0.1` ignores `AddTorrentOptions::disable_trackers` and the
"hidden" client was still announcing to the tracker. Only
`SessionOptions::disable_trackers` clears the tracker list. Two further runs
are recorded but superseded: `pex-20260920T173959Z` (same tracker leak) and
`pex-20260920T174800Z`, where the middle peer had already completed its
download, dropped the seeder from its live outgoing peer set, and so had
nothing to advertise — the client discovered nobody.

## Runtime evidence 2026-09-21 — storage eviction under a live torrent

`--evict-after-read` reads a range, verifies its digest, zeroes exactly that
range on disk while the torrent stays live, and reads it again through the
same handle.

| run | first read | after eviction | recovered |
| --- | --- | --- | --- |
| `20260920T175558Z` | `210ba6b1…` in 3870.2 ms | `8a39d2ab…` in 1.2 ms | no |
| `20260920T175634Z` | `210ba6b1…` in 3859.0 ms | `8a39d2ab…` in 0.9 ms | no |

The engine neither errored nor re-fetched: it served the zeroed bytes. This
result is what fixes Rustorr's eviction granularity at the torrent level in
[ADR 0004](adr/0004-cache-eviction-seam.md).

## Runtime evidence 2026-09-21 — state-matched re-measurement

Two flagged rows turned out to be comparisons between different quantities.
R1 times a seek only after the torrent is added, metadata resolved, a seeder
connected and a first range served; R1's magnet metric stops at "listed with a
connected seeder" and never includes a read. Re-running under those protocols:

| measurement | R3 result | R1 reference | source |
| --- | ---: | ---: | --- |
| seek after a warm-up read (`--warmup-offset 0`) | 500.2 / 505.2 ms | `seek-loaded` 4512.8 ms | `20260920T181105Z`, `20260920T181129Z` |
| magnet metadata resolution only | 2855.9 ms | `cold-magnet-metadata` 6058.5 ms | `20260920T181310Z` |
| real initialization, `.torrent` input | 1.4 ms | n/a | same runs |

Two things did not resolve and are carried into R4: the first read after a
magnet resolution took `20597.9 ms` versus `3.8 s` from a `.torrent` file, and
a torrent-scoped re-fetch costs about `20.1 s`. The similarity suggests a
shared cause in peer re-acquisition after a re-add; neither has an R1 floor.

The probe now reports `initialized_ms` correctly and adds `total_ms` and
`warmup_read`.

## Mapping to the R1 metrics

The comparison rule and the per-scenario table live in
[engine-r1-metric-mapping.md](engine-r1-metric-mapping.md). In short: engine
timings are a lower bound on the full Rustorr path, so they may rule a
candidate out but never declare parity, and they are only meaningful where the
R1 state precondition matches. Three rows — cold seek, magnet metadata and
torrent-scoped re-fetch — sit above their closest R1 reference and are carried
into R4 as open performance risks.

## Checkpoint 2026-09-21

Completed and verified:

- pinned Rust toolchain declaration `1.90.0`;
- pinned dependency `librqbit = 9.0.1`;
- generated `tools/engine-spike/Cargo.lock` with 362 resolved packages;
- Docker Compose overlay validation;
- release build inside Docker's internal filesystem;
- binary startup and `--help` output;
- shell syntax, rustfmt, clippy with `-D warnings` and `git diff --check`.

The release build completed as `rustorr-engine-spike` inside the
`rustorr-r1-engine-spike` image. The first build attempt through a host bind
mount hit a rustc SIGBUS before project diagnostics, so the Dockerfile now
builds in the image layer to avoid that filesystem path.

The earlier interrupted probe at
`/tmp/rustorr-engine-spike/20260920T153748Z` remains invalid and is not used.
The first bounded batch also used a stale seeder volume and is excluded from
the evidence above; the fresh batch was run after the fixture/seeder restart.
The subsequent bounded runs listed above are valid functional and repeatability
evidence. No HTTP/player performance parity and no piece-level
storage-eviction capability has been accepted. The DHT bootstrap, uTP-transfer
and peer-exchange gates are evidenced separately in the runtime artifacts
above, and the engine-selection conclusion is recorded in ADR 0003.

## Checkpoint 2026-09-21 — R3 closed

The three remaining gates are now addressed: peer exchange is evidenced with a
negative control, the cache-eviction seam is decided in ADR 0004, and the
engine-to-R1 comparison rule is written down. ADR 0003 records `adopt`.

Still not established, and deliberately carried forward:

- no HTTP Range or decoded-player measurement exists for the candidate; the R1
  floors remain unmatched by anything in R3;
- the first read after a magnet resolution and the torrent-scoped re-fetch
  both cost about 20 s and are unexplained; cold seek and magnet metadata were
  re-measured state-matched and are comfortably under their R1 references;
- piece-level eviction remains impossible without an upstream change, which is
  the named fork trigger.
