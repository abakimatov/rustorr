#!/bin/sh
# Deploys Rustorr with Docker on a server reached over SSH (docs/r9-plan.md).
# The server needs Docker with the compose plugin and nothing else: the image
# is built here for its architecture and sent over SSH, no registry involved.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

usage() {
  cat <<'EOF'
usage: tools/deploy.sh [options] <user@host> <command>

commands:
  deploy            install, or upgrade to this checkout (a backup first; rolls
                    back by itself when the new version does not come up)
  rollback          back to the previous version (restores the pre-upgrade
                    backup when the previous version cannot open the state)
  restart           restart, e.g. after editing ~/rustorr/.env on the server
  status            image, health and version
  logs [args]       docker compose logs, e.g. logs -f or logs --tail 50
  backup            a backup into ~/rustorr/backups on the server
  download <dir>    copy the newest backup from the server into <dir>
  restore <file>    restore a backup (a local file, or one in ~/rustorr/backups
                    on the server) and restart
  password <user>   set the HTTP password of <user> (prompts) and restart
  cert <chain> <key>
                    use this PEM certificate chain and key for HTTPS (turns
                    HTTPS on) and restart

options:
  --gst             the image with GStreamer (HLS in the browser); larger
  --ffprobe         add ffmpeg for /ffp (media info for some clients); larger
  --tag <tag>       image tag instead of the commit (rustorr:<tag>)
  --image <image>   send this local image instead of building one
  --dir <path>      directory on the server, relative to the home (rustorr)
  --ssh <options>   extra ssh options, e.g. "-p 2222 -i ~/.ssh/key"
EOF
}

GST=0
FFPROBE=0
TAG=""
IMAGE=""
DIR=rustorr
SSH_OPTIONS=""
while [ $# -gt 0 ]; do
  case $1 in
    --gst) GST=1; shift ;;
    --ffprobe) FFPROBE=1; shift ;;
    --tag) TAG=$2; shift 2 ;;
    --image) IMAGE=$2; shift 2 ;;
    --dir) DIR=$2; shift 2 ;;
    --ssh) SSH_OPTIONS=$2; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    --*) echo "error: unknown option $1" >&2; usage >&2; exit 2 ;;
    *) break ;;
  esac
done
[ $# -ge 2 ] || { usage >&2; exit 2; }
HOST=$1
COMMAND=$2
shift 2

say() { printf '==> %s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

# shellcheck disable=SC2086 # SSH_OPTIONS is a list of words on purpose.
remote() { ssh $SSH_OPTIONS -o BatchMode=yes "$HOST" "$@"; }

# Shell functions for the scripts run on the server, in the deployment
# directory. The compose project is `rustorr`, so its volume is rustorr_data.
HELPERS='
compose() { RUSTORR_IMAGE=$(cat current 2>/dev/null || echo none) docker compose "$@"; }
image_of() { cat "$1" 2>/dev/null || true; }
in_volume() { docker run --rm -i --user 0 -v rustorr_data:/data --entrypoint sh "$(cat current)" -c "$1"; }
healthy() {
  i=0
  while [ $i -lt 60 ]; do
    id=$(compose ps -q rustorr 2>/dev/null || true)
    state=$( [ -n "$id" ] && docker inspect -f "{{.State.Health.Status}}" "$id" 2>/dev/null || echo missing)
    [ "$state" = healthy ] && return 0
    [ "$state" = missing ] && [ $i -gt 5 ] && return 1
    # A process that exits at start-up is restarted by the policy: no use
    # waiting for it.
    [ -n "$id" ] && [ "$(docker inspect -f "{{.RestartCount}}" "$id" 2>/dev/null || echo 0)" -gt 0 ] && return 1
    i=$((i + 1)); sleep 2
  done
  return 1
}
take_backup() {
  umask 077; mkdir -p backups
  if [ -n "$(compose ps -q --status running rustorr 2>/dev/null)" ]; then
    compose exec -T rustorr sh -c "rustorr backup /tmp/b.tar.gz >&2 && cat /tmp/b.tar.gz && rm -f /tmp/b.tar.gz" < /dev/null > "backups/$1"
  else
    in_volume "rustorr --data-dir /data backup /tmp/b.tar.gz >&2 && cat /tmp/b.tar.gz" < /dev/null > "backups/$1"
  fi
  echo "backups/$1"
}
restore_backup() {
  compose stop rustorr >&2 || true
  in_volume "cat > /tmp/r.tar.gz && rustorr --data-dir /data restore /tmp/r.tar.gz --force && chown -R rustorr:rustorr /data" < "$1" >&2
}
'
# The script runs under sh whatever the login shell is; its stdin stays the
# caller's, for the commands that stream data.
quote() { printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")"; }
remote_sh() { remote "sh -c $(quote "set -eu; mkdir -p $DIR; cd $DIR
$HELPERS
$1")"; }

# Copies a local file to the server, readable by its owner only.
upload() { remote "umask 077; cat > '$2'" < "$1"; }

platform() {
  machine=$(remote uname -m)
  case $machine in
    x86_64|amd64) echo linux/amd64 ;;
    aarch64|arm64) echo linux/arm64 ;;
    *) die "unsupported server architecture $machine" ;;
  esac
}

image_tag() {
  if [ -n "$IMAGE" ]; then echo "$IMAGE"; return; fi
  if [ -n "$TAG" ]; then echo "rustorr:$TAG"; return; fi
  tag=$(git -C "$ROOT" rev-parse --short=12 HEAD)
  [ -z "$(git -C "$ROOT" status --porcelain)" ] || tag="$tag-dirty$(date -u +%H%M%S)"
  [ "$GST" = 1 ] && tag="$tag-gst"
  [ "$GST" = 0 ] && [ "$FFPROBE" = 1 ] && tag="$tag-ffprobe"
  echo "rustorr:$tag"
}

build_and_send() {
  image=$1
  if remote docker image inspect "$image" >/dev/null 2>&1; then
    say "$image is already on the server"
    return
  fi
  if [ -n "$IMAGE" ]; then
    docker image inspect "$image" >/dev/null 2>&1 || die "no local image $image"
  else
    target=$(platform)
    say "building $image for $target"
    if [ "$GST" = 1 ]; then
      set -- --build-arg RUSTORR_FEATURES=gstreamer \
        --build-arg "RUSTORR_BUILD_PACKAGES=libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev" \
        --build-arg "RUSTORR_RUNTIME_PACKAGES=ffmpeg libgstreamer1.0-0 gstreamer1.0-tools gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad gstreamer1.0-plugins-ugly gstreamer1.0-libav gstreamer1.0-plugins-base-apps"
    elif [ "$FFPROBE" = 1 ]; then
      set -- --build-arg RUSTORR_RUNTIME_PACKAGES=ffmpeg
    else
      # The plain image: a fifth of the size, as every upgrade sends it whole.
      set --
    fi
    docker buildx build --load --platform "$target" -t "$image" "$@" -f "$ROOT/Dockerfile" "$ROOT"
  fi
  say "sending $image to $HOST"
  docker save "$image" | gzip -1 | remote 'gunzip | docker load >/dev/null'
}

# With authentication on and no account yet: one account with a random
# password, printed once.
first_account() {
  remote_sh '
    grep -q "^RUSTORR_HTTP_AUTH=true" .env || exit 0
    in_volume "test -s /data/accs.db" < /dev/null && exit 0
    password=$(head -c 24 /dev/urandom | base64 | tr -d "/+=" | head -c 20)
    echo "$password" | in_volume "rustorr --data-dir /data passwd admin && chown rustorr:rustorr /data/accs.db" 2>/dev/null
    printf "\nHTTP account created: admin / %s\nIt is not shown again; tools/deploy.sh <host> password admin changes it.\n\n" "$password" >&2
  '
}

status() {
  remote_sh '
    id=$(compose ps -q rustorr 2>/dev/null || true)
    echo "image:    $(image_of current)"
    echo "previous: $(image_of previous)"
    if [ -n "$id" ]; then
      echo "state:    $(docker inspect -f "{{.State.Status}}, {{.State.Health.Status}}" "$id")"
      echo "version:  $(compose exec -T rustorr rustorr --version < /dev/null)"
      port=$(sed -n "s/^RUSTORR_HTTP_PORT=//p" .env); echo "http:     port ${port:-8090}"
    else
      echo "state:    not running"
    fi
  '
}

deploy() {
  image=$(image_tag)
  remote docker compose version >/dev/null 2>&1 || die "the server needs Docker with the compose plugin"
  build_and_send "$image"
  remote "mkdir -p $DIR/backups && chmod 700 $DIR $DIR/backups"
  upload "$ROOT/deploy/compose.yaml" "$DIR/compose.yaml"
  if ! remote "test -f $DIR/.env"; then
    upload "$ROOT/deploy/env.example" "$DIR/.env"
    say "created ~/$DIR/.env from deploy/env.example; edit it there and run restart"
  fi
  # Records the new image and, on an upgrade, the old one with a backup.
  outcome=$(remote_sh "
    old=\$(image_of current)
    if [ \"\$old\" = '$image' ]; then echo same; exit 0; fi
    if [ -n \"\$old\" ] && [ -n \"\$(compose ps -q rustorr 2>/dev/null)\" ]; then
      backup=\$(take_backup \"pre-upgrade-\$(date -u +%Y%m%dT%H%M%SZ).tar.gz\")
      echo \"\$backup\" > upgrade-backup
      echo \"\$old\" > previous
      echo \"upgrade from \$old, backup \$backup\"
    else
      rm -f previous upgrade-backup
      echo install
    fi
    echo '$image' > current
  ")
  say "$outcome"
  # Compose creates the volume (with its labels) before anything writes to it.
  remote_sh 'compose create --remove-orphans >&2'
  first_account
  remote_sh 'compose up -d --remove-orphans >&2'
  if remote_sh healthy; then
    remote_sh '
      keep=" $(image_of current) $(image_of previous) "
      for old in $(docker image ls rustorr --format "{{.Repository}}:{{.Tag}}"); do
        case $keep in *" $old "*) ;; *) docker image rm "$old" >/dev/null 2>&1 || true ;; esac
      done
    '
    say "$image is up"
    status
    return
  fi
  say "$image did not become healthy; its log:"
  remote_sh 'compose logs --tail 40 rustorr >&2' || true
  case $outcome in
    upgrade*) rollback; die "the upgrade to $image failed and was rolled back" ;;
    *) die "$image did not come up" ;;
  esac
}

rollback() {
  remote "test -s $DIR/previous" || die "no previous version recorded"
  say "rolling back to $(remote cat "$DIR/previous")"
  remote_sh '
    failed=$(image_of current)
    image_of previous > current
    echo "$failed" > previous
    compose up -d >&2
  '
  if remote_sh healthy; then
    say "the previous version runs on the current state"
  elif remote "test -s $DIR/upgrade-backup"; then
    say "the previous version cannot open the state; restoring the backup taken before the upgrade"
    remote_sh 'restore_backup "$(cat upgrade-backup)"; compose up -d >&2'
    remote_sh healthy || die "the previous version did not come up after the restore"
    say "restored $(remote cat "$DIR/upgrade-backup"); changes made after the upgrade are lost"
  else
    die "the previous version did not come up"
  fi
  status
}

case $COMMAND in
  deploy) deploy ;;
  rollback) rollback ;;
  restart)
    remote_sh 'compose up -d --force-recreate >&2'
    remote_sh healthy || die "Rustorr did not become healthy; see: tools/deploy.sh $HOST logs"
    status
    ;;
  status) status ;;
  logs) remote_sh "compose logs $*" ;;
  backup) remote_sh 'take_backup "manual-$(date -u +%Y%m%dT%H%M%SZ).tar.gz"' ;;
  download)
    [ $# -eq 1 ] || die "download needs a local directory"
    newest=$(remote "ls -1t $DIR/backups/*.tar.gz 2>/dev/null | head -1" || true)
    [ -n "$newest" ] || die "no backup on the server"
    mkdir -p "$1"
    (umask 077; remote "cat '$newest'" > "$1/$(basename "$newest")")
    say "downloaded $1/$(basename "$newest")"
    ;;
  restore)
    [ $# -eq 1 ] || die "restore needs a backup file"
    file=$1
    if [ -f "$file" ]; then
      upload "$file" "$DIR/backups/$(basename "$file")"
      file=backups/$(basename "$file")
    fi
    remote "test -f '$DIR/$file'" || die "no $file on the server"
    remote_sh "take_backup \"before-restore-\$(date -u +%Y%m%dT%H%M%SZ).tar.gz\" >&2; restore_backup '$file'; compose up -d >&2"
    remote_sh healthy || die "Rustorr did not come up after the restore"
    status
    ;;
  password)
    [ $# -eq 1 ] || die "password needs a user name"
    printf 'new password for %s: ' "$1" >&2
    stty -echo 2>/dev/null || true
    read -r password
    stty echo 2>/dev/null || true
    echo >&2
    [ -n "$password" ] || die "empty password"
    echo "$password" | remote_sh "in_volume 'rustorr --data-dir /data passwd \"$1\" && chown rustorr:rustorr /data/accs.db'; compose up -d --force-recreate >&2"
    remote_sh healthy && say "password set for $1"
    ;;
  cert)
    [ $# -eq 2 ] || die "cert needs a certificate chain and a key file"
    [ -f "$1" ] && [ -f "$2" ] || die "no such file"
    # Beside the self-signed server.pem, which a rejected pair would
    # otherwise be replaced with; backups include both.
    remote_sh "in_volume 'cat > /data/tls-cert.pem && chown rustorr:rustorr /data/tls-cert.pem && chmod 644 /data/tls-cert.pem'" < "$1"
    remote_sh "in_volume 'cat > /data/tls-key.pem && chown rustorr:rustorr /data/tls-key.pem && chmod 600 /data/tls-key.pem'" < "$2"
    remote_sh '
      set_env() { grep -q "^$1=" .env && sed -i "s|^$1=.*|$1=$2|" .env || echo "$1=$2" >> .env; }
      set_env RUSTORR_SSL true
      set_env RUSTORR_SSL_CERT /data/tls-cert.pem
      set_env RUSTORR_SSL_KEY /data/tls-key.pem
      compose up -d --force-recreate >&2
    '
    remote_sh healthy || die "Rustorr did not come up; see: tools/deploy.sh $HOST logs"
    if remote_sh 'compose logs --since 2m rustorr 2>&1 | grep -F "HTTPS certificate cannot be used"' >&2; then
      die "Rustorr rejected the certificate and serves a self-signed one (see the line above)"
    fi
    say "HTTPS uses the new certificate"
    ;;
  *) usage >&2; exit 2 ;;
esac
