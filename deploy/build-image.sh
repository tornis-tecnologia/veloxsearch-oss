#!/usr/bin/env bash
# Copyright (C) 2026 Tornis Desenvolvimento
# SPDX-License-Identifier: AGPL-3.0-only
#
# Build the VeloxSearch container image.
#
# The image is runtime-only (deploy/Dockerfile): it expects a prebuilt binary
# and a prebuilt SPA staged into the build context, which is what this script
# assembles. Nothing is pushed unless you ask for it with --push, and nothing
# is loaded into a cluster — deploy/build-image-local.sh wraps this script for
# the local-minikube path.
#
# Usage:
#   deploy/build-image.sh [--profile debug|release] [--tag <image:tag>] [--push]
#
#   --profile   cargo profile to build and stage   (default: release)
#   --tag       full image reference to build      (default: see IMAGE/VERSION)
#   --push      docker push the tag after building (default: no)
#   --skip-build  stage artifacts that are already built, do not re-run
#                 cargo/npm. Useful in CI where the two ran in earlier jobs.
#
# The default tag is derived from the crate version so the manifest, the binary
# and the image never drift: deploy/install.yaml pins the same version.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

IMAGE="${VELOX_IMAGE:-docker.io/tornistecnologia/veloxsearch-oss}"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$REPO_ROOT/Cargo.toml" | head -1)"
PROFILE="release"
TAG=""
PUSH="no"
SKIP_BUILD="no"

while [ $# -gt 0 ]; do
  case "$1" in
    --profile) PROFILE="${2:?--profile needs a value}"; shift 2 ;;
    --tag)     TAG="${2:?--tag needs a value}"; shift 2 ;;
    --push)    PUSH="yes"; shift ;;
    --skip-build) SKIP_BUILD="yes"; shift ;;
    -h|--help) sed -n '5,22p' "$0"; exit 0 ;;
    *) echo "unknown option: $1 (try --help)" >&2; exit 2 ;;
  esac
done

case "$PROFILE" in
  debug|release) ;;
  *) echo "--profile must be debug or release, got: $PROFILE" >&2; exit 2 ;;
esac

TAG="${TAG:-$IMAGE:$VERSION}"

# `docker` may need sudo depending on how the daemon is set up; use it only when
# the current user cannot reach the socket, so CI (where it can) stays sudo-free.
DOCKER="docker"
if ! docker info >/dev/null 2>&1; then
  DOCKER="sudo docker"
fi

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

if [ "$SKIP_BUILD" = "no" ]; then
  echo ">> cargo build ($PROFILE)"
  if [ "$PROFILE" = "release" ]; then
    cargo build --release --manifest-path "$REPO_ROOT/Cargo.toml"
  else
    cargo build --manifest-path "$REPO_ROOT/Cargo.toml"
  fi

  echo ">> building frontend (vite)"
  ( cd "$REPO_ROOT/frontend" && npm ci && npm run build )
fi

echo ">> staging $PROFILE artifacts"
for required in "target/$PROFILE/veloxsearch" "frontend/build" "LICENSE"; do
  [ -e "$REPO_ROOT/$required" ] || {
    echo "missing build input: $required (drop --skip-build to produce it)" >&2
    exit 1
  }
done
cp "$REPO_ROOT/target/$PROFILE/veloxsearch" "$STAGE/veloxsearch"
cp -r "$REPO_ROOT/frontend/build" "$STAGE/dist"
cp "$REPO_ROOT/deploy/Dockerfile" "$STAGE/Dockerfile"
# AGPL-3.0-only: the Dockerfile COPYs it, so it has to be staged here too.
cp "$REPO_ROOT/LICENSE" "$STAGE/LICENSE"

echo ">> docker build $TAG"
$DOCKER build -t "$TAG" "$STAGE"

if [ "$PUSH" = "yes" ]; then
  echo ">> docker push $TAG"
  $DOCKER push "$TAG"
  echo ">> pushed. Pin deploy/install.yaml to this digest:"
  $DOCKER inspect --format='{{index .RepoDigests 0}}' "$TAG" || true
else
  echo ">> built $TAG (not pushed; pass --push to publish)"
fi
