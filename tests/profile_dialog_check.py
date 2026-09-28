#!/usr/bin/env python3
# Copyright (C) 2026 Tornis Desenvolvimento
# SPDX-License-Identifier: AGPL-3.0-only
"""Cluster profile download dialog (ADR-060 decision 9).

The Capacity view's "Download cluster profile" fetches the profile ONCE per
names setting, previews the exact JSON, and saves THOSE bytes from a
client-side Blob — no second request, so the file cannot differ from the
preview. This check holds the app to that.

`/api/cluster_profile` and `/api/cluster_capacity` are answered by this script
in the browser (page.route), so it needs no cluster and is safe against any
backend — the same arrangement as create_submit_check.py.

Asserted:
  * opening the dialog sends exactly one cluster_profile request, names off;
  * the preview is the profile, pretty-printed;
  * the names toggle refetches exactly once, with `names=true`;
  * Save downloads a file whose bytes equal the preview's text, and sends no
    further request;
  * Close removes the dialog.

Network gate as in create_submit_check.py, with the same no-cluster allowance.

Usage: profile_dialog_check.py <base_url> <user> <pw>
"""
import json
import re
import sys
from playwright.sync_api import sync_playwright

base, user, pw = sys.argv[1], sys.argv[2], sys.argv[3]

EXPECTED = [
    # No cluster: these reads answer 500 from the Kubernetes layer and the SPA
    # already degrades on each. A real cluster produces none of them.
    (r"^500 GET .*/api/(bootstrap_status|list_deployments|storage_status)$",
     "Kubernetes layer unreachable (no cluster attached)"),
    (r"Failed to load resource: the server responded with a status of 500",
     "console echo of the no-cluster 500s"),
]

CAPACITY = {
    "nodes": [{
        "name": "node-a", "roles": ["control-plane"], "ready": True, "pressures": [],
        "kernel_version": "6.1.0-40-amd64",
        "cpu": {"total": 4000, "used": 1200, "requested": 2600},
        "mem": {"total": 16 << 30, "used": 9 << 30, "requested": 11 << 30},
        "host_disk": {"total": 100 << 30, "used": 36 << 30, "requested": None},
        "storage": None,
    }],
    "cpu": {"total": 4000, "used": 1200, "requested": 2600},
    "mem": {"total": 16 << 30, "used": 9 << 30, "requested": 11 << 30},
    "storage": None,
    "fit": [{"size": "small", "count": 2, "limited_by": "mem"}],
    "metrics_available": True,
}


def profile(names):
    d = {"id": "d-0123456789ab", "tenant": None, "size": "small", "docs": 12004311,
         "now": {"cpu_percent": 21.5, "heap_percent": 61.0}}
    if names:
        d = {"id": d["id"], "name": "logs", **{k: v for k, v in d.items() if k != "id"}}
    return {"schema_version": 1, "generated_at": "2026-09-14T12:00:00Z",
            "scope": "installation", "names_included": names, "deployments": [d]}


console, errors, faults, tolerated = [], [], [], []
profile_requests = []


def expected(line):
    if any(re.search(p, line) for p, _ in EXPECTED):
        tolerated.append(line)
        return True
    return False


def on_response(resp):
    if resp.status >= 400 and ("/api/" in resp.url or resp.url.startswith(base)):
        line = f"{resp.status} {resp.request.method} {resp.url[:160]}"
        if not expected(line):
            faults.append(line)


def on_requestfailed(req):
    f = req.failure or ""
    if "ERR_ABORTED" in f:
        return
    faults.append(f"requestfailed {req.method} {req.url[:160]} — {f}")


def answer_profile(route):
    url = route.request.url
    profile_requests.append(url)
    route.fulfill(status=200, content_type="application/json",
                  body=json.dumps(profile("names=true" in url)))


def fail(msg):
    print("FAIL:", msg)
    sys.exit(1)


with sync_playwright() as p:
    b = p.chromium.launch()
    ctx = b.new_context(ignore_https_errors=True, accept_downloads=True)
    page = ctx.new_page()
    page.on("console", lambda m: console.append((m.type, m.text[:200])))
    page.on("pageerror", lambda e: errors.append(str(e)[:300]))
    page.on("response", on_response)
    page.on("requestfailed", on_requestfailed)
    page.route("**/api/cluster_capacity", lambda r: r.fulfill(
        status=200, content_type="application/json", body=json.dumps(CAPACITY)))
    page.route(re.compile(r".*/api/cluster_profile(\?.*)?$"), answer_profile)

    # 1. boot + authenticate, then the Capacity tab (third in nav.tabs).
    page.goto(base, wait_until="domcontentloaded")
    page.wait_for_selector('input[name="username"], nav.tabs', timeout=30000)
    if page.locator('input[name="username"]').count():
        is_setup = page.locator('input[name="confirm"]').count() > 0
        page.fill('input[name="username"]', user)
        page.fill('input[name="password"]', pw)
        if is_setup:
            page.fill('input[name="confirm"]', pw)
        page.click('button[type="submit"]')
    page.wait_for_selector("nav.tabs", timeout=600000)
    page.click("nav.tabs button:nth-child(3)")
    page.wait_for_selector('[data-testid="profile-open"]', timeout=15000)

    # 2. open: one request, names off; the preview is the profile.
    page.click('[data-testid="profile-open"]')
    page.wait_for_selector('[data-testid="profile-preview"][aria-busy="false"]', timeout=10000)
    if len(profile_requests) != 1 or "names=" in profile_requests[0]:
        fail(f"opening sent {profile_requests}, expected one request without names")
    preview = page.locator('[data-testid="profile-preview"]').text_content()
    if json.loads(preview) != profile(False):
        fail(f"preview is not the profile: {preview[:200]}")
    if '\n  "schema_version": 1,' not in preview:
        fail("preview is not pretty-printed")
    print("  open -> 1 request (names off), preview = profile")

    # 3. names toggle: exactly one refetch, with names=true.
    page.check('[data-testid="profile-names"]')
    page.wait_for_function(
        "() => (document.querySelector('[data-testid=profile-preview]')?.textContent || '')"
        ".includes('\"names_included\": true')", timeout=10000)
    if len(profile_requests) != 2 or "names=true" not in profile_requests[1]:
        fail(f"toggling names sent {profile_requests[1:]}, expected one names=true request")
    preview = page.locator('[data-testid="profile-preview"]').text_content()
    print("  toggle -> 1 request (names=true)")

    # 4. save: the file is the preview, byte for byte, and nothing is fetched.
    with page.expect_download() as dl:
        page.click('[data-testid="profile-save"]')
    path = dl.value.path()
    saved = open(path, "rb").read()
    if saved != preview.encode("utf-8"):
        fail("saved file differs from the previewed text")
    if not dl.value.suggested_filename.startswith("cluster-profile-"):
        fail(f"unexpected file name {dl.value.suggested_filename!r}")
    page.wait_for_timeout(300)
    if len(profile_requests) != 2:
        fail(f"saving sent another request: {profile_requests[2:]}")
    print(f"  save -> {dl.value.suggested_filename}, {len(saved)} bytes = preview, no request")

    # 5. close.
    page.click('[data-testid="profile-close"]')
    page.wait_for_selector('[data-testid="profile-dialog"]', state="detached", timeout=5000)
    print("  close -> dialog gone")

    b.close()

for ty, tx in console:
    if (ty == "error" or "panic" in tx.lower() or "hydrat" in tx.lower()) and not expected(tx):
        faults.append(f"console {ty}: {tx}")
faults += [f"pageerror: {e}" for e in errors]

print(f"{len(console)} console / {len(faults)} errors")
for line in tolerated:
    print(f"    tolerated: {line}")
if faults:
    print("FAIL — captured faults:")
    for x in faults:
        print("  ", x)
    sys.exit(1)
print("PASS — one request per names setting; the saved file is the preview")
