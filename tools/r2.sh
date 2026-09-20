#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
COMPOSE="docker compose -f ${ROOT}/docker-compose.baseline.yml"
PROXY_COMPOSE="${COMPOSE} -f ${ROOT}/docker-compose.r2-proxy.yml"
RUN_ROOT=${RUSTORR_CONTRACT_RUN_ROOT:-/tmp/rustorr-contract}
MANIFEST=${ROOT}/tools/contract/scenarios.json

usage() { printf '%s\n' "usage: $0 {doctor|config|up|proxy-up|proxy-down|tls-up|tls-down|reset-seeder|capture|diff|down}"; }
doctor() { command -v docker >/dev/null 2>&1 || { echo "error: docker CLI is not installed" >&2; return 1; }; docker compose version >/dev/null 2>&1 || { echo "error: docker compose is unavailable" >&2; return 1; }; docker info >/dev/null 2>&1 || { echo "error: Docker daemon is unavailable" >&2; return 1; }; }
config() { ${COMPOSE} config; }
up() { doctor; ${COMPOSE} up -d --force-recreate tracker seeder torrserver; }
reset_seeder() { doctor; ${COMPOSE} up -d --force-recreate seeder; }
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
tls_up() {
  proxy_up
  cert_dir=${RUSTORR_CONTRACT_CERT_DIR:-${RUN_ROOT}/certs}
  RUSTORR_CONTRACT_CERT_DIR="${cert_dir}" ${PROXY_COMPOSE} up -d r2tls
}
tls_down() { doctor; ${PROXY_COMPOSE} rm -sf r2tls; }
proxy_down() { doctor; ${PROXY_COMPOSE} rm -sf r2proxy; }
down() { doctor; ${COMPOSE} down; }
capture() {
  target=${1:?capture target is required: reference or candidate}
  shift
  base_url=${RUSTORR_CONTRACT_BASE_URL:-http://127.0.0.1:8090}
  run_id=${RUSTORR_CONTRACT_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)}
  output_dir=${RUN_ROOT}/${run_id}
  mkdir -p "${output_dir}"
  [ "${target}" = candidate ] && base_url=${RUSTORR_CANDIDATE_BASE_URL:-http://127.0.0.1:8091}
  if [ "${target}" = reference ] && [ "${RUSTORR_RESET_SEEDER:-0}" = 1 ]; then reset_seeder; fi
  python3 "${ROOT}/tools/contract/run.py" --base-url "${base_url}" --manifest "${MANIFEST}" --output "${output_dir}/${target}.json" "$@"
}
diff_corpus() { reference=${1:?reference corpus is required}; candidate=${2:?candidate corpus is required}; output=${3:-${RUN_ROOT}/diff.json}; python3 "${ROOT}/tools/contract/diff.py" "${reference}" "${candidate}" --output "${output}"; }

command=${1:-}; shift || true
case "${command}" in
  doctor) doctor ;; config) config ;; up) up ;; proxy-up) proxy_up ;; proxy-down) proxy_down ;; tls-up) tls_up ;; tls-down) tls_down ;; reset-seeder) reset_seeder ;; capture) capture "$@" ;; diff) diff_corpus "$@" ;; down) down ;; *) usage >&2; exit 2 ;;
esac
