# Mapping R3 engine measurements to the R1 metrics

Status: `accepted as the comparison rule for R3`

R3 measures an in-process librqbit read. R1 measured TorrServer over HTTP.
The two are not the same quantity, so this document defines exactly how R3
numbers may and may not be used, and what work closes the gap.

## What each side actually measures

| | R1 baseline | R3 engine spike |
| --- | --- | --- |
| Surface | HTTP `GET`/Range against TorrServer in Docker | `handle.stream(file).seek().read_exact(n)` in-process |
| Path included | container network, HTTP framing, TorrServer cache and piece scheduling | engine piece scheduling and storage only |
| State protocol | explicit `empty`/`warm`/`loaded`/`missing`/`evicted` preconditions per scenario | fresh session per run, plus one same-output fast-resume case |
| Client start | measured by the R1 runner | not measured |
| Player start / rebuffering | not measured (transport stalls are a proxy only) | not measured |
| Resource sample | TorrServer container | spike container, no HTTP or cache layer |

Because the R3 path is a strict subset of the R1 path, an R3 timing is a
**lower bound** on what the same scenario would cost through Rustorr's HTTP
surface.

## The comparison rule

1. R3 timings may be used to **rule a candidate out**: if the engine alone
   cannot beat an R1 floor, the full Rustorr path cannot either.
2. R3 timings may **never** be used to declare parity, because they omit the
   HTTP, cache and player layers that the floors include.
3. A comparison is only meaningful when the R1 scenario's state precondition
   matches the R3 run's state. Cold-session R3 runs must not be compared to
   `warm` or `loaded` R1 rows.
4. Parity is declared only by running `tools/baseline/r1.sh` against the
   Rustorr HTTP surface, producing the same scenario IDs as
   `tools/baseline/scenarios.json`. That is R4/R5 work, not R3 work.

Every spike run already copies `scenarios.json` beside its raw output, so the
future matrix can be aggregated under the same scenario IDs.

## Applying the rule to the current evidence

Floors are from [`benchmark-baseline.json`](benchmark-baseline.json)
(`rustorr_targets`); engine numbers are from the repeat matrix in
[`engine-spike.md`](engine-spike.md).

| R1 scenario | Floor | Closest R3 run | R3 value | State match | Verdict under the rule |
| --- | ---: | --- | ---: | --- | --- |
| `cold-known-torrent` | p95 ≤ 10911.5 ms | cold known torrent, 1 view | 5426.1 / 8814.7 ms | yes (both empty) | not ruled out |
| `warm-known-torrent` | p95 ≤ 4976.2 ms | same-output fast resume | 3.7 ms | approximate only | not ruled out |
| `seek-loaded` | p95 ≤ 4512.8 ms | warm-up read then seek, R1 protocol | 500.2 / 505.2 ms | yes | not ruled out, with a wide margin |
| `seek-missing` | 7995.3 ms (reference) | warm-up read then seek, R1 protocol | 500.2 / 505.2 ms | yes | not ruled out |
| `seek-evicted` | 8507.9 ms (reference) | session delete + re-add re-fetch | ~20100 ms | **no** — different operation | no verdict; R3 evicts a whole torrent, R1 evicted nothing deterministically |
| `continuous-three-views` | p95 ≤ 10.6 ms | three concurrent views, cold session | 4233.4–8764.1 ms | **no** — R3 was cold | no verdict; must be re-measured warm |
| `cold-magnet-metadata` | p95 ≤ 6058.5 ms | magnet metadata resolution | 2855.9 ms | yes | not ruled out |
| `http_failed_requests` | 0 | n/a — no HTTP surface in R3 | — | no | only R4 can answer |
| `netem-delay-loss` | per-run p95 ≤ 988.317 ms, 20 samples/run | not exercised in R3 | — | no | only R4 can answer |
| `peer-departure` | 206, no unexplained failure | not exercised in R3 | — | no | only R4 can answer |

Resource comparison: TorrServer sampled `25.89 MiB` → `60 MiB` RSS; the spike
sampled `6.16–6.52 MiB` RSS at `0.22–1.78%` CPU for three views. The spike
carries no HTTP or cache layer, so this is a floor on Rustorr's footprint, not
a prediction of it.

## How the earlier flags were resolved

An earlier revision of this document flagged cold seek, magnet metadata and
re-fetch as sitting above their R1 references. Two of the three were artefacts
of comparing different quantities, and re-measurement under R1's own protocol
removed them:

- **Seek.** R1 times a seek only *after* the torrent is added, metadata is
  resolved, a seeder is connected and a first range has already been served.
  The R3 seek runs measured all of that inside the number. Re-running with
  `--warmup-offset 0` reproduces R1's protocol:
  `/tmp/rustorr-engine-spike/20260920T181105Z` and `20260920T181129Z` give
  `500.2 ms` and `505.2 ms` against a `4512.8 ms` floor.
- **Magnet.** R1's `metadata_discovery_ms` stops when the torrent is listed
  with a connected seeder; it never includes a read. The R3 number included a
  full 256 KiB read. Measured on its own,
  `/tmp/rustorr-engine-spike/20260920T181310Z` resolved magnet metadata in
  `2855.9 ms` against a `6058.5 ms` floor.
- **Re-fetch.** This one is not resolved and is not comparable either: R3
  deletes and re-downloads an entire torrent (~20.1 s), while R1's
  `seek-evicted` row only establishes a cold piece map, because TorrServer
  exposes no deterministic eviction API. See
  [ADR 0004](adr/0004-cache-eviction-seam.md).

Two genuine open questions survive, both to be answered in R4:

1. The first read after a magnet resolution took `20597.9 ms`, far longer than
   the `3.8 s` first read from a `.torrent` file. R1 never measured this, so
   there is no floor for it, but the gap is real and unexplained.
2. Torrent-scoped re-fetch costs about `20.1 s`, close enough to the magnet
   figure above to suggest a shared cause — most likely peer re-acquisition
   after a re-add. Worth diagnosing before the cache cap is sized.

### A measurement defect found while doing this

The spike's `initialized_ms` field was computed at the end of the run, so it
reported add + initialize + read + every later phase, not initialization. It
now stops right after `wait_until_initialized`, and a separate `total_ms`
carries the full duration. Real initialization is `1.4 ms`; every figure
labelled "initialized ms" in the earlier repeat matrix was effectively total
run time, which is why those numbers sat so close to their read times.

## What closes the gap

1. R4 builds the Rustorr HTTP Range surface over the engine adapter.
2. `tools/baseline/r1.sh` runs unchanged against that surface, producing rows
   with the same scenario IDs and the same state preconditions.
3. Those rows — not the R3 probe timings — are compared to
   `rustorr_targets`.
4. Player-level start and rebuffering remain a separate characterization task
   in both R1 and R4; transport stalls stay labelled as a proxy.
