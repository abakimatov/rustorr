#!/bin/sh
# Runs inside a target with its FUSE mount at $1 and the media fixture
# torrent loaded: prints what a local program sees of the mount, one command
# at a time with its exit status. tools/r2.sh fuse-probe normalizes and
# compares the output of both targets.
M=${1:-/mnt/torrserver}
T="$M/other/Медиа коллекция"
F="$T/01 Пример/Фильм.mkv"
run() {
  printf '$ %s\n' "$*"
  "$@" 2>&1
  printf 'exit=%s\n' "$?"
}
run sh -c "grep ' $M ' /proc/self/mounts"
run ls -la "$M"
run ls -la "$M/other"
run ls -la "$T"
run ls -la "$T/01 Пример"
for path in "$M" "$M/other" "$T" "$T/01 Пример" "$F"; do
  run stat -c '%n|%F|%a|%s|%b|%B|%h|%u|%g|%X|%Y|%Z|%i' "$path"
done
run sh -c "head -c 32 '$F' | od -An -tx1"
run sh -c "dd if='$F' bs=4096 skip=64 count=2 2>/dev/null | md5sum"
run sh -c "md5sum < '$F'"
run sh -c "tail -c 16 '$F' | od -An -tx1"
run touch "$M/new"
run touch "$F"
run mkdir "$M/dir"
run rm -f "$F"
run rmdir "$T/01 Пример"
run mv "$F" "$M/moved"
run sh -c "echo x > '$F'"
run ls "$F/extra"
run ls "$M/nope"
run cat "$T"
run df -P "$M"
