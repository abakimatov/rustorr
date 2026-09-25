#!/bin/sh
set -eu
/usr/sbin/sshd
exec dockerd-entrypoint.sh "$@"
