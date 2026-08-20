#!/usr/bin/env bash
# Build the veloxsearch image and load it into your LOCAL minikube.
#
# This is the SAFE local-dev path: it builds the same image as
# deploy/build-image.sh but side-loads it into a LOCAL minikube cluster with
# `minikube image load` — it NEVER touches the production nodes.
#
# (The production side-load lives in deploy/build-image.sh and is opt-in only:
#  `deploy/build-image.sh --target prod`.)
#
# Usage: deploy/build-image-local.sh [debug|release]   (default: release)
set -euo pipefail

PROFILE="${1:-release}"
TAG="veloxsearch:0.7.0"   # keep in lock-step with deploy/build-image.sh (DEPLOY.md §1)
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
STAGE="$(mktemp -d)"

if ! command -v minikube >/dev/null 2>&1; then
  echo "minikube not found — install it, then rerun. See docs/INSTALL.md (minikube)." >&2
  exit 1
fi

echo ">> cargo build ($PROFILE)"
if [ "$PROFILE" = "release" ]; then
  cargo build --release --manifest-path "$REPO_ROOT/Cargo.toml"
else
  cargo build --manifest-path "$REPO_ROOT/Cargo.toml"
fi

echo ">> building frontend (vite)"
( cd "$REPO_ROOT/frontend" && npm ci && npm run build )

echo ">> staging $PROFILE artifacts"
cp "$REPO_ROOT/target/$PROFILE/veloxsearch" "$STAGE/veloxsearch"
cp -r "$REPO_ROOT/frontend/build" "$STAGE/dist"
cp "$REPO_ROOT/deploy/Dockerfile" "$STAGE/Dockerfile"
# AGPL-3.0-only: the Dockerfile COPYs it, so it has to be staged here too.
cp "$REPO_ROOT/LICENSE" "$STAGE/LICENSE"

echo ">> docker build $TAG"
sudo docker build -t "$TAG" "$STAGE"

echo ">> minikube image load $TAG (LOCAL cluster only)"
minikube image load "$TAG"

rm -rf "$STAGE"
echo ">> done: $TAG loaded into local minikube (prod untouched)"
