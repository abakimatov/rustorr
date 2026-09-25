#!/bin/sh
# Release artifacts (R10): the binary for x86_64 and aarch64 Linux as
# .tar.gz archives, the Docker images (plain and GStreamer) for both as
# `docker save` archives, and SHA256SUMS. `verify` builds the binaries again
# without cache and compares them byte for byte.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
VERSION=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$ROOT/Cargo.toml" | head -1)
OUT=${RUSTORR_RELEASE_DIR:-$ROOT/release/$VERSION}
# Archive entries get this time: the commit's, so a rebuild matches.
SOURCE_DATE_EPOCH=$(git -C "$ROOT" log -1 --format=%ct)
export SOURCE_DATE_EPOCH

GST_BUILD_PACKAGES="libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev"
GST_RUNTIME_PACKAGES="ffmpeg libgstreamer1.0-0 gstreamer1.0-tools gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad gstreamer1.0-plugins-ugly gstreamer1.0-libav gstreamer1.0-plugins-base-apps"

usage() { echo "usage: $0 {build|binaries|images|verify}" >&2; }
say() { printf '==> %s\n' "$*" >&2; }

triple() {
  case $1 in
    amd64) echo x86_64-unknown-linux-gnu ;;
    arm64) echo aarch64-unknown-linux-gnu ;;
  esac
}

# The binary of one architecture into $2 (a directory), optionally without
# the build cache.
build_binary() {
  arch=$1 dest=$2
  shift 2
  docker buildx build --platform "linux/$arch" --target binary \
    --output "type=local,dest=$dest" "$@" -f "$ROOT/Dockerfile" "$ROOT" >&2
}

# A deterministic .tar.gz: sorted entries, fixed owner and times, no gzip
# timestamp.
pack() {
  python3 - "$@" <<'PY'
import gzip, io, os, sys, tarfile
out, top, *files = sys.argv[1:]
epoch = int(os.environ["SOURCE_DATE_EPOCH"])
buffer = io.BytesIO()
with tarfile.open(fileobj=buffer, mode="w", format=tarfile.PAX_FORMAT) as archive:
    for source, name, mode in sorted((f.split("=", 2)[0], f.split("=", 2)[1], int(f.split("=", 2)[2], 8)) for f in files):
        data = open(source, "rb").read()
        info = tarfile.TarInfo(f"{top}/{name}")
        info.size, info.mode, info.mtime = len(data), mode, epoch
        info.uid = info.gid = 0
        info.uname = info.gname = ""
        archive.addfile(info, io.BytesIO(data))
with open(out, "wb") as target:
    with gzip.GzipFile(fileobj=target, mode="wb", mtime=0, filename="") as compressed:
        compressed.write(buffer.getvalue())
PY
}

binaries() {
  mkdir -p "$OUT"
  for arch in amd64 arm64; do
    name="rustorr-$VERSION-$(triple "$arch")"
    work=$(mktemp -d)
    say "binary for $arch"
    build_binary "$arch" "$work"
    pack "$OUT/$name.tar.gz" "$name" \
      "$work/rustorr=rustorr=755" \
      "$ROOT/README.md=README.md=644" \
      "$ROOT/docs/deploy.md=docs/deploy.md=644" \
      "$ROOT/docs/release-notes.md=RELEASE-NOTES.md=644"
    rm -rf "$work"
  done
}

images() {
  mkdir -p "$OUT"
  for arch in amd64 arm64; do
    say "image for $arch"
    docker buildx build --load --platform "linux/$arch" -t "rustorr:$VERSION-$arch" \
      -f "$ROOT/Dockerfile" "$ROOT" >&2
    docker save "rustorr:$VERSION-$arch" | gzip -n > "$OUT/rustorr-$VERSION-image-$arch.tar.gz"
    say "GStreamer image for $arch"
    docker buildx build --load --platform "linux/$arch" -t "rustorr:$VERSION-gst-$arch" \
      --build-arg RUSTORR_FEATURES=gstreamer \
      --build-arg "RUSTORR_BUILD_PACKAGES=$GST_BUILD_PACKAGES" \
      --build-arg "RUSTORR_RUNTIME_PACKAGES=$GST_RUNTIME_PACKAGES" \
      -f "$ROOT/Dockerfile" "$ROOT" >&2
    docker save "rustorr:$VERSION-gst-$arch" | gzip -n > "$OUT/rustorr-$VERSION-gst-image-$arch.tar.gz"
  done
}

checksums() {
  (cd "$OUT" && shasum -a 256 ./*.tar.gz | sed 's# \./# #' > SHA256SUMS)
  say "artifacts in $OUT"
  cat "$OUT/SHA256SUMS" >&2
}

# The binaries built again from empty caches (a fresh cache id and no layer
# cache): the same bytes as the regular build.
verify() {
  status=0
  for arch in amd64 arm64; do
    first=$(mktemp -d) second=$(mktemp -d)
    say "building the $arch binary, then again from empty caches"
    build_binary "$arch" "$first"
    build_binary "$arch" "$second" --no-cache --build-arg "RUSTORR_CACHE_ID=-verify-$(date +%s)"
    a=$(shasum -a 256 "$first/rustorr" | cut -d' ' -f1)
    b=$(shasum -a 256 "$second/rustorr" | cut -d' ' -f1)
    if [ "$a" = "$b" ]; then
      say "$arch: reproducible ($a)"
    else
      say "$arch: DIFFERENT ($a vs $b)"
      status=1
    fi
    rm -rf "$first" "$second"
  done
  return $status
}

case ${1:-} in
  build) binaries; images; checksums ;;
  binaries) binaries; checksums ;;
  images) images; checksums ;;
  verify) verify ;;
  *) usage; exit 2 ;;
esac
