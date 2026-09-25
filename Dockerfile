# syntax=docker/dockerfile:1
# Build each target on the builder's native architecture. On an arm64 builder
# the amd64 image is cross-compiled, rather than compiled under QEMU.
ARG RUST_VERSION=1.90
ARG NODE_VERSION=24

# The web interface (R8): built once, on the builder's platform, and embedded
# into the binary by rustorr-http's build script.
FROM --platform=$BUILDPLATFORM node:${NODE_VERSION}-bookworm-slim AS web
WORKDIR /web
COPY web/package.json web/package-lock.json ./
ARG RUSTORR_CACHE_ID=""
RUN --mount=type=cache,id=rustorr-npm${RUSTORR_CACHE_ID},target=/root/.npm npm ci --no-audit --no-fund
COPY web ./
RUN npm run build

FROM --platform=$BUILDPLATFORM rust:${RUST_VERSION}-bookworm AS build

ARG TARGETARCH
ARG RUSTORR_FEATURES=""
WORKDIR /workspace

# librqbit's rustls path builds aws-lc-sys, and state deliberately bundles
# SQLite. Both compile C, so install complete sysroots for both Linux targets.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        gcc-aarch64-linux-gnu \
        gcc-x86-64-linux-gnu \
        libc6-dev-arm64-cross \
        libc6-dev-amd64-cross \
    && rm -rf /var/lib/apt/lists/* \
    && rustup target add aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu

ENV CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
    CC_x86_64_unknown_linux_gnu=x86_64-linux-gnu-gcc \
    CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc

# Development files of the target architecture for optional features, for
# example the GStreamer ones for `gstreamer`. Multiarch packages sit beside
# the cross toolchains; pkg-config is pointed at them per target below.
ARG RUSTORR_BUILD_PACKAGES=""
RUN if [ -n "${RUSTORR_BUILD_PACKAGES}" ]; then \
        dpkg --add-architecture "${TARGETARCH}" \
        # Multi-Arch libraries must have the same version on both
        # architectures, and bookworm-security publishes them per
        # architecture at different times; the builder only links against
        # them, so it takes both from bookworm itself.
        && printf 'Package: *\nPin: release l=Debian-Security\nPin-Priority: 1\n' \
            > /etc/apt/preferences.d/builder-without-security \
        && apt-get update \
        && apt-get upgrade -y --allow-downgrades \
        && apt-get install -y --no-install-recommends pkg-config \
            $(for package in ${RUSTORR_BUILD_PACKAGES}; do printf '%s:%s ' "${package}" "${TARGETARCH}"; done) \
        && rm -rf /var/lib/apt/lists/*; \
    fi

COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
COPY --from=web /web/dist ./web/dist

# A different RUSTORR_CACHE_ID builds from empty caches (tools/release.sh
# verify: the same bytes either way).
ARG RUSTORR_CACHE_ID=""
RUN --mount=type=cache,id=rustorr-cargo-registry${RUSTORR_CACHE_ID},target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=rustorr-cargo-target${RUSTORR_CACHE_ID},target=/workspace/target,sharing=locked \
    case "${TARGETARCH}" in \
        arm64) target=aarch64-unknown-linux-gnu; multiarch=aarch64-linux-gnu ;; \
        amd64) target=x86_64-unknown-linux-gnu; multiarch=x86_64-linux-gnu ;; \
        *) echo "unsupported target architecture: ${TARGETARCH}" >&2; exit 1 ;; \
    esac \
    && export PKG_CONFIG_ALLOW_CROSS=1 PKG_CONFIG_PATH="/usr/lib/${multiarch}/pkgconfig" \
    && cargo build --locked --release --package rustorr-server --target "${target}" ${RUSTORR_FEATURES:+--features "${RUSTORR_FEATURES}"} \
    && install -D -m 0755 "target/${target}/release/rustorr" /out/rustorr

# The binary alone, for release archives (tools/release.sh):
# docker buildx build --target binary --output type=local,dest=<dir> .
FROM scratch AS binary
COPY --from=build /out/rustorr /rustorr

FROM debian:bookworm-slim AS runtime

# Optional runtime capabilities, for example `ffmpeg` for /ffp; the default
# image has none.
ARG RUSTORR_RUNTIME_PACKAGES=""

# rustls uses the system root store for HTTPS trackers. /data must already be
# writable by this uid: Docker copies directory metadata into a new volume.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates ${RUSTORR_RUNTIME_PACKAGES} \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 65532 rustorr \
    && useradd --uid 65532 --gid rustorr --no-create-home --shell /usr/sbin/nologin rustorr \
    && install -d --owner=rustorr --group=rustorr /data

COPY --from=build /out/rustorr /usr/local/bin/rustorr

USER rustorr:rustorr
ENV RUSTORR_DATA_DIR=/data \
    RUSTORR_LISTEN=0.0.0.0:8090
EXPOSE 8090
VOLUME ["/data"]
# The image has no curl: the binary asks itself (`rustorr health`).
HEALTHCHECK --interval=30s --timeout=10s --start-period=30s --retries=3 CMD ["rustorr", "health"]
ENTRYPOINT ["rustorr"]
