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
COPY .sqlx .sqlx
# Demo seed photos, compiled into the api binary (`api admin seed-demo`).
COPY fixtures/images/demo fixtures/images/demo
ENV SQLX_OFFLINE=true
RUN cargo build --release --locked --bin api --bin worker

# Typst CLI (WP12 PDFs: invoices, packing slips, label sheets), a static musl binary pinned
# by version and checksum.
FROM debian:trixie-slim AS typst
ARG TARGETARCH
ARG TYPST_VERSION=0.15.1
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl xz-utils \
 && rm -rf /var/lib/apt/lists/*
RUN set -eu; \
    case "${TARGETARCH:-amd64}" in \
      amd64) arch=x86_64; sum=a6d077d0a95eed5a2eba715b2dae06be954f624ccbf85758a03f389ded33118c ;; \
      arm64) arch=aarch64; sum=5aa8d74a3d906e60ea12a66ac2f37f8eef1b14cbad7182a745e393a10c23dcee ;; \
      *) echo "unsupported arch ${TARGETARCH}" >&2; exit 1 ;; \
    esac; \
    curl -fsSL -o /tmp/typst.tar.xz \
      "https://github.com/typst/typst/releases/download/v${TYPST_VERSION}/typst-${arch}-unknown-linux-musl.tar.xz"; \
    echo "${sum}  /tmp/typst.tar.xz" | sha256sum -c -; \
    tar -xJf /tmp/typst.tar.xz -C /tmp; \
    install -m 0755 "/tmp/typst-${arch}-unknown-linux-musl/typst" /usr/local/bin/typst

# Distroless: glibc + CA certs, no shell or package manager, runs as uid 65532.
FROM gcr.io/distroless/cc-debian13:nonroot@sha256:54df941ed0d06a1bd95ef5e0ce391fd8d9f94b64782dc9a60062727849ee3f97 AS runtime
COPY --from=builder /src/target/release/api /src/target/release/worker /usr/local/bin/
COPY --from=typst /usr/local/bin/typst /usr/local/bin/typst
USER nonroot:nonroot
EXPOSE 8000
CMD ["/usr/local/bin/api"]
