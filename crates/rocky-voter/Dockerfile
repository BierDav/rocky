# syntax=docker/dockerfile:1.7
# Container image for the public coordination service's voter and relay.
#
# The image jobs of `.github/workflows/release.yml` build the binary natively
# and pass it as the `binary` build context, which replaces the `binary` stage
# below, so the release never compiles inside Docker. A plain `docker build`
# from the repository root compiles it in the `builder` stage instead.

FROM rust:1.99-bookworm AS builder
WORKDIR /src
# The workspace resolves every member manifest, so the whole repository is the
# build context; `.dockerignore` drops everything the build does not read.
COPY . .
# The image has no sccache; the repository config names it as rustc-wrapper.
ENV RUSTC_WRAPPER=
RUN cargo build --release --locked -p rocky-voter --bin rocky-voter

FROM scratch AS binary
COPY --from=builder /src/target/release/rocky-voter /rocky-voter

# Ubuntu 24.04 matches the release build runners, so the prebuilt binary finds
# the glibc it was linked against; the bookworm builder's binary needs less.
FROM ubuntu:24.04 AS runtime
# The voter dials iroh relays and the runtime has no CA store of its own; the
# client lane needs it for TLS to the relay.
RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
# Workflow artifacts drop the executable bit.
COPY --from=binary --chmod=0755 /rocky-voter /usr/local/bin/rocky-voter
# The container image is a launcher only. Provider-specific configuration,
# keys and the trust bundle are mounted at start.
ENTRYPOINT ["/usr/local/bin/rocky-voter"]
