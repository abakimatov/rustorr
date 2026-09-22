# syntax=docker/dockerfile:1
# Build each target on the builder's native architecture. On an arm64 builder
# the amd64 image is cross-compiled, rather than compiled under QEMU.
ARG RUST_VERSION=1.90
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

COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates

RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/workspace/target,sharing=locked \
    case "${TARGETARCH}" in \
        arm64) target=aarch64-unknown-linux-gnu ;; \
        amd64) target=x86_64-unknown-linux-gnu ;; \
        *) echo "unsupported target architecture: ${TARGETARCH}" >&2; exit 1 ;; \
    esac \
    && cargo build --locked --release --package rustorr-server --target "${target}" ${RUSTORR_FEATURES:+--features "${RUSTORR_FEATURES}"} \
    && install -D -m 0755 "target/${target}/release/rustorr" /out/rustorr

FROM debian:bookworm-slim AS runtime

# rustls uses the system root store for HTTPS trackers. /data must already be
# writable by this uid: Docker copies directory metadata into a new volume.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
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
ENTRYPOINT ["rustorr"]
