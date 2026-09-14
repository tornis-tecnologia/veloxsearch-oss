#!/usr/bin/env python3
# Copyright (C) 2026 Tornis Desenvolvimento
# SPDX-License-Identifier: AGPL-3.0-only
"""Create-wizard double-submit gate (issue #56).

Clicking "Create cluster" twice used to send two creates — and since every
create generates a fresh `<name>-<suffix>`, that meant two deployments. The
button only disabled itself when the create was going to install Longhorn.

This drives the wizard to review and DOUBLE-CLICKS the submit button while the
`create_cluster` request is HELD in the browser (page.route): the request never
reaches the backend until the script answers it. That holding is the slow server
the double-click used to race, made deterministic, and it is also why this check
is safe against ANY backend, cluster or not — it never provisions anything.

Asserted:
  * exactly ONE create_cluster request leaves the browser for a double click;
  * the button is disabled, aria-busy and labelled "Submitting…" while held;
  * answered with the backend's in-flight refusal (409, #56) the wizard
    re-arms: button enabled again, create label back, the reason toasted;
  * a later single click sends exactly one more request.

Network gate as in browser_check.py: console errors, pageerrors, failed
requests and >=400 responses fail the run — except the entries listed in
EXPECTED, each with its reason, which a backend with no cluster attached
(the local loop in docs/DEVELOPMENT.md) produces by design.

Usage: create_submit_check.py <base_url> <user> <pw>
"""
import json
import re
import sys
from playwright.sync_api import sync_playwright

base, user, pw = sys.argv[1], sys.argv[2], sys.argv[3]

# (pattern over "<status> <method> <url>" or the console text, why it is fine)
EXPECTED = [
    # The 409 this script fulfils itself — the response under test.
    (r"^409 POST .*/api/create_cluster", "the simulated in-flight refusal"),
    # Chromium logs every >=400 fetch as a console error; this is the same 409.
    (r"Failed to load resource: the server responded with a status of 409",
     "console echo of the simulated 409"),
    # No cluster: these reads answer 500 from the Kubernetes layer and the SPA
    # already degrades on each (no gate, empty list, no capacity, no storage
    # heads-up). A real cluster produces none of them.
    (r"^500 GET .*/api/(bootstrap_status|list_deployments|cluster_capacity|storage_status)$",
     "Kubernetes layer unreachable (no cluster attached)"),
    (r"Failed to load resource: the server responded with a status of 500",
     "console echo of the no-cluster 500s"),
]

console, errors, requests, faults = [], [], [], []


tolerated = []  # every EXPECTED match, printed so the allowance stays visible


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
    if "ERR_ABORTED" in f:  # SSE / asset teardown on close, as in browser_check.py
        return
    faults.append(f"requestfailed {req.method} {req.url[:160]} — {f}")


held = []  # create_cluster routes waiting for this script to answer them


def fail(msg):
    print("FAIL:", msg)
    for route in held:  # never leave a held request dangling on exit
        try:
            route.abort()
        except Exception:
            pass
    sys.exit(1)


with sync_playwright() as p:
    b = p.chromium.launch()
    page = b.new_context(ignore_https_errors=True).new_page()
    page.on("console", lambda m: console.append((m.type, m.text[:200])))
    page.on("pageerror", lambda e: errors.append(str(e)[:300]))
    page.on("request", lambda r: requests.append(r.url))
    page.on("response", on_response)
    page.on("requestfailed", on_requestfailed)
    page.route("**/api/create_cluster", lambda route: held.append(route))

    # 1. boot + authenticate (first-run setup or login), then home.
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

    # 2. wizard to review: name -> size (pick a preset) -> backup (skip) -> review.
    page.click("nav.tabs button:nth-child(2)")
    page.wait_for_selector(".stepper", timeout=10000)
    page.fill('input[name="name"]', "doubleclick")
    page.locator('[data-testid="wizard-next"]').click()
    page.wait_for_timeout(300)
    if page.locator('[data-testid="wizard-next"]').is_disabled():
        page.locator('button.purpose:not([aria-pressed="true"])').nth(0).click()
        page.wait_for_timeout(300)
    page.locator('[data-testid="wizard-next"]').click()
    page.wait_for_timeout(300)
    page.locator('[data-testid="wizard-next"]').click()
    page.wait_for_selector(".kvrow", timeout=10000)

    submit = page.locator('[data-testid="create-submit"]')
    if submit.is_disabled():
        fail("create-submit is disabled on review before any click — "
             "check the wizard's validity/missing-packages state")
    idle_label = submit.inner_text().strip()

    # 3. the double click. `force` on the second click: a DISABLED button is
    #    exactly what we want it to hit — Playwright would otherwise wait for it
    #    to become enabled and the test would assert nothing.
    submit.click()
    submit.click(force=True, no_wait_after=True)
    page.wait_for_timeout(500)  # let any second request actually leave

    if len(held) != 1:
        fail(f"a double click sent {len(held)} create_cluster requests, expected 1")
    if not submit.is_disabled():
        fail("create-submit is still enabled while the create is in flight")
    if submit.get_attribute("aria-busy") != "true":
        fail("create-submit is not aria-busy while the create is in flight")
    busy_label = submit.inner_text().strip()
    if busy_label == idle_label or page.locator("[data-testid=create-submit] .btn-spinner").count() != 1:
        fail(f"no in-button acknowledgement: label {busy_label!r}, spinner missing or duplicated")
    body = json.loads(held[0].request.post_data or "{}")
    if body.get("name") != "doubleclick":
        fail(f"held request carries the wrong payload: {body}")
    print(f"  double click -> 1 request; button disabled + busy ({busy_label!r})")

    # 4. answer with the backend's in-flight refusal (#56): the wizard re-arms.
    refusal = "a deployment named 'doubleclick' is already being created — wait for it to finish"
    held.pop().fulfill(status=409, content_type="application/json",
                       body=json.dumps({"error": refusal}))
    page.wait_for_function(
        "() => { const b = document.querySelector('[data-testid=create-submit]');"
        " return b && !b.disabled; }", timeout=10000)
    if submit.get_attribute("aria-busy") == "true":
        fail("create-submit stayed aria-busy after the create failed")
    if submit.inner_text().strip() != idle_label:
        fail(f"label did not return to {idle_label!r}: {submit.inner_text()!r}")
    if refusal not in page.inner_text("body"):
        fail("the refusal reason was not shown to the user")
    print("  refusal -> button re-armed, reason toasted")

    # 5. re-armed means usable: one more click, one more request.
    submit.click()
    page.wait_for_timeout(500)
    if len(held) != 1:
        fail(f"a single click after re-arm sent {len(held)} requests, expected 1")
    held.pop().fulfill(status=409, content_type="application/json",
                       body=json.dumps({"error": refusal}))
    page.wait_for_function(
        "() => !document.querySelector('[data-testid=create-submit]').disabled", timeout=10000)
    print("  re-armed button sends exactly one request")

    b.close()

# ── verdict ──────────────────────────────────────────────────────────────
for ty, tx in console:
    if (ty == "error" or "panic" in tx.lower() or "hydrat" in tx.lower()) and not expected(tx):
        faults.append(f"console {ty}: {tx}")
faults += [f"pageerror: {e}" for e in errors]

print(f"{len(console)} console / {len(requests)} requests / {len(faults)} errors")
print("  tolerated by design:")
for pattern, why in EXPECTED:
    print(f"    {pattern}  — {why}")
for line in tolerated:
    print(f"    seen: {line}")
if faults:
    print("FAIL — captured faults:")
    for x in faults:
        print("  ", x)
    sys.exit(1)
print("PASS — double-submit guarded; one request per intent; re-arms on failure")
