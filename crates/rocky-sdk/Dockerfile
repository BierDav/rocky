# syntax=docker/dockerfile:1.7
# Container image for the always-on custody peer binary.
#
# The image is built for GHCR by the image jobs of
# `.github/workflows/check.yml`. Its deployment is described in
# `docs/runtime/always-on-peer.md`.

FROM rust:1.99-bookworm AS builder
WORKDIR /src
# The workspace resolves every member manifest, so the whole repository is the
# build context; `.dockerignore` drops everything the build does not read.
COPY . .
RUN apt-get update \
    && apt-get install --yes --no-install-recommends libclang-dev \
    && rm -rf /var/lib/apt/lists/*
RUN cargo build --release --locked -p rocky-sdk --bin rocky-peer

FROM debian:bookworm-slim AS runtime
# The peer dials iroh relays and the crates.io-free runtime has no CA store of
# its own; the peer needs it for TLS to the relay.
RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /src/target/release/rocky-peer /usr/local/bin/rocky-peer
# The container image is a launcher only. Application-specific configuration and
# persistent custody are mounted at start; see the runbook.
ENTRYPOINT ["/usr/local/bin/rocky-peer"]
