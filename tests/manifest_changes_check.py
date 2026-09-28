#!/usr/bin/env python3
# Copyright (C) 2026 Tornis Desenvolvimento
# SPDX-License-Identifier: AGPL-3.0-only
"""deploy/manifest-changes.sh against the fixtures in tests/fixtures/manifest-changes/.

Every case compares the script's whole output, so a change in wording or order
is a test change too: the output is pasted into release notes and PR summaries,
and it must be the same bytes for the same pair of manifests.

    python3 tests/manifest_changes_check.py

stdlib only, no cluster. CI runs it in the `manifest-version` job.
"""
import pathlib
import subprocess
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "deploy" / "manifest-changes.sh"
FIX = ROOT / "tests" / "fixtures" / "manifest-changes"

CASES = {
    # Tag -> digest, plus comment/quoting/order noise: nothing to apply.
    ("base", "none"): """\
classification: none
only the veloxsearch image changed: `kubectl set image` is enough
""",
    ("base", "rbac"): """\
classification: rbac
rbac:
  - ClusterRole/veloxsearch-runtime + apiGroups=[apps] resources=[deployments] verbs=[get,list,patch]
  - ClusterRole/veloxsearch-runtime - apiGroups=[apps] resources=[deployments] verbs=[get,list]
  - RoleBinding/veloxsearch-system/veloxsearch-runtime + subject ServiceAccount velox-agents/velox-agent
""",
    ("base", "config"): """\
classification: config
config:
  - ConfigMap/veloxsearch-system/veloxsearch-env data: +VELOX_PG_ENABLED ~VELOX_COOKIE_SECURE
  - Deployment/veloxsearch-system/veloxsearch container veloxsearch env: +POD_NAME
""",
    # A different image REPOSITORY is not an image-only upgrade.
    ("base", "other"): """\
classification: other
other:
  - Deployment/veloxsearch-system/veloxsearch: spec.template.spec.containers[veloxsearch].image, spec.template.spec.containers[veloxsearch].resources.limits.memory
  - + Ingress/veloxsearch-system/veloxsearch (added)
  - Service/veloxsearch-system/veloxsearch: spec.ports[0].nodePort
""",
    ("other", "base"): """\
classification: other
other:
  - Deployment/veloxsearch-system/veloxsearch: spec.template.spec.containers[veloxsearch].image, spec.template.spec.containers[veloxsearch].resources.limits.memory
  - - Ingress/veloxsearch-system/veloxsearch (removed)
  - Service/veloxsearch-system/veloxsearch: spec.ports[0].nodePort
""",
    # Several classes at once, in the fixed order rbac, config, other.
    ("rbac", "config"): """\
classification: rbac, config
rbac:
  - ClusterRole/veloxsearch-runtime + apiGroups=[apps] resources=[deployments] verbs=[get,list]
  - ClusterRole/veloxsearch-runtime - apiGroups=[apps] resources=[deployments] verbs=[get,list,patch]
  - RoleBinding/veloxsearch-system/veloxsearch-runtime - subject ServiceAccount velox-agents/velox-agent
config:
  - ConfigMap/veloxsearch-system/veloxsearch-env data: +VELOX_PG_ENABLED ~VELOX_COOKIE_SECURE
  - Deployment/veloxsearch-system/veloxsearch container veloxsearch env: +POD_NAME
""",
}


def run(old, new):
    proc = subprocess.run([str(SCRIPT), str(old), str(new)], capture_output=True, text=True)
    if proc.returncode != 0:
        raise AssertionError(f"exit {proc.returncode} for {old.name} -> {new.name}: {proc.stderr}")
    return proc.stdout


def main():
    failures = 0
    for (old, new), expected in CASES.items():
        a, b = FIX / f"{old}.yaml", FIX / f"{new}.yaml"
        got = run(a, b)
        if got != expected or run(a, b) != got:
            failures += 1
            print(f"FAIL {old} -> {new}\n--- expected\n{expected}--- got\n{got}")
        else:
            print(f"ok   {old} -> {new}: {got.splitlines()[0]}")

    # A document outside the parsed subset is reported, never a crash or a
    # silent `none`: the release must still publish, and must not overclaim.
    with tempfile.TemporaryDirectory() as tmp:
        bad = pathlib.Path(tmp) / "anchor.yaml"
        bad.write_text((FIX / "base.yaml").read_text().replace("replicas: 1", "replicas: &r 1"))
        got = run(FIX / "base.yaml", bad)
        if not got.startswith("classification: other\n") or "could not parse" not in got:
            failures += 1
            print(f"FAIL unparseable document\n{got}")
        else:
            print("ok   unparseable document: classification: other")

    if failures:
        print(f"{failures} case(s) failed")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
