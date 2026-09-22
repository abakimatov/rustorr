# R5 continuation

Status: `done 2026-09-22` by explicit product decision.

The lifecycle/cache implementation is complete and the functional evidence is
green. The approved release netem gate did not pass: Rustorr's two per-run p95
values are `1518.228 ms` and `1652.650 ms` against a `988.317 ms` floor.
The paired idle transport diagnosis found no reproducible Rustorr transport
defect, and the user explicitly accepted closing R5 with this documented
performance deviation. This is not a claim that `gate.passed=true`.

## Re-open only if investigating the performance deviation

1. Preserve the intentionally uncommitted tree on branch `r3-engine-spike`
   at base HEAD `c0e42afde075d4871e2354592747d50af89b4d6a`. Inspect it with
   `git status --short`; the R5 changes belong to this work.
2. Rebuild the benchmark image before any measurement:

   ```sh
   docker compose -f docker-compose.baseline.yml \
     -f docker-compose.r5-benchmark.yml build rustorr
   ```

   The working tree correctly uses the default 4 KiB
   `ReaderStream::new(...)`. The current local
   `rustorr-r1-rustorr:latest` image was built during the reverted 64 KiB
   experiment and does not match the source.
3. Diagnose the loss tail with matched TorrServer/Rustorr focused loops before
   another candidate pair. Record per-sample `tc -s qdisc` drop deltas and TCP
   retransmission counters outside the timed Range request. The question to
   close is whether Rustorr produces a different loss/retransmission pattern,
   not whether another random 20-request draw happens to pass.
4. After an evidence-backed transport change, run the final candidate pair:

   ```sh
   RUSTORR_R1_TARGET=rustorr tools/baseline/r1.sh run
   RUSTORR_R1_TARGET=rustorr tools/baseline/r1.sh run
   python3 tools/baseline/aggregate.py \
     --targets docs/benchmark-baseline.json \
     --output /tmp/rustorr-baseline/r5-next-aggregate.json \
     /tmp/rustorr-baseline/<run-1>/measurement.json \
     /tmp/rustorr-baseline/<run-2>/measurement.json
   ```

5. The normal performance gate remains `gate.passed=true`: both runs complete,
   zero errors, exactly 20 samples per run, and p95 at most `988.317 ms`.
   It remains failed for the historic candidate pair; R5 is closed only by the
   explicit exception recorded above.

Before handoff, run:

```sh
tools/r4.sh check
tools/r4.sh cargo test -p rustorr-server --features r5-test-control --locked
PYTHONPYCACHEPREFIX=/tmp/rustorr-pycache \
  python3 -m unittest tools/baseline/test_methodology.py
PYTHONPYCACHEPREFIX=/tmp/rustorr-pycache \
  python3 -m py_compile tools/baseline/*.py
sh -n tools/baseline/r1.sh
docker compose -f docker-compose.baseline.yml config --quiet
docker compose -f docker-compose.baseline.yml \
  -f docker-compose.r5-benchmark.yml config --quiet
git diff --check
```

Stop/remove only the R1/R5 services and preserve `r2proxy` and `r2tls`:

```sh
docker compose -f docker-compose.baseline.yml \
  -f docker-compose.r5-benchmark.yml \
  stop rustorr torrserver seeder tracker fixture
docker compose -f docker-compose.baseline.yml \
  -f docker-compose.r5-benchmark.yml \
  rm -f rustorr torrserver seeder tracker fixture
```

Current runtime state: all R1/R5 services are stopped and removed. `r2proxy`
and `r2tls` are still running. The raw reference, candidate, and restart
artifacts listed below currently exist under `/tmp`; the committed source of
truth for the accepted floor is `docs/benchmark-baseline.json` if `/tmp` is
later cleaned.

## Valid evidence

- `tools/r4.sh check` passes the seven-crate boundary guard, rustfmt, clippy
  with `-D warnings`, and all 179 workspace tests.
- `tools/r4.sh cargo test -p rustorr-server --features r5-test-control
  --locked` passes all 20 server tests with the benchmark-only control
  feature enabled.
- Two independent release measurements under the rebaselined method completed
  every original R1 scenario:
  `/tmp/rustorr-baseline/20260921T164519Z/measurement.json` and
  `/tmp/rustorr-baseline/20260921T164721Z/measurement.json`. Each reports
  `complete=true`, zero request/integrity/precondition/control errors, and
  valid HTTP 206 bodies, lengths, `Content-Range` values and fixture digests.
- Their gate aggregate is
  `/tmp/rustorr-baseline/r5-rebaseline-aggregate.json`. Passing p95 checks are
  cold known `8764.526 <= 10911.5 ms`, warm known `4.343 <= 4976.2 ms`, and
  seek-loaded `392.656 <= 4512.8 ms`. Seek-missing is bounded at `519.657 ms`
  and deterministic seek-evicted at `9029.849 ms`; the latter is reported
  without a parity floor. Peer-departure recovery is `1.966 ms` with no
  failure.
- The added diagnostic `magnet_first_range_ms` is `4529.068 ms` p95. It has no
  parity floor. Magnet metadata discovery itself is `11085.005 ms` p95.
- `/tmp/rustorr-baseline/20260921T151011Z-restart/restart-probe.json` proves
  restart recovery: before and after restart both return HTTP 206 and digest
  `31e67ed8a3c058d5d68dfac1cd83c24b6ade45ffea0f7833d8eaa3951eae643b`;
  the second read is served after the seeder is stopped in `3.263 ms`.

## Blocking scenario

The approved method keeps the scenario ID and 80 ms / 1% model, collects 20
Range samples per run, and uses the maximum of two pinned TorrServer per-run
p95 values as the symmetric floor. Reference runs
`20260921T163650Z` and `20260921T163912Z` produced `988.317 ms` and
`986.719 ms`, setting the floor to `988.317 ms`; their aggregate is
`/tmp/rustorr-baseline/netem-reference-aggregate.json`.

Rustorr runs `20260921T164519Z` and `20260921T164721Z` produced per-run p95
values of `1518.228 ms` and `1652.650 ms`. Both collected exactly 20 correct
HTTP 206 ranges and all netem apply/clear controls succeeded, but both exceed
the reference floor. Every other close-gate check passes.

### Netem gate diagnosis

A focused 20-request cached-Range loop established that the original
single-request `579.7 ms` floor was invalid. It does not override the current
repeated-sample gate or prove that Rustorr's current tail is equivalent:

- Rustorr: p95 `754.453 ms`, 18/20 samples above `579.7 ms`, about 184–186
  response packets per sample.
- TorrServer MatriX.145 under the same 80 ms / 1% control: p95 `1334.163 ms`,
  18/20 samples above `579.7 ms`, about 185–194 packets per sample.
- With zero qdisc drops, medians are `585.387 ms` for Rustorr and
  `589.802 ms` for TorrServer. One-to-three random drops produce the same
  `~0.65–1.5 s` retransmission tail on both servers.

Raw focused results are `/tmp/rustorr-netem-before.json` and
`/tmp/torrserver-netem-reference.json`. The committed R1 artifact confirms why
the floor is unstable: `docs/benchmark-baseline.json` records
`netem-delay-loss.runs = 1`; `579.7 ms` is one controlled request, not a p95
from the claimed two-run sample. The other reference run records an
uncontrolled wrapper placeholder and cannot contribute to this floor.

The repeated-sample methodology was explicitly approved and is now implemented
in the harness. An additional Rustorr loop that recorded qdisc counters found
the long requests coinciding with random drops (for example one drop at
`1339.305 ms` and four at `1505.572 ms`) while server traces still completed
request setup in `0 ms`. This supports a network retransmission tail, but it
does not override the accepted comparison. The next action is to improve or
further isolate Rustorr's HTTP/TCP body delivery under loss and then produce
two new complete candidate runs; the current failed pair remains evidence.

### 2026-09-22 paired idle transport diagnosis

`tools/baseline/netem_diagnose.py` is a benchmark-only focused loop invoked by
`tools/baseline/r1.sh netem-diagnose`. It waits for the target API, wipes and
re-adds the fixture, warms offsets `0` and `4194304`, then requires two quiet
network-interface intervals before applying the unchanged `80 ms / 1%` model.
For every one of 20 timed Range requests it records the `tc -s qdisc` delta
and multiple `ss -tin` TCP observations from the target network namespace.
Missing qdisc or TCP observations make an artifact incomplete.

Two complete, idle-gated artifacts are:

- TorrServer: `/tmp/rustorr-baseline/20260922T062935Z-netem-diagnose-torrserver/netem-diagnosis.json`;
  p95 `727.218 ms`. Its 12 zero-drop samples are `484.380–583.170 ms`; its
  loss distribution is 12 zero, 4 one, 3 two and 1 five-drop sample.
- Rustorr: `/tmp/rustorr-baseline/20260922T063305Z-netem-diagnose-rustorr/netem-diagnosis.json`;
  p95 `1488.191 ms`. Its 7 zero-drop samples are `485.387–573.240 ms`; its
  random loss distribution is 7 zero, 5 one, 6 two and 2 three-drop samples.

The HTTP packet budgets are stable (TorrServer median `197`, Rustorr median
`185` packets) and both targets record TCP retransmissions when qdisc drops.
The focused loop therefore isolates the observed tail to the random netem/TCP
loss sequence rather than a reproducible Rustorr pre-send or warm-cache delay.
It is not a replacement for the approved release harness and does not change
the `988.317 ms` floor. It supplies the evidence for the explicit product
decision to close R5 with the failed gate recorded as a deviation; a future
transport change must still pass two complete release runs under that gate.

## Implemented

- The object-safe `Engine` port uses boxed futures for add/read/status/delete;
  `TorrentCoordinator` depends on `Arc<dyn Engine>`. The librqbit registry is
  changed only after successful session deletion.
- Eviction cancels and joins owned prefetch work, refuses a live playback pin
  before mutating engine/cache/SQLite, preserves live peer addresses for lazy
  re-add, and drops those hints on normal removal. Reader release and prefetch
  completion serialize soft-cap enforcement. Structured events record the
  reason, freed/remaining/cap bytes, pin state, peer-hint count and re-add time.
- Repeated add returns the existing catalog entry without changing
  `added_at` or repeating an engine add. Runtime `stat` and
  `connected_seeders` come from librqbit status; R5 counts live peers.
- Disk recovery persists atomic layouts, extents and verified pieces. A new
  engine can read a recovered range without a peer; corrupt or mismatched
  manifests are discarded.
- The measurement client bounds curl and subprocess time, atomically records
  partial state, fails fast on preconditions, and validates every Range byte.
  Fixture generation and validation share one deterministic payload helper.
  Netem is applied by the existing tooling image in the target network
  namespace, keeping `tc` out of the production image.
- `r1.sh` checks curl, always reports the output directory, captures final
  Docker stats, and exposes the separate restart probe. The benchmark-only
  Unix socket provides deterministic eviction; no production HTTP route does.

## Discarded attempts

- `/tmp/rustorr-baseline/20260921T124248Z` exposed the original prefetch-pin
  race and is interrupted evidence, not a result.
- `20260921T142018Z`, `20260921T142234Z`, `20260921T143456Z`, and
  `20260921T143845Z` are incomplete magnet add/first-Range diagnostics. They
  led to cancelling prefetch before lifecycle changes and to short-circuiting
  repeated adds; none can close the gate.
- `20260921T144417Z` reached netem but stopped on a missing `tc` binary in the
  Rustorr image. The harness now uses a dedicated tooling container; this
  attempt remains invalid.
- `20260921T145202Z` and `20260921T145337Z` were complete pre-final-interface
  runs, but their aggregate also failed netem (`1278.014 ms`) and they are
  superseded by the boxed-status final image runs above.
- `20260921T150358Z` exposed a foreground/prefetch priority race: the response
  returned 206 headers but no body inside 30 seconds. Prefetch now waits 50 ms
  after registration so the playback reader expresses demand first; a unit
  test fixes that ordering and both final full runs pass cold Range.
- `20260921T164452Z` did not begin measurement because the reference
  TorrServer still owned host port 8090. Only the R1 target services were
  removed, preserving the unrelated R2 proxy/TLS containers, before the two
  valid candidate runs.
- A 64 KiB `ReaderStream` experiment was tested with complete runs
  `20260921T165840Z` and `20260921T170032Z`. Their netem p95 values were
  `1161.103 ms` and `831.574 ms`: one still failed, while p50 regressed from
  about `589 ms` to `661–662 ms`. The change was reverted and the aggregate
  `/tmp/rustorr-baseline/r5-buffered-aggregate.json` is diagnostic only.
- The R3 engine-spike timings are not substituted for HTTP parity evidence.

## Working-tree provenance

- Branch: `r3-engine-spike`; base HEAD: `c0e42af`; the tree remains
  intentionally uncommitted and unpushed.
- New paths include `crates/rustorr-lifecycle/`,
  `docker-compose.r5-benchmark.yml`, `tools/baseline/fixture_payload.py`,
  `tools/baseline/restart_probe.py`, and this handoff.
- Raw benchmark and restart artifacts stay under `/tmp/rustorr-baseline/` and
  are not committed.
