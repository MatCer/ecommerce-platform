# syntax=docker/dockerfile:1
# Builds the `api` and `worker` binaries into one small image; compose picks the command.
# cargo-chef caches the dependency build, so source edits only recompile workspace crates.

FROM lukemathwalker/cargo-chef:0.1.78-rust-1.98.1-slim-trixie AS chef
WORKDIR /src
ENV CARGO_BUILD_JOBS=6

FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY crates crates
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
# aws-lc-sys (rustls crypto provider) builds C code with cmake.
RUN apt-get update \
 && apt-get install -y --no-install-recommends cmake \
 && rm -rf /var/lib/apt/lists/*
COPY --from=planner /src/recipe.json recipe.json
RUN cargo chef cook --release --locked --recipe-path recipe.json
COPY Cargo.toml Cargo.lock ./
COPY crates crates
COPY migrations migrations
RUN cargo build --release --locked --bin api --bin worker

# Distroless: glibc + CA certs, no shell or package manager, runs as uid 65532.
FROM gcr.io/distroless/cc-debian13:nonroot@sha256:54df941ed0d06a1bd95ef5e0ce391fd8d9f94b64782dc9a60062727849ee3f97 AS runtime
COPY --from=builder /src/target/release/api /src/target/release/worker /usr/local/bin/
USER nonroot:nonroot
EXPOSE 8000
CMD ["/usr/local/bin/api"]
