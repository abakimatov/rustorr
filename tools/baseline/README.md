# R1 baseline harness

The harness measures TorrServer MatriX.145 in Linux containers. It is the
baseline for later Rustorr and BitTorrent-engine comparisons; it does not
contain Rustorr code and does not select an engine.

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

Results are written to `${RUSTORR_BASELINE_RUN_ROOT:-/tmp/rustorr-baseline}`.
They are intentionally outside the repository because fixtures, logs and raw
telemetry are generated data.

## Result format

`measurement.json` uses schema `rustorr.r1.measurement.v2` and keeps raw probe
events alongside p50, p95, mean and failure counts. `docker-stats.json` is a
snapshot of the TorrServer container's CPU, memory, block I/O and network
counters. `scenarios.json` is the checked-in workload matrix; the current
runner executes the matrix and records the matrix beside each run. Missing
piece and eviction preconditions are recorded explicitly; netem and peer
departure rows apply `tc netem` and stop the seeder between probes; their raw
events include the control command results. A completed
R1 run must execute every matrix row and add the resulting raw events and
aggregates before the stage is marked done.

Aggregate multiple runs with:

```sh
python3 tools/baseline/aggregate.py \
  --output /tmp/rustorr-baseline/baseline.json \
  /tmp/rustorr-baseline/<run>/measurement.json ...
```

The aggregate records the transport-level stall proxy separately from player
rebuffering; it must not be presented as decoded playback stalls.

The reviewed aggregate is committed as
[`docs/benchmark-baseline.json`](../../docs/benchmark-baseline.json). Raw run
directories remain outside the repository and are referenced by path in that
artifact.
