#!/usr/bin/env python3
"""Conformance-fleet journey test (ADR-026) — React SPA edition (ADR-032).

Drives a FRESH VeloxSearch install (first-run) through the browser:
  setup (admin creation) -> conformity screen (REQUIREMENTS.md report) ->
  either the cluster bootstraps to the main tabs (expect=pass) or it is
  refused with ✗ marks and no install (expect=reject).

The UI is a single-URL React SPA: there are NO /setup or / routes — the
screen is swapped by React state off `GET /api/auth_state`
(first_run -> setup). Drive it by the rendered widgets, never by paths.

Usage:
  firstrun_check.py <base> <user> <pw> pass   [shot.png]   # ct1-style cluster
  firstrun_check.py <base> <user> <pw> reject [shot.png]   # ct2-style cluster

Console must stay free of hydration/panic errors throughout (ADR-024:
curl can't see those). Assumes a genuinely fresh cluster — if the cluster
is already bootstrapped there is no conformity gate to assert.
"""
import sys
from playwright.sync_api import sync_playwright

base, user, pw, expect = sys.argv[1:5]
shot = sys.argv[5] if len(sys.argv) > 5 else None
errors, console = [], []

with sync_playwright() as p:
    b = p.chromium.launch()
    page = b.new_context(ignore_https_errors=True).new_page()
    page.on("console", lambda m: console.append(f"{m.type}: {m.text[:200]}"))
    page.on("pageerror", lambda e: errors.append(str(e)[:300]))

    # 1. first-run: the SPA probes /api/auth_state and mounts the setup screen
    #    (AuthView mode=setup) — identified by the confirm-password field, which
    #    only the setup form renders. networkidle never fires (SSE), so settle
    #    on the widget itself, not the network.
    page.goto(base, wait_until="domcontentloaded")
    page.wait_for_selector('input[name="confirm"]', timeout=20000)
    page.wait_for_timeout(800)  # let the Babel-compiled views settle

    # 2. create the admin account -> setup_admin auto-logins (sets the cookie) ->
    #    onAuthed re-probes -> the bootstrap/conformity gate.
    page.fill('input[name="username"]', user)
    page.fill('input[name="password"]', pw)
    page.fill('input[name="confirm"]', pw)
    page.click('button[type="submit"]')

    # 3. conformity screen: the REQUIREMENTS.md report renders as ul.req-list.
    page.wait_for_selector("ul.req-list", timeout=30000)
    page.wait_for_timeout(1500)
    report = page.inner_text("ul.req-list")
    print("requirements report:")
    for line in report.splitlines():
        print("   ", line)

    if expect == "reject":
        # Unsupported cluster: ✗ present, refusal shown, install NOT started
        # (BootstrapView omits ul.setup-list when status.unsupported).
        assert "✗" in report, f"expected at least one failing requirement:\n{report}"
        assert page.locator("ul.setup-list").count() == 0, "install checklist should be absent"
        if shot:
            page.screenshot(path=shot, full_page=True)
        print("verdict: refused informatively (as designed)")
    else:
        # Supported cluster: no hard failures; bootstrap runs to the main tabs.
        assert "✗" not in report, f"unexpected failing requirement:\n{report}"
        # cert-manager + operator install can take minutes on a fresh cluster.
        page.wait_for_selector("nav.tabs", timeout=600000)
        page.wait_for_timeout(1500)
        body = page.inner_text("body")
        assert "Status" in body, f"tabs missing after bootstrap: {body[:200]}"
        if shot:
            page.screenshot(path=shot, full_page=True)
        print("verdict: bootstrapped to main tabs")

    b.close()

hyd = [c for c in console if "hydrat" in c.lower() or "panic" in c.lower()]
if hyd or errors:
    print("FAIL — console/page errors:")
    for x in hyd + errors:
        print("  ", x)
    sys.exit(1)
print(f"PASS ({expect}) — no hydration/panic errors, {len(console)} console msgs")
