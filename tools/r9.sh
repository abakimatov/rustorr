#!/bin/sh
# R9 checks. `smoke` deploys Rustorr with tools/deploy.sh to a stand-in
# server (sshd and its own Docker, tools/r9/Dockerfile.remote) and walks the
# operations: first install, upgrade, restart, rollback, rollback over a newer
# schema, a failed upgrade rolled back by itself, backup and restore, a
# password change.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
NAME=${RUSTORR_R9_REMOTE:-rustorr-r9-remote}
SSH_PORT=${RUSTORR_R9_SSH_PORT:-2222}
HTTP_PORT=${RUSTORR_R9_HTTP_PORT:-18190}
HTTPS_PORT=${RUSTORR_R9_HTTPS_PORT:-18191}
HOST=root@127.0.0.1

usage() { echo "usage: $0 {smoke}" >&2; }

step() { printf '\n### %s\n' "$*" >&2; }
fail() { printf 'FAILED: %s\n' "$*" >&2; exit 1; }

smoke() {
  run=$(mktemp -d "${TMPDIR:-/tmp}/rustorr-r9-smoke.XXXXXX")
  cleanup() {
    status=$?
    if [ "$status" != 0 ]; then
      echo "--- server log ---" >&2
      docker exec -w /root "$NAME" sh -c 'cd rustorr 2>/dev/null && RUSTORR_IMAGE=$(cat current) docker compose logs --tail 60' >&2 || true
    fi
    docker rm -f -v "$NAME" >/dev/null 2>&1 || true
    rm -rf "$run"
    exit "$status"
  }
  trap cleanup EXIT
  trap 'exit 130' INT TERM

  step "starting the stand-in server"
  docker build -q -t rustorr-r9-remote -f "$ROOT/tools/r9/Dockerfile.remote" "$ROOT/tools/r9" >/dev/null
  docker rm -f -v "$NAME" >/dev/null 2>&1 || true
  ssh-keygen -q -t ed25519 -N '' -f "$run/key"
  docker run -d --name "$NAME" --privileged \
    -p "127.0.0.1:$SSH_PORT:22" -p "127.0.0.1:$HTTP_PORT:8090" -p "127.0.0.1:$HTTPS_PORT:8091" \
    rustorr-r9-remote >/dev/null
  docker cp "$run/key.pub" "$NAME:/root/.ssh/authorized_keys" >/dev/null
  docker exec "$NAME" sh -c 'chown root:root /root/.ssh/authorized_keys && chmod 600 /root/.ssh/authorized_keys'
  for _ in $(seq 60); do docker exec "$NAME" docker info >/dev/null 2>&1 && break; sleep 1; done
  ssh_options="-p $SSH_PORT -i $run/key -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR"
  deploy() { "$ROOT/tools/deploy.sh" --ssh "$ssh_options" "$@"; }
  on_server() { docker exec -w /root "$NAME" sh -c "cd rustorr && RUSTORR_IMAGE=\$(cat current) $1"; }

  password=""
  api() { # api <method> <path> [curl args]: the HTTP status, body in $run/body
    method=$1; path=$2; shift 2
    curl -s -o "$run/body" -w '%{http_code}' -u "admin:$password" -X "$method" "http://127.0.0.1:$HTTP_PORT$path" "$@"
  }
  titles() { api POST /torrents -d '{"action":"list"}' >/dev/null; python3 -c 'import json,sys;print(",".join(sorted(t["title"] for t in json.load(open(sys.argv[1])) or [])))' "$run/body"; }
  expect_titles() { got=$(titles); [ "$got" = "$1" ] || fail "torrents: expected '$1', got '$got'"; }
  current() { docker exec -w /root "$NAME" cat rustorr/current; }

  # A tiny torrent to keep in the catalog; no peer is needed to add it.
  python3 - "$run/one.torrent" <<'PY'
import hashlib, sys
def b(v):
    if isinstance(v, int): return b"i%de" % v
    if isinstance(v, bytes): return b"%d:%s" % (len(v), v)
    if isinstance(v, dict): return b"d" + b"".join(b(k) + b(v[k]) for k in sorted(v)) + b"e"
data = b"rustorr r9 smoke\n"
info = {b"name": b"one.txt", b"length": len(data), b"piece length": 16384, b"pieces": hashlib.sha1(data).digest()}
open(sys.argv[1], "wb").write(b({b"announce": b"http://tracker.invalid/announce", b"info": info}))
PY

  step "first install"
  deploy --tag r9-a "$HOST" deploy 2>&1 | tee "$run/install.log" >&2
  password=$(sed -n 's/^HTTP account created: admin \/ //p' "$run/install.log")
  [ -n "$password" ] || fail "no account printed on the first install"
  [ "$(curl -s -o /dev/null -w '%{http_code}' -X POST "http://127.0.0.1:$HTTP_PORT/torrents" -d '{"action":"list"}')" = 401 ] \
    || fail "the API answers without a password"
  [ "$(api GET /echo)" = 200 ] || fail "/echo"
  [ "$(api POST /torrent/upload -F "file=@$run/one.torrent" -F save=true)" = 200 ] || fail "upload: $(cat "$run/body")"
  expect_titles one.txt
  if on_server "docker compose logs rustorr" 2>&1 | grep -F "$password" >/dev/null; then fail "the password is in the log"; fi

  step "upgrade"
  deploy --tag r9-b "$HOST" deploy >&2
  [ "$(current)" = rustorr:r9-b ] || fail "current is $(current)"
  docker exec -w /root "$NAME" sh -c 'ls rustorr/backups/pre-upgrade-*.tar.gz' >/dev/null || fail "no pre-upgrade backup"
  expect_titles one.txt

  step "restart"
  deploy "$HOST" restart >&2
  expect_titles one.txt

  step "rollback"
  deploy "$HOST" rollback >&2
  [ "$(current)" = rustorr:r9-a ] || fail "current is $(current) after the rollback"
  expect_titles one.txt

  step "rollback when the new version changed the schema"
  deploy --tag r9-b "$HOST" deploy >&2
  [ "$(api POST /torrents -d '{"action":"rem","hash":"'"$(python3 -c 'import hashlib,sys;d=open(sys.argv[1],"rb").read();i=d.index(b"4:info")+6;print(hashlib.sha1(d[i:-1]).hexdigest())' "$run/one.torrent")"'"}')" = 200 ] \
    || fail "rem"
  expect_titles ""
  # What a newer Rustorr leaves behind: a schema this one does not know.
  on_server 'docker compose stop rustorr >/dev/null 2>&1'
  on_server "docker run --rm --user 0 -v rustorr_data:/data --entrypoint sh \$(cat current) -c 'printf \"\\000\\000\\000\\143\" | dd of=/data/rustorr.db bs=1 seek=60 conv=notrunc 2>/dev/null'"
  deploy "$HOST" rollback 2>&1 | tee "$run/rollback.log" >&2
  grep -F "restoring the backup taken before the upgrade" "$run/rollback.log" >/dev/null || fail "the backup was not restored"
  [ "$(current)" = rustorr:r9-a ] || fail "current is $(current)"
  expect_titles one.txt

  step "a failed upgrade rolls back by itself"
  printf 'FROM rustorr:r9-a\nENTRYPOINT ["sh", "-c", "echo broken >&2; exit 1"]\n' \
    | docker build -q -t rustorr:r9-broken - >/dev/null
  if deploy --image rustorr:r9-broken "$HOST" deploy 2>"$run/broken.log"; then fail "a broken upgrade reported success"; fi
  grep -F "was rolled back" "$run/broken.log" >/dev/null || { cat "$run/broken.log" >&2; fail "no rollback after a failed upgrade"; }
  [ "$(current)" = rustorr:r9-a ] || fail "current is $(current) after the failed upgrade"
  expect_titles one.txt

  step "backup, download and restore"
  deploy "$HOST" backup >&2
  deploy "$HOST" download "$run/backups" >&2
  backup=$(ls "$run"/backups/*.tar.gz)
  [ "$(api POST /torrents -d '{"action":"wipe"}')" = 200 ] || fail "wipe"
  expect_titles ""
  deploy "$HOST" restore "$backup" >&2
  expect_titles one.txt

  step "password change"
  printf 'n3w-Password\n' | deploy "$HOST" password admin >&2
  password=n3w-Password
  [ "$(api GET /echo)" = 200 ] && [ "$(api POST /torrents -d '{"action":"list"}')" = 200 ] || fail "the new password is refused"

  step "own HTTPS certificate"
  # LibreSSL makes an X.509 v1 certificate without -addext: rejected, loudly.
  openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 2 \
    -subj /CN=r9-old -keyout "$run/old-key.pem" -out "$run/old-cert.pem" 2>/dev/null
  if openssl x509 -in "$run/old-cert.pem" -noout -text | grep -q "Version: 1"; then
    if deploy "$HOST" cert "$run/old-cert.pem" "$run/old-key.pem" 2>"$run/old.log"; then fail "a v1 certificate was accepted"; fi
    grep -F "rejected the certificate" "$run/old.log" >/dev/null || { cat "$run/old.log" >&2; fail "no error for a rejected certificate"; }
  fi
  # X.509 v3 (-addext) with a named curve, as CAs issue them; LibreSSL
  # otherwise makes v1 with explicit curve parameters, which TLS refuses.
  openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -pkeyopt ec_param_enc:named_curve -nodes -days 2 \
    -subj /CN=r9-smoke -addext subjectAltName=DNS:localhost -keyout "$run/key.pem" -out "$run/cert.pem" 2>/dev/null
  deploy "$HOST" cert "$run/cert.pem" "$run/key.pem" >&2
  subject=$(openssl s_client -connect "127.0.0.1:$HTTPS_PORT" </dev/null 2>/dev/null | openssl x509 -noout -subject 2>/dev/null)
  case $subject in *r9-smoke*) ;; *) fail "HTTPS serves '$subject', not the new certificate" ;; esac
  curl -sk -u "admin:$password" "https://127.0.0.1:$HTTPS_PORT/echo" -o "$run/body" || true
  [ "$(cat "$run/body")" = MatriX.145 ] || fail "/echo over HTTPS"
  deploy "$HOST" backup >&2
  docker exec -w /root "$NAME" sh -c 'tar tzf "$(ls -1t rustorr/backups/manual-*.tar.gz | head -1)"' | grep -x tls-key.pem >/dev/null \
    || fail "the certificate is not in the backup"

  step "status"
  deploy "$HOST" status >&2
  printf '\nR9 smoke: OK\n' >&2
}

case ${1:-} in
  smoke) smoke ;;
  *) usage; exit 2 ;;
esac
