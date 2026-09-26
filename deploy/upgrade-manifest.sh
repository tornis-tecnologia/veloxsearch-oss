#!/usr/bin/env bash
# Copyright (C) 2026 Tornis Desenvolvimento
# SPDX-License-Identifier: AGPL-3.0-only
#
# Derive the upgrade manifest from an install manifest (ADR-057, #54).
#
#   deploy/upgrade-manifest.sh <install.yaml> > upgrade.yaml
#
# The upgrade manifest is the install manifest minus ONE document: the
# `veloxsearch-bootstrap` ClusterRoleBinding to cluster-admin. That binding is
# for the one-time self-bootstrap (ADR-027); an upgrade finds the operator and
# cert-manager already installed and needs nothing it grants, so applying the
# upgrade manifest never re-grants cluster-admin. Everything else — the runtime
# RBAC a new release may need, the Deployment, the Service — is byte-identical.
#
# `kubectl apply` without --prune deletes nothing, so on an install whose
# bootstrap is still owed (no default StorageClass yet, ADR-061) the existing
# binding is left exactly as it was.
#
# release.yml runs this on the digest-pinned install.yaml; ci.yml runs it on
# deploy/install.yaml on every PR, so a manifest it cannot split fails there.
set -euo pipefail

[ $# -eq 1 ] || { echo "usage: $0 <install.yaml>" >&2; exit 2; }
in=$1

# Documents are separated by lines that are exactly `---`. A document is
# dropped when it is a ClusterRoleBinding named veloxsearch-bootstrap; its
# leading comments go with it. Every other line is copied unchanged.
out=$(awk '
  function flush() {
    if (buf ~ /(^|\n)kind: ClusterRoleBinding\n/ && buf ~ /\n  name: veloxsearch-bootstrap\n/) dropped++
    else printf "%s", buf
    buf = ""
  }
  /^---$/ { flush() }
  { buf = buf $0 "\n" }
  END {
    flush()
    if (dropped != 1) {
      printf "upgrade-manifest: expected exactly one veloxsearch-bootstrap binding, found %d\n", dropped > "/dev/stderr"
      exit 1
    }
  }
' "$in")

if grep -Eq '^[[:space:]]*name: (cluster-admin|veloxsearch-bootstrap)[[:space:]]*$' <<<"$out"; then
  echo "upgrade-manifest: the result still names cluster-admin or veloxsearch-bootstrap" >&2
  exit 1
fi
printf '%s\n' "$out"
