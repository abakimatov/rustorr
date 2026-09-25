#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
IMAGE=${RUSTORR_R4_IMAGE:-rustorr-r4-dev}
# Build output and the cargo cache live in named volumes: a bind-mounted target
# directory made rustc crash with SIGBUS during R3. Only sources are mounted.
TARGET_VOLUME=${RUSTORR_R4_TARGET_VOLUME:-rustorr-r4-target}
CARGO_VOLUME=${RUSTORR_R4_CARGO_VOLUME:-rustorr-r4-cargo}

usage() {
  printf '%s\n' "usage: $0 {doctor|image|check|web-check|web|e2e|boundaries|test|build|cross-build|smoke|cargo|clean}"
  printf '%s\n' "       cargo runs any cargo command in the toolchain container, e.g. cargo generate-lockfile"
}

doctor() {
  command -v docker >/dev/null 2>&1 || { echo "error: docker CLI is not installed" >&2; return 1; }
  docker info >/dev/null 2>&1 || { echo "error: Docker daemon is unavailable" >&2; return 1; }
}

image() {
  doctor
  docker build -t "${IMAGE}" -f "${ROOT}/tools/r4/Dockerfile.dev" "${ROOT}/tools/r4"
}

ensure_image() {
  doctor
  docker image inspect "${IMAGE}" >/dev/null 2>&1 || image
}

in_dev() {
  ensure_image
  docker run --rm \
    -v "${ROOT}:/workspace" \
    -v "${TARGET_VOLUME}:/cache/target" \
    -v "${CARGO_VOLUME}:/cache/cargo" \
    -e CARGO_TARGET_DIR=/cache/target \
    -e CARGO_HOME=/cache/cargo \
    -e CARGO_TERM_COLOR=never \
    -w /workspace \
    "${IMAGE}" "$@"
}

# Crate boundaries (docs/r4-plan.md) and the audit result that OpenSSL stays out
# of the dependency tree on every target: librqbit is built with rust-tls, which
# removes it. openssl-probe, a pure-Rust certificate locator, is not OpenSSL.
boundaries() {
  ensure_image
  in_dev cargo metadata --format-version 1 --no-deps --locked | python3 "${ROOT}/tools/r4/check-boundaries.py"
  for crate in openssl openssl-sys; do
    tree=$(in_dev cargo tree --workspace --locked --target all -i "${crate}" 2>&1 || true)
    case "${tree}" in
      *"did not match any packages"*) ;;
      openssl*) echo "error: ${crate} is in the dependency tree:" >&2; echo "${tree}" >&2; return 1 ;;
      *) echo "error: could not inspect the dependency tree:" >&2; echo "${tree}" >&2; return 1 ;;
    esac
  done
}

# The web interface (R8) in Node's image. node_modules lives in a volume of
# its own, so the container's Linux packages never replace the host's.
WEB_IMAGE=${RUSTORR_WEB_IMAGE:-node:24-bookworm-slim}
in_web() {
  doctor
  docker run --rm \
    -v "${ROOT}/web:/web" \
    -v rustorr-r8-node-modules:/web/node_modules \
    -v rustorr-r8-npm-cache:/root/.npm \
    -w /web \
    "${WEB_IMAGE}" "$@"
}

# Type check, lint, tests and the production build, which leaves web/dist for
# rustorr-http to embed.
web_check() { in_web sh -c 'npm ci --no-audit --no-fund && npm run check'; }

# The R8 browser scenarios (web/e2e) against Rustorr with GStreamer on the
# fixture stand: the tracker and the seeder are restarted first, as the
# contract harness does, so stale peers of earlier runs do not stall
# downloads; Rustorr's data is fresh on every run.
E2E_COMPOSE="docker compose -f ${ROOT}/docker-compose.baseline.yml -f ${ROOT}/docker-compose.r2-hermetic.yml -f ${ROOT}/docker-compose.r8-e2e.yml"
e2e() {
  doctor
  in_web sh -c 'npm ci --no-audit --no-fund >/dev/null'
  # As tools/r2.sh reset-seeder: both recreated (the fixtures are complete
  # before the seeder starts), then wait until it seeds and has announced.
  ${E2E_COMPOSE} up -d --force-recreate tracker seeder
  ready=0
  for _ in $(seq 1 60); do
    seeder=$(${E2E_COMPOSE} ps -q seeder)
    health=$(docker inspect --format '{{if .State.Health}}{{.State.Health.Status}}{{end}}' "${seeder}" 2>/dev/null || true)
    announced=$(${E2E_COMPOSE} exec -T seeder transmission-remote 127.0.0.1:9091 -t 1 -it 2>/dev/null | grep -c 'Tracker had 1 seeders' || true)
    if [ "${health}" = healthy ] && [ "${announced}" -ge 1 ]; then ready=1; break; fi
    sleep 1
  done
  [ "${ready}" = 1 ] || { echo "error: the seeder did not become ready" >&2; return 1; }
  ${E2E_COMPOSE} build e2e-rustorr e2e
  ${E2E_COMPOSE} up -d --force-recreate --renew-anon-volumes e2e-rustorr
  status=0
  ${E2E_COMPOSE} run --rm e2e npx playwright test -c e2e/playwright.config.ts "$@" || status=$?
  mkdir -p "${ROOT}/web/e2e/results"
  ${E2E_COMPOSE} logs --no-color e2e-rustorr >"${ROOT}/web/e2e/results/rustorr.log" 2>&1 || true
  ${E2E_COMPOSE} rm -sf -v e2e-rustorr e2e-seed >/dev/null
  return "${status}"
}

check() {
  web_check
  boundaries
  in_dev sh -c '
    set -eu
    cargo fmt --all --check
    cargo clippy --workspace --all-targets --locked -- -D warnings
    cargo test --workspace --locked
    # The GStreamer variant links libgstreamer; the dev image carries it.
    cargo clippy --package rustorr-server --all-targets --locked --features gstreamer -- -D warnings
    cargo test --package rustorr-gstreamer --locked --features runtime
  '
}

test_all() { in_dev cargo test --workspace --locked; }

build() { in_dev cargo build --workspace --release --locked; }

# This runs on the arm64 development image but emits the supported amd64 Linux
# binary. It deliberately exercises aws-lc-sys and bundled SQLite before the
# multi-stage Dockerfile makes a release image.
cross_build() {
  in_dev cargo build --workspace --release --locked --target x86_64-unknown-linux-gnu
}

smoke() {
  doctor
  command -v curl >/dev/null 2>&1 || { echo "error: curl is required for smoke" >&2; return 1; }
  docker buildx version >/dev/null 2>&1 || { echo "error: docker buildx is required for smoke" >&2; return 1; }

  compose() { docker compose -f "${ROOT}/docker-compose.r4-smoke.yml" "$@"; }
  arm_image=${RUSTORR_R4_SMOKE_ARM_IMAGE:-rustorr:r4-smoke-arm64}
  amd_image=${RUSTORR_R4_SMOKE_AMD_IMAGE:-rustorr:r4-smoke-amd64}
  port=${RUSTORR_R4_SMOKE_PORT:-8095}
  run_root=$(mktemp -d "${TMPDIR:-/tmp}/rustorr-r4-smoke.XXXXXX")
  export RUSTORR_R4_SMOKE_IMAGE=${arm_image}
  export RUSTORR_R4_SMOKE_PORT=${port}
  cleanup() { compose down >/dev/null 2>&1 || true; rm -rf "${run_root}"; }
  trap cleanup EXIT
  trap 'cleanup; exit 130' INT TERM

  # --load makes each single-platform result runnable by this Docker daemon.
  docker buildx build --load --platform linux/arm64 -t "${arm_image}" -f "${ROOT}/Dockerfile" "${ROOT}"
  docker buildx build --load --platform linux/amd64 -t "${amd_image}" -f "${ROOT}/Dockerfile" "${ROOT}"

  compose up -d --force-recreate rustorr
  ready=0
  for attempt in $(seq 1 30); do
    if curl --fail --silent --show-error "http://127.0.0.1:${port}/echo" >"${run_root}/echo"; then
      ready=1
      break
    fi
    sleep 1
  done
  [ "${ready}" = 1 ] || { compose logs rustorr >&2; echo "error: Rustorr did not answer GET /echo" >&2; return 1; }
  grep -Fx 'MatriX.145' "${run_root}/echo" >/dev/null || { cat "${run_root}/echo" >&2; echo "error: unexpected /echo body" >&2; return 1; }

  container=$(compose ps -q rustorr)
  docker stop --time 10 "${container}" >/dev/null
  [ "$(docker inspect --format '{{.State.ExitCode}}' "${container}")" = 0 ] || { docker logs "${container}" >&2; echo "error: Rustorr did not exit cleanly" >&2; return 1; }
  docker logs "${container}" 2>&1 | grep -F 'shutdown complete' >/dev/null || { docker logs "${container}" >&2; echo "error: shutdown was not logged" >&2; return 1; }

  # The named volume remains attached; the second start proves SQLite reopens it.
  compose up -d --force-recreate rustorr
  ready=0
  for attempt in $(seq 1 30); do
    if curl --fail --silent --show-error "http://127.0.0.1:${port}/echo" >/dev/null; then ready=1; break; fi
    sleep 1
  done
  [ "${ready}" = 1 ] || { compose logs rustorr >&2; echo "error: Rustorr did not restart" >&2; return 1; }
  container=$(compose ps -q rustorr)
  docker logs "${container}" 2>&1 | grep -F 'schema_version=3' >/dev/null || { docker logs "${container}" >&2; echo "error: restart did not open schema version 3" >&2; return 1; }
  docker stop --time 10 "${container}" >/dev/null
  [ "$(docker inspect --format '{{.State.ExitCode}}' "${container}")" = 0 ] || { docker logs "${container}" >&2; echo "error: restarted Rustorr did not exit cleanly" >&2; return 1; }
}

clean() {
  doctor
  docker volume rm "${TARGET_VOLUME}" "${CARGO_VOLUME}" >/dev/null 2>&1 || true
}

command=${1:-}
shift || true
case "${command}" in
  doctor) doctor ;;
  image) image ;;
  check) check ;;
  web-check) web_check ;;
  web) in_web "$@" ;;
  e2e) e2e "$@" ;;
  boundaries) boundaries ;;
  test) test_all ;;
  build) build ;;
  cross-build) cross_build ;;
  smoke) smoke ;;
  cargo) in_dev cargo "$@" ;;
  clean) clean ;;
  *) usage >&2; exit 2 ;;
esac
