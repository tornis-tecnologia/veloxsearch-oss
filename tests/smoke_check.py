#!/usr/bin/env python3
"""Install-and-smoke check for the minikube CI lane (#51).

The browser journey (firstrun_check.py) drives the FULL bootstrap — admin
creation, the conformity gate, then cert-manager + the OpenSearch operator
self-install — which needs a cluster that clears the R4 floor (>=8Gi
allocatable, REQUIREMENTS.md). That is a Proxmox-fleet conformance concern
(conformance/, ct1/ct2/ct4), not something an in-CI minikube can satisfy.

This script is the deliberately lightweight *smoke subset* the minikube lane
runs instead: it proves `kubectl apply -f deploy/install.yaml` brings the app
up and that a freshly-installed VeloxSearch boots and correctly reports
first-run — WITHOUT triggering the heavy operator bootstrap. Stdlib only (no
Playwright/Chromium), so the CI job needs nothing but python3.

Checks, against a port-forward to the Service:
  1. GET /login            -> 200            (the SPA is served / pod is up)
  2. GET /api/auth_state   -> 200 + JSON {"first_run": true, ...}
                                            (fresh install detected correctly)

Usage:
  smoke_check.py <base>            # e.g. http://127.0.0.1:3000
  smoke_check.py <base> --retries 30 --delay 5
"""
import json
import sys
import time
import urllib.error
import urllib.request

base = sys.argv[1].rstrip("/")
retries = 30
delay = 5.0
for i, a in enumerate(sys.argv):
    if a == "--retries" and i + 1 < len(sys.argv):
        retries = int(sys.argv[i + 1])
    if a == "--delay" and i + 1 < len(sys.argv):
        delay = float(sys.argv[i + 1])


def _get(path):
    """Return (status, body) for GET base+path; raise on transport failure."""
    req = urllib.request.Request(base + path, headers={"Accept": "*/*"})
    with urllib.request.urlopen(req, timeout=10) as r:
        return r.status, r.read().decode("utf-8", "replace")


def _wait_up():
    """Poll /login until the pod serves 200 (rollout + readiness settle)."""
    last = None
    for n in range(retries):
        try:
            status, _ = _get("/login")
            if status == 200:
                print(f"app up: GET /login -> 200 (after {n} polls)")
                return
            last = f"status {status}"
        except (urllib.error.URLError, OSError) as e:
            last = str(e)
        time.sleep(delay)
    print(f"FAIL — /login never returned 200 ({retries} polls): {last}")
    sys.exit(1)


_wait_up()

# auth_state must be a fresh first-run install (no admin account yet).
status, body = _get("/api/auth_state")
if status != 200:
    print(f"FAIL — GET /api/auth_state -> {status}, expected 200")
    sys.exit(1)
try:
    state = json.loads(body)
except json.JSONDecodeError:
    print(f"FAIL — /api/auth_state is not JSON: {body[:200]}")
    sys.exit(1)

if state.get("first_run") is not True:
    print(f"FAIL — expected first_run=true on a fresh install, got: {state}")
    sys.exit(1)
if state.get("authenticated") is not False:
    print(f"FAIL — expected authenticated=false pre-setup, got: {state}")
    sys.exit(1)

print(f"auth_state OK: {state}")
print("PASS (smoke) — install.yaml applied, app boots, reports first-run")
