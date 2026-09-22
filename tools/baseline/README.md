# R1 baseline harness

The harness measures TorrServer MatriX.145 in Linux containers and replays the
same scenario IDs against the R5 Rustorr image. The reference result remains
the baseline; the Rustorr overlay adds deterministic eviction and restart
controls without changing the reference workload IDs.

## Prerequisites

- Docker Desktop or a Linux Docker daemon
- Docker Compose v2
- Python 3.9+ on the host for the measurement client

The TorrServer image checks out commit
`2c7fa43b9ac64a9eda27314c0b6791518497f188` during its build.

## Commands

```sh
tools/baseline/r1.sh doctor
tools/baseline/r1.sh config
tools/baseline/r1.sh build
tools/baseline/r1.sh up
tools/baseline/r1.sh run
tools/baseline/r1.sh logs torrserver
tools/baseline/r1.sh down
```

Use the Rustorr release target and its restart probe with:

```sh
RUSTORR_R1_TARGET=rustorr tools/baseline/r1.sh build
RUSTORR_R1_TARGET=rustorr tools/baseline/r1.sh run
RUSTORR_R1_TARGET=rustorr tools/baseline/r1.sh restart-probe
RUSTORR_R1_TARGET=rustorr tools/baseline/r1.sh down
```

For the R5 loss-tail diagnosis, run the same focused warm Range loop once per
target. It records per-sample `tc -s qdisc` deltas and `ss -tin` TCP snapshots
from the target network namespace; this is diagnostic evidence, not an R1
gate replacement.

```sh
tools/baseline/r1.sh netem-diagnose
RUSTORR_R1_TARGET=rustorr tools/baseline/r1.sh netem-diagnose
```

Results are written to `${RUSTORR_BASELINE_RUN_ROOT:-/tmp/rustorr-baseline}`.
They are intentionally outside the repository because fixtures, logs and raw
telemetry are generated data.

## Result format

`measurement.json` uses schema `rustorr.r1.measurement.v3` and keeps raw probe
events alongside p50, p95, mean and failure counts. It is replaced atomically
after every scenario and retains `complete=false`, `last_scenario` and
`stop_reason` after interruption or failure. `docker-stats.json` and
`docker-stats-after.json` bracket the target container's CPU, memory, block
I/O and network counters. `scenarios.json` is the checked-in workload matrix.

Every Range has a 30-second curl wall-clock limit and is accepted only with
HTTP 206, exact `Content-Range`, exact byte length and the deterministic
fixture digest. Netem runs in a short-lived tooling container sharing the
target network namespace; peer departure stops the seeder between probes.
Their raw events include all control results. A completed R1 run must execute
every matrix row and add the resulting raw events and aggregates before the
stage is marked done.

Aggregate multiple runs with:

```sh
python3 tools/baseline/aggregate.py \
  --output /tmp/rustorr-baseline/baseline.json \
  /tmp/rustorr-baseline/<run>/measurement.json ...
```

Passing `--targets docs/benchmark-baseline.json` also evaluates the R5 gate
and refuses incomplete inputs. The gate requires two complete runs, all
scenario IDs, no request/integrity/precondition/control errors, the approved
cold/warm/seek-loaded/netem floors, successful peer departure, and bounded
seek-missing/seek-evicted results. Deterministic eviction latency is reported
without inventing a parity floor.

`netem-delay-loss` keeps the R1 scenario ID and the 80 ms / 1% network model,
but collects 20 Range samples in each run. Its reference floor is the maximum
of the two pinned TorrServer per-run p95 values; the candidate passes only when
each of its two per-run p95 values is within that same floor.

The aggregate records the transport-level stall proxy separately from player
rebuffering; it must not be presented as decoded playback stalls.

The reviewed aggregate is committed as
[`docs/benchmark-baseline.json`](../../docs/benchmark-baseline.json). Raw run
directories remain outside the repository and are referenced by path in that
artifact.
