#!/usr/bin/env bash
# Copyright (C) 2026 Tornis Desenvolvimento
# SPDX-License-Identifier: AGPL-3.0-only
#
# The build boundary ADR-068 rests on. `velox-agent` is a static binary for
# hosts outside the cluster and must not link the control plane (`veloxsearch`)
# or the Kubernetes client (`kube*`, `k8s-openapi`); `velox-mesh` talks to the
# core over HTTP and must not link it either. Checked on the RESOLVED graph, so
# a transitive pull-in (say, through a future shared crate) fails exactly like
# a direct one. The `veloxagent` CI job runs it; locally:
#
#   ./.github/scripts/check-agent-boundary.sh
set -euo pipefail

cd "$(dirname "$0")/../.."

fail=0
check() {
  local crate=$1 pattern=$2 hits
  hits=$(cargo tree --manifest-path veloxagent/Cargo.toml -p "$crate" -e normal --prefix none -f '{p}' \
    | awk '{print $1}' | sort -u | grep -E "$pattern" || true)
  if [ -n "$hits" ]; then
    echo "$crate links what ADR-068 forbids:" >&2
    # shellcheck disable=SC2086
    printf '  %s\n' $hits >&2
    fail=1
  else
    echo "$crate: dependency boundary holds"
  fi
}

check velox-agent '^(veloxsearch|kube|kube-.*|k8s-openapi)$'
check velox-mesh  '^veloxsearch$'
exit $fail
