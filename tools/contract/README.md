# R2 contract characterization

The runner captures the pinned TorrServer HTTP surface as a raw, replayable
corpus. It deliberately stores status, headers, body bytes and decoded JSON;
the diff command ignores only headers explicitly listed by the manifest.

```sh
tools/r2.sh doctor
tools/r2.sh up
tools/r2.sh proxy-up
tools/r2.sh tls-up
RUSTORR_RESET_SEEDER=1 tools/r2.sh capture reference
tools/r2.sh capture candidate --base-url http://127.0.0.1:8091
tools/r2.sh diff /tmp/rustorr-contract/<run>/reference.json /tmp/rustorr-contract/<run>/candidate.json
tools/r2.sh down
tools/r2.sh proxy-down
tools/r2.sh tls-down
```

Generated captures and reports are written outside the repository under
`RUSTORR_CONTRACT_RUN_ROOT` (default `/tmp/rustorr-contract`).

Set `RUSTORR_RESET_SEEDER=1` for each independent reference run. The full
stream corpus consumes the fixture and recreating Transmission gives each run
the same peer lifecycle precondition.

The optional proxy overlay exposes nginx on `127.0.0.1:8091` (HTTP) and
`https://127.0.0.1:8443` (self-signed TLS). It forwards
`Host`, `X-Forwarded-Host`, `X-Forwarded-Proto` and `X-Forwarded-For` without
buffering stream bodies. The `tls-up` profile also starts TorrServer itself
with `--ssl` on `https://127.0.0.1:8444`; use `--insecure` for both local TLS
captures.
