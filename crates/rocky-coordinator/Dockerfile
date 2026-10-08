# syntax=docker/dockerfile:1.7
# Container image for the public coordination service's voter binary.
#
# The image is built and pushed to public GHCR by
# `.github/workflows/coordinator-image.yaml`. Its deployment is described in
# `docs/runtime/coordinator-operations.md`; the operator runbook is
# `docs/runtime/coordinator-operations.md`.

FROM rust:1.99-bookworm AS builder
WORKDIR /src
# The workspace resolves every member manifest, so the whole repository is the
# build context; `.dockerignore` drops everything the build does not read.
COPY . .
RUN cargo build --release --locked -p rocky-coordinator --bin rocky-voter

FROM debian:bookworm-slim AS runtime
# The voter dials iroh relays and the crates.io-free runtime has no CA store of
# its own; the client lane needs it for TLS to the relay.
RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /src/target/release/rocky-voter /usr/local/bin/rocky-voter
# The container image is a launcher only. Provider-specific configuration,
# keys and the trust bundle are mounted at start; see the runbook.
ENTRYPOINT ["/usr/local/bin/rocky-voter"]
