#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
TARGET=${RUSTORR_R1_TARGET:-torrserver}
if [ "${TARGET}" = rustorr ]; then
  COMPOSE="docker compose -f ${ROOT}/docker-compose.baseline.yml -f ${ROOT}/docker-compose.r5-benchmark.yml"
else
  COMPOSE="docker compose -f ${ROOT}/docker-compose.baseline.yml"
fi
RUN_ROOT=${RUSTORR_BASELINE_RUN_ROOT:-/tmp/rustorr-baseline}

usage() {
  printf '%s\n' "usage: $0 {doctor|config|build|fixtures|up|run|netem-diagnose|restart-probe|logs|down}"
}

doctor() {
  command -v docker >/dev/null 2>&1 || { echo "error: docker CLI is not installed" >&2; return 1; }
  docker compose version >/dev/null 2>&1 || { echo "error: docker compose is not available" >&2; return 1; }
  command -v curl >/dev/null 2>&1 || { echo "error: curl is required" >&2; return 1; }
  if ! docker info >/dev/null 2>&1; then
    echo "error: Docker daemon is unavailable; start Docker Desktop or a Linux Docker daemon" >&2
    return 1
  fi
  echo "Docker daemon is available"
}

config() { ${COMPOSE} config; }
build() { doctor; ${COMPOSE} build; }
fixtures() { doctor; ${COMPOSE} up --abort-on-container-exit --exit-code-from fixture fixture; }
up() { doctor; fixtures; ${COMPOSE} up -d --force-recreate tracker seeder "${TARGET}"; }
logs() { doctor; ${COMPOSE} logs "$@"; }
down() { doctor; ${COMPOSE} down; }

run_measurement() {
  doctor
  run_id=$(date -u +%Y%m%dT%H%M%SZ)
  output_dir=${RUN_ROOT}/${run_id}
  mkdir -p "${output_dir}"
  container_id=""
  finalize_measurement() {
    if [ -n "${container_id}" ]; then
      docker stats --no-stream --format '{{json .}}' "${container_id}" >"${output_dir}/docker-stats-after.json" 2>"${output_dir}/docker-stats-after.error" || true
    fi
    echo "results: ${output_dir}"
  }
  trap finalize_measurement EXIT
  up
  container_id=$(${COMPOSE} ps -q "${TARGET}")
  seeder_container_id=$(${COMPOSE} ps -q seeder)
  docker stats --no-stream --format '{{json .}}' "${container_id}" >"${output_dir}/docker-stats.json" || true
  cp "${ROOT}/tools/baseline/scenarios.json" "${output_dir}/scenarios.json"
  control_args=""
  if [ "${TARGET}" = rustorr ]; then
    control_args="--control-socket ${RUSTORR_R5_CONTROL_SOCKET:-/tmp/rustorr-r5-control/control.sock}"
  fi
  # The socket argument has no spaces in the documented default. Keep the
  # command intentionally simple rather than evaluating arbitrary shell text.
  status=0
  python3 "${ROOT}/tools/baseline/measure.py" \
      --output "${output_dir}/measurement.json" \
      --known-link "file:///fixtures/torrents/single.torrent" \
      --known-hash "68c3ccdd52b2925f4f97e2f61ea248e728e54cea" \
      --torrserver-container "${container_id}" \
      --seeder-container "${seeder_container_id}" \
      ${control_args} || status=$?
  finalize_measurement
  trap - EXIT
  return "${status}"
}

run_restart_probe() {
  doctor
  if [ "${TARGET}" != rustorr ]; then
    echo "error: restart-probe requires RUSTORR_R1_TARGET=rustorr" >&2
    return 2
  fi
  run_id=$(date -u +%Y%m%dT%H%M%SZ)-restart
  output_dir=${RUN_ROOT}/${run_id}
  mkdir -p "${output_dir}"
  container_id=""
  finalize_restart_probe() {
    if [ -n "${container_id}" ]; then
      docker stats --no-stream --format '{{json .}}' "${container_id}" >"${output_dir}/docker-stats-after.json" 2>"${output_dir}/docker-stats-after.error" || true
    fi
    echo "results: ${output_dir}"
  }
  trap finalize_restart_probe EXIT
  up
  container_id=$(${COMPOSE} ps -q rustorr)
  seeder_container_id=$(${COMPOSE} ps -q seeder)
  status=0
  python3 "${ROOT}/tools/baseline/restart_probe.py" \
      --output "${output_dir}/restart-probe.json" \
      --known-link "file:///fixtures/torrents/single.torrent" \
      --known-hash "68c3ccdd52b2925f4f97e2f61ea248e728e54cea" \
      --rustorr-container "${container_id}" \
      --seeder-container "${seeder_container_id}" || status=$?
  finalize_restart_probe
  trap - EXIT
  return "${status}"
}

run_netem_diagnosis() {
  doctor
  run_id=$(date -u +%Y%m%dT%H%M%SZ)-netem-diagnose-${TARGET}
  output_dir=${RUN_ROOT}/${run_id}
  mkdir -p "${output_dir}"
  container_id=""
  finalize_netem_diagnosis() {
    if [ -n "${container_id}" ]; then
      docker stats --no-stream --format '{{json .}}' "${container_id}" >"${output_dir}/docker-stats-after.json" 2>"${output_dir}/docker-stats-after.error" || true
    fi
    echo "results: ${output_dir}"
  }
  trap finalize_netem_diagnosis EXIT
  up
  container_id=$(${COMPOSE} ps -q "${TARGET}")
  status=0
  python3 "${ROOT}/tools/baseline/netem_diagnose.py" \
      --output "${output_dir}/netem-diagnosis.json" \
      --target "${TARGET}" \
      --target-container "${container_id}" \
      --known-link "file:///fixtures/torrents/single.torrent" \
      --known-hash "68c3ccdd52b2925f4f97e2f61ea248e728e54cea" || status=$?
  finalize_netem_diagnosis
  trap - EXIT
  return "${status}"
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
  netem-diagnose) run_netem_diagnosis "$@" ;;
  restart-probe) run_restart_probe "$@" ;;
  logs) logs "$@" ;;
  down) down ;;
  *) usage >&2; exit 2 ;;
esac
