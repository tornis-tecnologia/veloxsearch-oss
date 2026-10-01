#!/usr/bin/env python3
# Copyright (C) 2026 Tornis Desenvolvimento
# SPDX-License-Identifier: AGPL-3.0-only
"""Settings -> Access refuses an ingress class the cluster lacks (issue #139).

On v0.12.0 the screen offered made-up classes when the cluster reported none,
saved `ingress` + `traefik` on a cluster with no Traefik, and every create
after that failed on its route. The backend now refuses such a save; this
check holds the screen to saying so before the user clicks.

`/api/access_settings` and `/api/save_access_settings` are answered by this
script in the browser (page.route), so it needs no cluster and is safe against
any backend — the same arrangement as profile_dialog_check.py.

Asserted:
  * no IngressClass on the cluster + ingress mode: the reason is shown under
    the class select and Save is disabled;
  * switching to port-forward re-enables Save (it is the way out);
  * a stored class the cluster lacks is shown, named in the reason, and blocks
    Save; picking an available class clears it, and Save sends that class.

Network gate as in create_submit_check.py, with the same no-cluster allowance.

Usage: access_settings_check.py <base_url> <user> <pw>
"""
import json
import re
import sys
from playwright.sync_api import sync_playwright

base, user, pw = sys.argv[1], sys.argv[2], sys.argv[3]

EXPECTED = [
    # No cluster: these reads answer 500 from the Kubernetes layer and the SPA
    # already degrades on each. A real cluster produces none of them.
    (r"^500 GET .*/api/(bootstrap_status|list_deployments|storage_status|retention_settings)$",
     "Kubernetes layer unreachable (no cluster attached)"),
    (r"Failed to load resource: the server responded with a status of 500",
     "console echo of the no-cluster 500s"),
]

# The v0.12.0 report: ingress + traefik stored, nothing installed.
NO_CLASSES = {"mode": "ingress", "base_domain": "example.test", "ingress_class": "traefik",
              "tls_secret": "", "available_classes": [], "default_base_domain": ""}
# A stored class the cluster does not have, next to one it does.
OTHER_CLASS = {**NO_CLASSES, "available_classes": ["nginx"]}

state = {"settings": NO_CLASSES}
saves = []
console, errors, faults, tolerated = [], [], [], []


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


def answer_save(route):
    saves.append(json.loads(route.request.post_data or "{}"))
    route.fulfill(status=200, body="")


def fail(msg):
    print("FAIL:", msg)
    sys.exit(1)


def open_settings(page):
    page.wait_for_selector("nav.tabs", timeout=600000)
    page.click("nav.tabs button:nth-child(4)")
    page.wait_for_selector('[data-testid="access-save"]', timeout=15000)


with sync_playwright() as p:
    b = p.chromium.launch()
    page = b.new_context(ignore_https_errors=True).new_page()
    page.on("console", lambda m: console.append((m.type, m.text[:200])))
    page.on("pageerror", lambda e: errors.append(str(e)[:300]))
    page.on("response", on_response)
    page.on("requestfailed", on_requestfailed)
    page.route("**/api/access_settings", lambda r: r.fulfill(
        status=200, content_type="application/json", body=json.dumps(state["settings"])))
    page.route("**/api/save_access_settings", answer_save)

    # 1. boot + authenticate, then Settings (fourth in nav.tabs).
    page.goto(base, wait_until="domcontentloaded")
    page.wait_for_selector('input[name="username"], nav.tabs', timeout=30000)
    if page.locator('input[name="username"]').count():
        is_setup = page.locator('input[name="confirm"]').count() > 0
        page.fill('input[name="username"]', user)
        page.fill('input[name="password"]', pw)
        if is_setup:
            page.fill('input[name="confirm"]', pw)
        page.click('button[type="submit"]')
    open_settings(page)
    save = page.locator('[data-testid="access-save"]')

    # 2. no IngressClass: reason shown, Save blocked.
    page.wait_for_selector('[data-testid="ingress-class"]', timeout=10000)
    page.wait_for_function(
        "() => document.querySelector('[data-testid=access-save]').disabled", timeout=10000)
    if "IngressClass" not in page.inner_text("body"):
        fail("no reason shown for a cluster without IngressClass")
    options = page.locator('[data-testid="ingress-class"] option').all_inner_texts()
    if options != ["traefik"]:
        fail(f"the select offers classes the cluster does not have: {options}")
    print("  no IngressClass -> reason shown, Save disabled, only the stored class listed")

    # 3. port-forward is the way out: Save re-enabled.
    page.locator("button.purpose").nth(0).click()
    if save.is_disabled():
        fail("Save stays disabled in port-forward mode")
    print("  port-forward -> Save enabled")

    # 4. stored class missing next to an available one.
    state["settings"] = OTHER_CLASS
    page.reload(wait_until="domcontentloaded")
    open_settings(page)
    page.wait_for_function(
        "() => document.querySelector('[data-testid=access-save]').disabled", timeout=10000)
    if '"traefik"' not in page.inner_text("body"):
        fail("the reason does not name the stored class")
    page.select_option('[data-testid="ingress-class"]', "nginx")
    if save.is_disabled():
        fail("Save stays disabled with an available class picked")
    save.click()
    page.wait_for_timeout(500)
    if len(saves) != 1 or saves[0].get("ingress_class") != "nginx":
        fail(f"expected one save of class nginx, got {saves}")
    print("  missing class -> named + blocked; nginx picked -> saved")

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
print("PASS — Settings -> Access only saves a class the cluster has")
