FROM rust:1.90-bookworm

WORKDIR /workspace/tools/engine-spike
COPY tools/engine-spike/Cargo.toml tools/engine-spike/Cargo.lock ./

RUN rustup component add rustfmt clippy
RUN mkdir src && printf 'fn main() {}\n' > src/main.rs && CARGO_BUILD_JOBS=2 cargo build --locked --release && rm -rf src

COPY tools/engine-spike/src/ ./src/

RUN touch src/main.rs src/storage_probe.rs && CARGO_BUILD_JOBS=2 cargo build --locked --release

ENTRYPOINT ["/workspace/tools/engine-spike/target/release/rustorr-engine-spike"]
