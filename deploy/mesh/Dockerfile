# Velox Mesh runtime image (ADR-068: separate binary, separate image, separate
# release cadence from the core). Expects prebuilt artifacts staged into the
# build context by deploy/mesh/build-image.sh:
#   ./velox-mesh  (from `cd veloxagent && cargo build --release -p velox-mesh`)
#   ./LICENSE
#
# Same base as the core's deploy/Dockerfile, for the same reason: trixie-slim
# matches the glibc the binary is built against on Debian 13. A static musl
# build (`build-image.sh --target x86_64-unknown-linux-musl`) runs here too;
# the base is chosen for the glibc case, not required by the musl one.
FROM debian:trixie-slim

# `upgrade` is NOT optional — see the core's Dockerfile (#118): without it the
# base image's packages stay frozen at its build date however often we rebuild.
# ca-certificates: the Mesh makes no outbound TLS call today (Postgres is
# in-cluster and NoTls, store.rs), but licence activation is a planned HTTPS
# call (veloxagent/docs/ARCHITECTURE.md §6), and an image that cannot
# verify a public certificate fails that in a way that reads as a network bug.
RUN apt-get update \
 && apt-get upgrade -y --no-install-recommends \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*

# Non-root system user, same uid as the core (#118): deploy/mesh/mesh.yaml
# pins runAsUser 10001 and the `restricted` Pod Security Standard refuses a
# pod that can run as root.
RUN useradd --uid 10001 --user-group --no-create-home \
      --shell /usr/sbin/nologin velox

# Build identity as OCI labels (#55), informational only — the version the
# Mesh logs at start is compiled in (CARGO_PKG_VERSION), not read from here.
ARG VELOX_VERSION=""
ARG VELOX_BUILD_COMMIT=""
LABEL org.opencontainers.image.title="velox-mesh" \
      org.opencontainers.image.version="${VELOX_VERSION}" \
      org.opencontainers.image.revision="${VELOX_BUILD_COMMIT}" \
      org.opencontainers.image.licenses="AGPL-3.0-only"

WORKDIR /app
COPY velox-mesh /app/velox-mesh
# AGPL-3.0-only: ship the license alongside the binary it covers.
COPY LICENSE /app/LICENSE

# The binary defaults to 127.0.0.1:4320, which is right for a dev box and
# unreachable in a pod; the image binds every interface, and the Service in
# mesh.yaml decides who can reach it. VELOX_MESH_PG_URL and
# VELOX_MESH_SERVICE_TOKEN are deliberately NOT defaulted: both are secrets,
# and the binary refuses to start without the first (fail closed, main.rs).
ENV VELOX_MESH_ADDR=0.0.0.0:4320

EXPOSE 4320
# Numeric on purpose: Kubernetes' `runAsNonRoot` cannot resolve a named USER
# against the image's /etc/passwd ("unable to validate image user").
USER 10001:10001
CMD ["/app/velox-mesh"]
