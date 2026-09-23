#!/bin/sh
set -eu

test -d /data/torrents
test -d /data/files

config_dir=/tmp/transmission
mkdir -p "${config_dir}"

transmission-daemon \
  --foreground \
  --config-dir="${config_dir}" \
  --port=9091 \
  --rpc-bind-address=0.0.0.0 \
  --allowed='127.0.0.1,172.*.*.*' \
  --no-auth \
  --no-dht \
  --no-lpd \
  --no-portmap \
  --encryption-tolerated \
  --peerport=6881 &
daemon_pid=$!
trap 'kill "${daemon_pid}" 2>/dev/null || true' EXIT

sleep 2
while IFS= read -r torrent_path; do
  transmission-remote 127.0.0.1:9091 --add "${torrent_path}" --download-dir /data/files
done < /data/torrents/all.txt

wait "${daemon_pid}"
