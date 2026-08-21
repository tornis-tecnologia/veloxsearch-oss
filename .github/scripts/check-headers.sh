#!/usr/bin/env bash
# Copyright (C) 2026 Tornis Desenvolvimento
# SPDX-License-Identifier: AGPL-3.0-only
#
# Every source file must carry the SPDX licence header (CONTRIBUTING.md).
# Checked mechanically so a missing header costs a CI run, not a review comment.
#
# Run it locally with: ./.github/scripts/check-headers.sh
set -euo pipefail

cd "$(dirname "$0")/../.."

# Generated or vendored trees are exempt: we do not own their headers, and
# rewriting them would make the next upstream re-vendor a manual merge.
missing=()
while IFS= read -r file; do
  case "$file" in
    deploy/bootstrap/*) continue ;;   # vendored upstream manifests
    frontend/build/*|target/*) continue ;;
  esac
  if ! head -5 "$file" | grep -q "SPDX-License-Identifier: AGPL-3.0-only"; then
    missing+=("$file")
  fi
done < <(git ls-files '*.rs' '*.jsx' '*.js' '*.css' '*.sh')

if [ ${#missing[@]} -gt 0 ]; then
  echo "Missing the SPDX licence header (see CONTRIBUTING.md):" >&2
  printf '  %s\n' "${missing[@]}" >&2
  echo >&2
  echo "Add these two lines at the very top (/* */ form for .css):" >&2
  echo "  // Copyright (C) 2026 Tornis Desenvolvimento" >&2
  echo "  // SPDX-License-Identifier: AGPL-3.0-only" >&2
  exit 1
fi

echo "All $(git ls-files '*.rs' '*.jsx' '*.js' '*.css' '*.sh' | wc -l) source files carry the header."
