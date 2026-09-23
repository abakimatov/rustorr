#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
COMPOSE="docker compose -f ${ROOT}/docker-compose.baseline.yml -f ${ROOT}/docker-compose.r2-hermetic.yml"
R6_COMPOSE="${COMPOSE} -f ${ROOT}/docker-compose.r6-contract.yml"
PROXY_COMPOSE="${COMPOSE} -f ${ROOT}/docker-compose.r2-proxy.yml"
R6_PROXY_COMPOSE="${R6_COMPOSE} -f ${ROOT}/docker-compose.r6-proxy.yml"
AUTH_COMPOSE="${R6_COMPOSE} -f ${ROOT}/docker-compose.r6-auth.yml"
R7_REFERENCE_COMPOSE="${COMPOSE} -f ${ROOT}/docker-compose.r7-capability.yml"
R7_CANDIDATE_COMPOSE="${R6_COMPOSE} -f ${ROOT}/docker-compose.r7-capability.yml -f ${ROOT}/docker-compose.r7-candidate.yml"
RUN_ROOT=${RUSTORR_CONTRACT_RUN_ROOT:-/tmp/rustorr-contract}
MANIFEST=${ROOT}/tools/contract/scenarios.json
ALLOWLIST=${ROOT}/tools/contract/deferred-routes.json

usage() { printf '%s\n' "usage: $0 {doctor|config|up|candidate-up|candidate-down|auth-reference-up|auth-candidate-up|proxy-up|candidate-proxy-up|proxy-down|candidate-proxy-down|tls-up|tls-down|reset-seeder|restart-matrix|capture|diff|down}"; }
doctor() { command -v docker >/dev/null 2>&1 || { echo "error: docker CLI is not installed" >&2; return 1; }; docker compose version >/dev/null 2>&1 || { echo "error: docker compose is unavailable" >&2; return 1; }; docker info >/dev/null 2>&1 || { echo "error: Docker daemon is unavailable" >&2; return 1; }; }
config() { ${COMPOSE} config; }
up() { doctor; ${COMPOSE} up -d --force-recreate tracker seeder torrserver; }
candidate_up() { doctor; ${R6_COMPOSE} up -d --build rustorr; }
candidate_down() { doctor; ${R6_COMPOSE} rm -sf rustorr; }
auth_reference_up() { doctor; ${AUTH_COMPOSE} up -d --force-recreate torrserver; }
auth_candidate_up() { doctor; ${AUTH_COMPOSE} up -d --force-recreate rustorr; }
reset_seeder() {
  doctor
  # The tracker keeps peer announcements in memory. Recreate it together with
  # Transmission so crashed/removed sessions from an earlier target cannot be
  # returned as stale peers to the next corpus.
  ${COMPOSE} up -d --force-recreate tracker seeder
  for attempt in $(seq 1 30); do
    container=$(${COMPOSE} ps -q seeder)
    health=$(docker inspect --format '{{if .State.Health}}{{.State.Health.Status}}{{end}}' "${container}" 2>/dev/null || true)
    tracker_ready=$(${COMPOSE} exec -T seeder transmission-remote 127.0.0.1:9091 -t 1 -it 2>/dev/null | grep -c 'Tracker had 1 seeders' || true)
    if [ "${health}" = healthy ] && [ "${tracker_ready}" -ge 1 ]; then
      return 0
    fi
    sleep 1
  done
  ${COMPOSE} logs seeder >&2
  echo "error: seeder did not become healthy and announce to the local tracker" >&2
  return 1
}
host_fixtures() {
  fixture_root=${RUN_ROOT}/fixtures
  if [ ! -s "${fixture_root}/torrents/single.torrent" ]; then
    python3 "${ROOT}/tools/baseline/generate-fixtures.py" "${fixture_root}"
  fi
  printf '%s\n' "${fixture_root}/torrents/single.torrent"
}
proxy_up() {
  doctor
  cert_dir=${RUSTORR_CONTRACT_CERT_DIR:-${RUN_ROOT}/certs}
  command -v openssl >/dev/null 2>&1 || { echo "error: openssl is required for the TLS proxy fixture" >&2; return 1; }
  mkdir -p "${cert_dir}"
  if [ ! -s "${cert_dir}/server.crt" ] || [ ! -s "${cert_dir}/server.key" ]; then
    openssl req -x509 -newkey rsa:2048 -nodes -days 1 \
      -keyout "${cert_dir}/server.key" -out "${cert_dir}/server.crt" \
      -subj '/CN=localhost' -addext 'subjectAltName=DNS:localhost,IP:127.0.0.1' \
      >/dev/null 2>&1
  fi
  RUSTORR_CONTRACT_CERT_DIR="${cert_dir}" ${PROXY_COMPOSE} up -d r2proxy
}
candidate_proxy_up() {
  proxy_up
  cert_dir=${RUSTORR_CONTRACT_CERT_DIR:-${RUN_ROOT}/certs}
  RUSTORR_CONTRACT_CERT_DIR="${cert_dir}" ${R6_PROXY_COMPOSE} up -d r6proxy
}
tls_up() {
  proxy_up
  cert_dir=${RUSTORR_CONTRACT_CERT_DIR:-${RUN_ROOT}/certs}
  RUSTORR_CONTRACT_CERT_DIR="${cert_dir}" ${PROXY_COMPOSE} up -d r2tls
}
tls_down() { doctor; ${PROXY_COMPOSE} rm -sf r2tls; }
proxy_down() { doctor; ${PROXY_COMPOSE} rm -sf r2proxy; }
candidate_proxy_down() { doctor; ${R6_PROXY_COMPOSE} rm -sf r6proxy; }
down() { doctor; ${COMPOSE} down; }
capture() {
  target=${1:?capture target is required: reference or candidate}
  shift
  profile=direct
  previous=
  for argument in "$@"; do
    [ "${previous}" = --profile ] && profile=${argument}
    case "${argument}" in --profile=*) profile=${argument#--profile=} ;; esac
    previous=${argument}
  done
  # Each profile needs its own target configuration and entry point. The auth
  # overlay must also be applied when the target is recreated, and nginx
  # resolves its upstream once, so it is restarted after the target is.
  case "${profile}" in
    auth) reference_compose=${AUTH_COMPOSE}; candidate_compose=${AUTH_COMPOSE} ;;
    r7) reference_compose=${R7_REFERENCE_COMPOSE}; candidate_compose=${R7_CANDIDATE_COMPOSE} ;;
    *) reference_compose=${COMPOSE}; candidate_compose=${R6_COMPOSE} ;;
  esac
  case "${target}:${profile}" in
    reference:proxy) default_url=https://r2proxy:8443 ;;
    candidate:proxy) default_url=https://r6proxy:8443 ;;
    reference:*) default_url=http://torrserver:8090 ;;
    candidate:*) default_url=http://rustorr:8090 ;;
    *) echo "error: unknown capture target ${target}" >&2; return 2 ;;
  esac
  if [ "${target}" = reference ]; then
    base_url=${RUSTORR_CONTRACT_BASE_URL:-${default_url}}
  else
    base_url=${RUSTORR_CANDIDATE_BASE_URL:-${default_url}}
  fi
  run_id=${RUSTORR_CONTRACT_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)}
  output_dir=${RUN_ROOT}/${run_id}
  mkdir -p "${output_dir}"
  if [ "${RUSTORR_RESET_SEEDER:-1}" = 1 ]; then
    reset_seeder
    if [ "${target}" = reference ]; then
      ${reference_compose} up -d --force-recreate torrserver
    else
      ${candidate_compose} up -d --force-recreate rustorr
    fi
    # Both servers bind before their torrent runtime is fully settled. Give
    # that runtime a bounded quiet interval before the isolated corpus starts.
    sleep 2
  fi
  if [ "${profile}" = r7 ]; then
    # The fake Torznab indexer answers both targets from the fixture network.
    ${reference_compose} up -d --force-recreate indexer
  fi
  if [ "${profile}" = proxy ]; then
    if [ "${target}" = reference ]; then
      proxy_up
      ${PROXY_COMPOSE} restart r2proxy
    else
      candidate_proxy_up
      ${R6_PROXY_COMPOSE} restart r6proxy
    fi
    set -- "$@" --insecure
  fi
  torrent_file=$(host_fixtures)
  docker run --rm --network rustorr-r1_baseline \
    -v "${ROOT}:/workspace:ro" \
    -v "${RUN_ROOT}:${RUN_ROOT}" \
    -v /var/run/docker.sock:/var/run/docker.sock \
    -w /workspace \
    rustorr-r1-fixture \
    python3 tools/contract/run.py --base-url "${base_url}" \
      --manifest tools/contract/scenarios.json --torrent-file "${torrent_file}" \
      --output "${output_dir}/${target}.json" "$@"
}
restart_matrix() {
  target=${1:?restart-matrix target is required: reference or candidate}
  case "${target}" in
    reference) service=torrserver; base_url=http://torrserver:8090 ;;
    candidate) service=rustorr; base_url=http://rustorr:8090 ;;
    *) echo "error: unknown restart-matrix target ${target}" >&2; return 2 ;;
  esac
  run_id=${RUSTORR_CONTRACT_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)}
  reset_seeder
  if [ "${target}" = reference ]; then ${COMPOSE} up -d --force-recreate torrserver; else ${R6_COMPOSE} up -d --force-recreate rustorr; fi
  sleep 2
  docker run --rm --network rustorr-r1_baseline \
    -v "${ROOT}:/workspace:ro" \
    -v "${RUN_ROOT}:${RUN_ROOT}" \
    -v /var/run/docker.sock:/var/run/docker.sock \
    -w /workspace/tools/contract \
    rustorr-r1-fixture \
    python3 restart_matrix.py --base-url "${base_url}" \
      --target-container "rustorr-r1-${service}-1" \
      --output "${RUN_ROOT}/${run_id}/${target}-restart.json"
}
diff_corpus() { reference=${1:?reference corpus is required}; candidate=${2:?candidate corpus is required}; output=${3:-${RUN_ROOT}/diff.json}; python3 "${ROOT}/tools/contract/diff.py" "${reference}" "${candidate}" --allowlist "${ALLOWLIST}" --output "${output}"; }

command=${1:-}; shift || true
case "${command}" in
  doctor) doctor ;; config) config ;; up) up ;; candidate-up) candidate_up ;; candidate-down) candidate_down ;; auth-reference-up) auth_reference_up ;; auth-candidate-up) auth_candidate_up ;; proxy-up) proxy_up ;; candidate-proxy-up) candidate_proxy_up ;; proxy-down) proxy_down ;; candidate-proxy-down) candidate_proxy_down ;; tls-up) tls_up ;; tls-down) tls_down ;; reset-seeder) reset_seeder ;; restart-matrix) restart_matrix "$@" ;; capture) capture "$@" ;; diff) diff_corpus "$@" ;; down) down ;; *) usage >&2; exit 2 ;;
esac
