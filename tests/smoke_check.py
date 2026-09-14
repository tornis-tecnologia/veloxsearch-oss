#!/usr/bin/env python3
"""Install-and-smoke check for the minikube CI lane (#51).

The browser journey (firstrun_check.py) drives the FULL bootstrap — admin
creation, the conformity gate, then cert-manager + the OpenSearch operator
self-install — which needs a cluster that clears the R4 floor (>=8Gi
allocatable, REQUIREMENTS.md). That is a conformance-fleet concern
(the conformance fleet: k3s-greenfield, k0s-bare, …), not something an in-CI minikube can satisfy.

This script is the deliberately lightweight *smoke subset* the minikube lane
runs instead: it proves `kubectl apply -f deploy/install.yaml` brings the app
up and that a freshly-installed VeloxSearch boots and correctly reports
first-run — WITHOUT triggering the heavy operator bootstrap. Stdlib only (no
Playwright/Chromium), so the CI job needs nothing but python3.

Checks, against a port-forward to the Service:
  1. GET /login            -> 200            (the SPA is served / pod is up)
  2. GET /api/auth_state   -> 200 + JSON {"first_run": true, ...}
                                            (fresh install detected correctly)
  3. POST /api/setup_admin, then GET /api/build_info (admin-only)
                           -> version == Cargo.toml `version`       (#55)
     Creating the admin only writes the credentials Secret; it does not start
     the operator bootstrap, so the subset stays light.

Usage:
  smoke_check.py <base>            # e.g. http://127.0.0.1:3000
  smoke_check.py <base> --retries 30 --delay 5
"""
import json
import pathlib
import re
import secrets
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


def _get(path, cookie=None):
    """Return (status, body) for GET base+path; raise on transport failure.

    A 4xx/5xx is returned as a status, not raised, so callers can say which
    check failed and why.
    """
    headers = {"Accept": "*/*"}
    if cookie:
        headers["Cookie"] = cookie
    req = urllib.request.Request(base + path, headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=10) as r:
            return r.status, r.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode("utf-8", "replace")


def _post_json(path, payload):
    """Return (status, body, Set-Cookie) for a JSON POST."""
    req = urllib.request.Request(
        base + path,
        data=json.dumps(payload).encode(),
        headers={"Content-Type": "application/json", "Accept": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            return r.status, r.read().decode("utf-8", "replace"), r.headers.get("Set-Cookie")
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode("utf-8", "replace"), None


def _cargo_version():
    """The crate version, read the same way release.yml's gate reads it."""
    toml = pathlib.Path(__file__).resolve().parent.parent / "Cargo.toml"
    for line in toml.read_text().splitlines():
        m = re.match(r'^version = "(.*)"', line)
        if m:
            return m.group(1)
    print(f"FAIL — no version line in {toml}")
    sys.exit(1)


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

# build_info (#55): the running build must report the version this tree
# declares. It is admin-only, so create the first admin (a throwaway password:
# the minikube cluster dies with the job) and use the session it hands back.
expected = _cargo_version()
password = secrets.token_urlsafe(18)
status, body, set_cookie = _post_json(
    "/api/setup_admin", {"username": "smoke", "password": password, "confirm": password}
)
if status != 200 or not set_cookie:
    print(f"FAIL — POST /api/setup_admin -> {status} (cookie: {bool(set_cookie)}): {body[:200]}")
    sys.exit(1)
cookie = set_cookie.split(";", 1)[0]

status, body = _get("/api/build_info", cookie=cookie)
# The ONE tolerated miss, named precisely: the 0.9.0 release image was built
# before this endpoint existed. There an unknown /api path is answered by the
# SPA fallback (200 + index.html), or a 404. Every later version ships the
# route, so from the next version bump either shape fails like any other.
endpoint_absent = status == 404 or (status == 200 and body.lstrip().lower().startswith("<!doctype html"))
if endpoint_absent and expected == "0.9.0":
    print("SKIP build_info — the released 0.9.0 image predates /api/build_info (#55); "
          "this check is enforced from the next release")
    print("PASS (smoke) — install.yaml applied, app boots, reports first-run")
    sys.exit(0)
if status != 200:
    print(f"FAIL — GET /api/build_info -> {status}, expected 200: {body[:200]}")
    sys.exit(1)
try:
    info = json.loads(body)
except json.JSONDecodeError:
    print(f"FAIL — /api/build_info is not JSON: {body[:200]}")
    sys.exit(1)
if info.get("version") != expected:
    print(f"FAIL — build_info.version {info.get('version')!r} != Cargo.toml version {expected!r}")
    sys.exit(1)
print(f"build_info OK: version={info['version']} commit={info.get('commit')} "
      f"image_digest={info.get('image_digest')}")
print("PASS (smoke) — install.yaml applied, app boots, reports first-run, "
      "build_info matches Cargo.toml")
