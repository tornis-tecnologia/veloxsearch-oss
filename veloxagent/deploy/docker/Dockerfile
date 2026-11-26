# Copyright (C) 2026 Tornis Desenvolvimento
# SPDX-License-Identifier: AGPL-3.0-only
#
# Velox Agent container image (ADR-069 amendment 1).
#
# Built from the REPOSITORY ROOT — the agent links the shared crates under
# ./crates (velox-policy, velox-integrations), so a veloxagent/-only context
# cannot compile it. Dockerfile.dockerignore next to this file allowlists
# what the build reads; nothing else of the tree is sent:
#
#   docker build -f veloxagent/deploy/docker/Dockerfile -t velox-agent .
#
# Three stages, one recipe for every artifact:
#   build     static musl `velox-agent` (rust:alpine), native to --platform
#   artifact  FROM scratch, the binary + LICENSE: what the release tarball
#             packs (`--target artifact --output type=local,dest=out`)
#   (final)   the official fluent/fluent-bit image, UNMODIFIED, plus the
#             agent. Fluent Bit is upstream's build byte for byte; the image
#             redistributes it, which is what ADR-069 amendment 1 allows.
#
# The final base is distroless: no shell, no package manager. That is why the
# agent enrols from VELOX_MESH_URL / VELOX_ENROLL_TOKEN itself (`run`) instead
# of an entrypoint script.

ARG RUST_VERSION=1.96
ARG FLUENT_BIT_VERSION=5.2.0

FROM rust:${RUST_VERSION}-alpine AS build
# musl-dev: the C runtime the static link needs. cmake/perl/make/clang:
# aws-lc-sys (rustls' provider, the product's one crypto library) builds
# its C sources here — no prebuilt object is trusted.
RUN apk add --no-cache musl-dev cmake make perl clang
WORKDIR /src
COPY crates/velox-integrations crates/velox-integrations
COPY crates/velox-policy crates/velox-policy
COPY veloxagent veloxagent
WORKDIR /src/veloxagent
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/veloxagent/target,sharing=locked \
    cargo build --release --locked -p velox-agent \
 && install -D -m 0755 target/release/velox-agent /out/velox-agent \
 && strip /out/velox-agent \
 && /out/velox-agent version

FROM scratch AS artifact
COPY --from=build /out/velox-agent /velox-agent
COPY LICENSE /LICENSE

FROM fluent/fluent-bit:${FLUENT_BIT_VERSION}
ARG VELOX_VERSION=""
ARG VELOX_BUILD_COMMIT=""
LABEL org.opencontainers.image.title="velox-agent" \
      org.opencontainers.image.description="Velox Agent (Velox Mesh fleet agent) with the upstream Fluent Bit it supervises" \
      org.opencontainers.image.version="${VELOX_VERSION}" \
      org.opencontainers.image.revision="${VELOX_BUILD_COMMIT}" \
      org.opencontainers.image.source="https://github.com/tornis-tecnologia/veloxsearch-oss" \
      org.opencontainers.image.licenses="AGPL-3.0-only"

# The same layout as a host install (deploy/install.sh), so `doctor` output,
# docs and support bundles read the same everywhere.
COPY --from=build /out/velox-agent /opt/velox-agent/bin/velox-agent
COPY LICENSE /opt/velox-agent/LICENSE
ENV VELOX_AGENT_STATE=/opt/velox-agent/state \
    VELOX_FLUENT_BIT=/fluent-bit/bin/fluent-bit

# The identity and the engine's offsets: a volume, or every restart re-enrols
# a new agent and re-reads every log from the top.
VOLUME ["/opt/velox-agent/state"]

# Root inside the container, on purpose: reading the host's mounted /var/log
# needs CAP_DAC_READ_SEARCH, and Docker hands capabilities to a non-root
# user only through file capabilities this distroless base cannot set. The
# compose file drops every other capability and mounts the rootfs read-only.
ENTRYPOINT ["/opt/velox-agent/bin/velox-agent"]
CMD ["run"]
