# Runtime image. Expects prebuilt artifacts staged into the build context:
#   ./veloxsearch  (the server binary, from `cargo build --release`)
#   ./dist/        (the React SPA: Vite build output — index.html + hashed,
#                   minified assets under assets/ + favicon.ico, see issue #31)
# Base is trixie-slim to match the glibc the binary is built against (Debian 13).
# See deploy/build-image.sh for the staging + build flow.
FROM debian:trixie-slim

# `upgrade` is NOT optional (#118). Without it every package the base image
# shipped stays frozen at the BASE IMAGE's build date, however often we rebuild
# — `update` only refreshes the index and `install` only touches what it is
# asked for. That single omission accounted for 36 of the 48 HIGH findings on
# the first published image: CVE-2026-53612/53613/53614/53615 in util-linux
# 2.41-5, fixed in 2.41.5-0+deb13u1 and sitting in the archive the whole time,
# multiplied across the nine binary packages built from that source.
RUN apt-get update \
 && apt-get upgrade -y --no-install-recommends \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*

# Run as a non-root system user (#118). The container had no USER, so it ran as
# root — not a CVE, but the multiplier on every CVE, and the difference between
# a container escape being hard and being one step. It also matters more for us
# than for most: the product is applied to strangers' clusters, and a namespace
# with the `restricted` Pod Security Standard REFUSES a pod that can run as root.
RUN useradd --uid 10001 --user-group --no-create-home \
      --shell /usr/sbin/nologin velox

WORKDIR /app
COPY veloxsearch /app/veloxsearch
COPY dist /app/dist
# AGPL-3.0-only: ship the license alongside the binary it covers.
COPY LICENSE /app/LICENSE

ENV VELOX_STATIC_DIR=/app/dist \
    VELOX_SITE_ADDR=0.0.0.0:3000

EXPOSE 3000
# Numeric on purpose: Kubernetes' `runAsNonRoot` check reads the image's USER
# and cannot resolve a name against the image's /etc/passwd, so a named USER
# would still be rejected as "unable to validate image user".
USER 10001:10001
CMD ["/app/veloxsearch"]
