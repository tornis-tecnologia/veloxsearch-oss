#!/usr/bin/env bash
# Copyright (C) 2026 Tornis Desenvolvimento
# SPDX-License-Identifier: AGPL-3.0-only
#
# Derive the upgrade manifest from an install manifest (ADR-057, #54).
#
#   deploy/upgrade-manifest.sh <install.yaml> > upgrade.yaml
#
# The upgrade manifest is the install manifest minus TWO documents:
#
#   - the `veloxsearch-bootstrap` ClusterRoleBinding to cluster-admin. That
#     binding is for the one-time self-bootstrap (ADR-027); an upgrade finds the
#     operator and cert-manager already installed and needs nothing it grants,
#     so applying the upgrade manifest never re-grants cluster-admin.
#   - the `veloxsearch-env` ConfigMap (#128). It holds the operator's settings
#     (ADR-034); install.yaml creates it once with the shipped defaults, and
#     re-applying it on every upgrade reset whatever the operator had set there.
#     The binary has a default for every key, so a release that adds a key needs
#     no new ConfigMap (ADR-057, tested in src/install_env.rs).
#
# Everything else — the runtime RBAC a new release may need, the Deployment,
# the Service — is byte-identical.
#
# `kubectl apply` without --prune deletes nothing, so on an install whose
# bootstrap is still owed (no default StorageClass yet, ADR-061) the existing
# binding is left exactly as it was, and the existing ConfigMap is kept. A
# GitOps tool that prunes is different: it deletes what the manifest stops
# declaring, so such an install must own `veloxsearch-env` itself (DEPLOY.md).
#
# release.yml runs this on the digest-pinned install.yaml; ci.yml runs it on
# deploy/install.yaml on every PR, so a manifest it cannot split fails there.
set -euo pipefail

[ $# -eq 1 ] || { echo "usage: $0 <install.yaml>" >&2; exit 2; }
in=$1

# Documents are separated by lines that are exactly `---`. A document is
# dropped when it is the ClusterRoleBinding named veloxsearch-bootstrap or the
# ConfigMap named veloxsearch-env; its leading comments go with it. Every other
# line is copied unchanged. Each must be found exactly once, or the install
# manifest changed shape and this script must be revisited.
out=$(awk '
  function named(kind, name) {
    return buf ~ ("(^|\n)kind: " kind "\n") && buf ~ ("\n  name: " name "\n")
  }
  function flush() {
    if (named("ClusterRoleBinding", "veloxsearch-bootstrap")) binding++
    else if (named("ConfigMap", "veloxsearch-env")) env++
    else printf "%s", buf
    buf = ""
  }
  /^---$/ { flush() }
  { buf = buf $0 "\n" }
  END {
    flush()
    bad = 0
    if (binding != 1) {
      printf "upgrade-manifest: expected exactly one veloxsearch-bootstrap binding, found %d\n", binding > "/dev/stderr"
      bad = 1
    }
    if (env != 1) {
      printf "upgrade-manifest: expected exactly one veloxsearch-env ConfigMap, found %d\n", env > "/dev/stderr"
      bad = 1
    }
    if (bad) exit 1
  }
' "$in")

if grep -Eq '^[[:space:]]*name: (cluster-admin|veloxsearch-bootstrap)[[:space:]]*$' <<<"$out"; then
  echo "upgrade-manifest: the result still names cluster-admin or veloxsearch-bootstrap" >&2
  exit 1
fi
printf '%s\n' "$out"
