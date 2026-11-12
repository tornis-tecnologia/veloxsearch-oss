#!/usr/bin/env bash
# Copyright (C) 2026 Tornis Desenvolvimento
# SPDX-License-Identifier: AGPL-3.0-only
#
# Build the Velox Mesh container image.
#
# Mirrors deploy/build-image.sh for the core: the image is runtime-only
# (deploy/mesh/Dockerfile) and expects a prebuilt `velox-mesh` binary staged
# into the build context, which is what this script assembles. The binary
# comes from the separate `veloxagent/` workspace (ADR-068), never the root
# crate. Nothing is pushed unless you ask with --push, and nothing is loaded
# into a cluster.
#
# Usage:
#   deploy/mesh/build-image.sh [--profile debug|release] [--tag <image:tag>]
#                              [--target <triple>] [--push] [--skip-build]
#
#   --profile     cargo profile to build and stage   (default: release)
#   --tag         full image reference to build      (default: see IMAGE/VERSION)
#   --target      cargo target triple, e.g. x86_64-unknown-linux-musl for a
#                 static binary (default: the host's, a glibc build that
#                 matches the trixie-slim base like the core's image)
#   --push        docker push the tag after building (default: no)
#   --skip-build  stage a binary that is already built; do not re-run cargo
#
# The default tag is derived from veloxagent/Cargo.toml's workspace version, so
# the binary (which logs CARGO_PKG_VERSION at start) and the image never drift;
# deploy/mesh/mesh.yaml pins the same version. The Mesh versions on its own
# cadence (ADR-068), so this is NOT the core's version.
#
# VELOX_BUILD_COMMIT stamps the OCI revision label only; unset, no revision is
# stamped — deliberately never derived from a possibly dirty local checkout.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
WORKSPACE="$REPO_ROOT/veloxagent"

# Next to the core's docker.io/tornistecnologia/veloxsearch-oss: same
# namespace, its own repository, because it is its own image (ADR-068).
IMAGE="${VELOX_MESH_IMAGE:-docker.io/tornistecnologia/veloxsearch-mesh}"
# The first `version = "..."` line is [workspace.package]'s; dependency
# versions are inline tables (`x = { version = ... }`) and never start a line.
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$WORKSPACE/Cargo.toml" | head -1)"
PROFILE="release"
TARGET=""
TAG=""
PUSH="no"
SKIP_BUILD="no"

while [ $# -gt 0 ]; do
  case "$1" in
    --profile) PROFILE="${2:?--profile needs a value}"; shift 2 ;;
    --tag)     TAG="${2:?--tag needs a value}"; shift 2 ;;
    --target)  TARGET="${2:?--target needs a value}"; shift 2 ;;
    --push)    PUSH="yes"; shift ;;
    --skip-build) SKIP_BUILD="yes"; shift ;;
    -h|--help) sed -n '5,32p' "$0"; exit 0 ;;
    *) echo "unknown option: $1 (try --help)" >&2; exit 2 ;;
  esac
done

case "$PROFILE" in
  debug|release) ;;
  *) echo "--profile must be debug or release, got: $PROFILE" >&2; exit 2 ;;
esac

[ -n "$VERSION" ] || { echo "no workspace version in $WORKSPACE/Cargo.toml" >&2; exit 1; }
TAG="${TAG:-$IMAGE:$VERSION}"

# cargo puts a --target build under target/<triple>/<profile>.
if [ -n "$TARGET" ]; then
  BIN="veloxagent/target/$TARGET/$PROFILE/velox-mesh"
else
  BIN="veloxagent/target/$PROFILE/velox-mesh"
fi

# Same sudo fallback as the core's script: only when the socket is out of
# reach, so CI (where it is reachable) stays sudo-free.
DOCKER="docker"
if ! docker info >/dev/null 2>&1; then
  DOCKER="sudo docker"
fi

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

if [ "$SKIP_BUILD" = "no" ]; then
  echo ">> cargo build -p velox-mesh ($PROFILE${TARGET:+, $TARGET})"
  args=(build --manifest-path "$WORKSPACE/Cargo.toml" -p velox-mesh)
  [ "$PROFILE" = "release" ] && args+=(--release)
  [ -n "$TARGET" ] && args+=(--target "$TARGET")
  cargo "${args[@]}"
fi

echo ">> staging $PROFILE artifacts"
for required in "$BIN" "LICENSE"; do
  [ -e "$REPO_ROOT/$required" ] || {
    echo "missing build input: $required (drop --skip-build to produce it)" >&2
    exit 1
  }
done
cp "$REPO_ROOT/$BIN" "$STAGE/velox-mesh"
cp "$REPO_ROOT/deploy/mesh/Dockerfile" "$STAGE/Dockerfile"
# AGPL-3.0-only: the Dockerfile COPYs it, so it has to be staged here too.
cp "$REPO_ROOT/LICENSE" "$STAGE/LICENSE"

# The label mirrors the binary; with --skip-build this script cannot know which
# commit the staged binary came from, so it stamps no revision rather than a
# guess (same rule as the core's script).
REVISION=""
if [ "$SKIP_BUILD" = "no" ]; then REVISION="${VELOX_BUILD_COMMIT:-}"; fi

echo ">> docker build $TAG"
$DOCKER build -t "$TAG" \
  --build-arg VELOX_VERSION="$VERSION" \
  --build-arg VELOX_BUILD_COMMIT="$REVISION" \
  "$STAGE"

if [ "$PUSH" = "yes" ]; then
  echo ">> docker push $TAG"
  $DOCKER push "$TAG"
  echo ">> pushed. Pin deploy/mesh/mesh.yaml to this digest:"
  $DOCKER inspect --format='{{index .RepoDigests 0}}' "$TAG" || true
else
  echo ">> built $TAG (not pushed; pass --push to publish)"
fi
