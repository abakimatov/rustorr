#!/bin/sh
set -eu
# MatriX.145 downloads rutor.ls unless the copy in its data directory is less
# than three hours old. A fresh fixture copy keeps the reference offline.
mkdir -p /config
cp /r7/rutor.ls /config/rutor.ls
touch /config/rutor.ls
exec /usr/local/bin/TorrServer "$@"
