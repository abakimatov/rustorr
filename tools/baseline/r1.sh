#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
COMPOSE="docker compose -f ${ROOT}/docker-compose.baseline.yml"
RUN_ROOT=${RUSTORR_BASELINE_RUN_ROOT:-/tmp/rustorr-baseline}

usage() {
  printf '%s\n' "usage: $0 {doctor|config|build|fixtures|up|run|logs|down}"
}

doctor() {
  command -v docker >/dev/null 2>&1 || { echo "error: docker CLI is not installed" >&2; return 1; }
  docker compose version >/dev/null 2>&1 || { echo "error: docker compose is not available" >&2; return 1; }
  if ! docker info >/dev/null 2>&1; then
    echo "error: Docker daemon is unavailable; start Docker Desktop or a Linux Docker daemon" >&2
    return 1
  fi
  echo "Docker daemon is available"
}

config() { ${COMPOSE} config; }
build() { doctor; ${COMPOSE} build; }
fixtures() { doctor; ${COMPOSE} up --abort-on-container-exit --exit-code-from fixture fixture; }
up() { doctor; fixtures; ${COMPOSE} up -d --force-recreate tracker seeder torrserver; }
logs() { doctor; ${COMPOSE} logs "$@"; }
down() { doctor; ${COMPOSE} down; }

run_measurement() {
  doctor
  up
  run_id=$(date -u +%Y%m%dT%H%M%SZ)
  output_dir=${RUN_ROOT}/${run_id}
  mkdir -p "${output_dir}"
  container_id=$(${COMPOSE} ps -q torrserver)
  seeder_container_id=$(${COMPOSE} ps -q seeder)
  docker stats --no-stream --format '{{json .}}' "${container_id}" >"${output_dir}/docker-stats.json" || true
  cp "${ROOT}/tools/baseline/scenarios.json" "${output_dir}/scenarios.json"
  python3 "${ROOT}/tools/baseline/measure.py" \
    --output "${output_dir}/measurement.json" \
    --known-link "file:///fixtures/torrents/single.torrent" \
    --known-hash "68c3ccdd52b2925f4f97e2f61ea248e728e54cea" \
    --torrserver-container "${container_id}" \
    --seeder-container "${seeder_container_id}"
  docker stats --no-stream --format '{{json .}}' "${container_id}" >"${output_dir}/docker-stats-after.json" || true
  echo "results: ${output_dir}"
}

command=${1:-}
shift || true
case "${command}" in
  doctor) doctor ;;
  config) config ;;
  build) build ;;
  fixtures) fixtures ;;
  up) up ;;
  run) run_measurement "$@" ;;
  logs) logs "$@" ;;
  down) down ;;
  *) usage >&2; exit 2 ;;
esac
