#!/usr/bin/env bash
# Copyright (C) 2026 Tornis Desenvolvimento
# SPDX-License-Identifier: AGPL-3.0-only
#
# Build the VeloxSearch image and side-load it into your LOCAL minikube.
#
# This is the safe local-dev path: it delegates the build to
# deploy/build-image.sh (never with --push) and loads the result into the
# minikube cluster on this machine with `minikube image load`. No registry is
# contacted and no remote cluster is touched.
#
# Usage: deploy/build-image-local.sh [debug|release]   (default: release)
#
# See docs/DEVELOPMENT.md for the full local loop and docs/DEPLOY.md for the
# release path.
set -euo pipefail

PROFILE="${1:-release}"
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# A local-only tag: deliberately NOT the docker.io/... reference, so a stray
# `docker push` cannot publish a dev build, and so `imagePullPolicy` can never
# silently fetch a remote image over the side-loaded one.
TAG="veloxsearch:dev"

if ! command -v minikube >/dev/null 2>&1; then
  echo "minikube not found — install it, then rerun. See docs/INSTALL.md (minikube)." >&2
  exit 1
fi

"$REPO_ROOT/deploy/build-image.sh" --profile "$PROFILE" --tag "$TAG"

echo ">> minikube image load $TAG (LOCAL cluster only)"
minikube image load "$TAG"

echo ">> done: $TAG loaded into local minikube."
echo ">> To run it, point the Deployment at the side-loaded tag:"
echo "     kubectl -n veloxsearch-system set image deploy/veloxsearch veloxsearch=$TAG"
echo "     kubectl -n veloxsearch-system patch deploy/veloxsearch --type=json \\"
echo "       -p='[{\"op\":\"replace\",\"path\":\"/spec/template/spec/containers/0/imagePullPolicy\",\"value\":\"IfNotPresent\"}]'"
