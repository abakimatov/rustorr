#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
COMPOSE="docker compose -f ${ROOT}/docker-compose.baseline.yml -f ${ROOT}/docker-compose.r3-engine.yml"
RUN_ROOT=${RUSTORR_ENGINE_SPIKE_RUN_ROOT:-/tmp/rustorr-engine-spike}

usage() {
  printf '%s\n' "usage: $0 {doctor|config|build|up|probe|pex|down} [probe args]"
  printf '%s\n' "       probe accepts --magnet URL to replace the default known torrent"
}

doctor() {
  command -v docker >/dev/null 2>&1 || { echo "error: docker CLI is not installed" >&2; return 1; }
  docker compose version >/dev/null 2>&1 || { echo "error: docker compose is not available" >&2; return 1; }
  docker info >/dev/null 2>&1 || { echo "error: Docker daemon is unavailable" >&2; return 1; }
  mkdir -p "${RUN_ROOT}"
  echo "Docker daemon is available"
}

config() { ${COMPOSE} config; }

build() {
  doctor
  ${COMPOSE} build engine-spike pex-middle pex-client
}

up() {
  doctor
  ${COMPOSE} up --abort-on-container-exit --exit-code-from fixture fixture
  ${COMPOSE} up -d --force-recreate --wait tracker seeder
  sleep 5
}

probe() {
  doctor
  up
  source_flag=--torrent
  source_value=/fixtures/torrents/single.torrent
  if [ "${1:-}" = "--magnet" ]; then
    source_flag=--magnet
    shift
    source_value=${1:?missing magnet URL after --magnet}
    shift
  fi
  run_id=$(date -u +%Y%m%dT%H%M%SZ)
  output_dir=${RUN_ROOT}/${run_id}
  mkdir -p "${output_dir}"
  cp "${ROOT}/tools/baseline/scenarios.json" "${output_dir}/r1-scenarios.json"
  ${COMPOSE} run --rm engine-spike \
    "${source_flag}" "${source_value}" \
    --output /results/${run_id}/state \
    --tracker http://tracker:6969/announce \
    "$@" >"${output_dir}/probe.json"
  echo "results: ${output_dir}"
}

# Two-peer peer-exchange probe: pex-middle knows the tracker, pex-client knows
# only pex-middle, so any extra peer the client sees came over PEX.
pex() {
  doctor
  up
  RUSTORR_PEX_RUN_ID=pex-$(date -u +%Y%m%dT%H%M%SZ)
  export RUSTORR_PEX_RUN_ID
  output_dir=${RUN_ROOT}/${RUSTORR_PEX_RUN_ID}
  mkdir -p "${output_dir}"
  cp "${ROOT}/tools/baseline/scenarios.json" "${output_dir}/r1-scenarios.json"

  ${COMPOSE} up -d --force-recreate pex-middle
  waited=0
  while [ ! -f "${output_dir}/middle.json" ]; do
    if [ "${waited}" -ge 180 ]; then
      ${COMPOSE} logs --no-log-prefix pex-middle >"${output_dir}/middle.log" 2>&1 || true
      ${COMPOSE} rm -sf pex-middle >/dev/null 2>&1 || true
      echo "error: pex-middle did not report a result within 180s" >&2
      return 1
    fi
    sleep 2
    waited=$((waited + 2))
  done
  echo "pex-middle is seeding; starting the tracker-hidden client"

  set +e
  ${COMPOSE} run --rm --no-deps pex-client \
    --result-file "/results/${RUSTORR_PEX_RUN_ID}/client.json" \
    --torrent /fixtures/torrents/single.torrent \
    --output "/results/${RUSTORR_PEX_RUN_ID}/client" \
    --disable-dht \
    --disable-trackers \
    --initial-peers pex-middle:46900 \
    --initialize-timeout-ms 120000 \
    --read-timeout-ms 120000 \
    "$@" >"${output_dir}/client.stdout.log" 2>&1
  client_status=$?
  set -e

  ${COMPOSE} logs --no-log-prefix pex-middle >"${output_dir}/middle.log" 2>&1 || true
  ${COMPOSE} rm -sf pex-middle >/dev/null 2>&1 || true
  echo "results: ${output_dir}"
  return ${client_status}
}

down() {
  doctor
  ${COMPOSE} down
}

command=${1:-}
shift || true
case "${command}" in
  doctor) doctor ;;
  config) config ;;
  build) build ;;
  up) up ;;
  probe) probe "$@" ;;
  pex) pex "$@" ;;
  down) down ;;
  *) usage >&2; exit 2 ;;
esac
