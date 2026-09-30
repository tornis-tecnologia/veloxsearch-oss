#!/usr/bin/env python3
# Copyright (C) 2026 Tornis Desenvolvimento
# SPDX-License-Identifier: AGPL-3.0-only
"""Frontend robustness (#141): copy buttons, error boundaries, tooltips.

Every /api/* call is answered by this script in the browser (page.route), so
it needs no cluster and no backend: point it at the real app, or at nothing
more than a static server over `frontend/build`:

    (cd frontend && npm ci && npm run build)
    python3 -m http.server -d frontend/build 8099 &
    python3 tests/frontend_robustness_check.py http://127.0.0.1:8099

Asserted:
  * copy with the Clipboard API denied: no page error; the legacy fallback
    copies the text and the UI says "copied" only because it did;
  * copy with the Clipboard API denied AND the fallback failing: no page
    error, the UI says it failed and leaves the text selected for Ctrl+C;
  * copy on a context without the Clipboard API (plain-HTTP origin): the
    fallback copies and the UI says so; with the fallback failing it says not;
  * the once-shown generated password's dialog closes on its copy button only
    when the copy happened; on a failure it stays open with the value;
  * an auth-provider test answer without `checks` renders the result, not a
    blank page;
  * a render error inside a view shows the view boundary's fallback while the
    header and navigation keep working, and leaving the view recovers;
  * a render error in the app shell shows the top-level fallback with a reload;
  * the theme, language and copy tooltips are translated in pt, en and es.

Gate: no page error and no console error, except React's own report of the
two render errors this script causes on purpose.

Usage: frontend_robustness_check.py <base_url> [evidence_dir]
"""
import json
import os
import re
import sys
from playwright.sync_api import sync_playwright

base = sys.argv[1].rstrip("/")
shots = sys.argv[2] if len(sys.argv) > 2 else None

DEPLOYMENT = {
    "name": "logs", "size": "small", "purpose": "observability", "health": "green",
    "phase": "Running", "replicas": 3, "nodes_ready": 3, "nodes_desired": 3,
    "memory": "2Gi", "disk": "10Gi", "heap": "1g", "version": "3.2.0",
    "dashboard_url": "https://logs.example.test",
    "opensearch_url": "https://logs-api.example.test",
    "provisioning": "complete",
}

AUTH_PROVIDER = {
    "spec": {"kind": "ldap", "hosts": ["ldap.example.test:389"], "role_mappings": []},
    "kinds_available": ["internal", "ldap", "oidc", "saml", "jwt", "proxy"],
    "builtin_roles": ["all_access", "readall"],
    "public_url": None, "redirect_blocked_reason": None,
    "break_glass_user": "admin", "secret_kept": "__velox_secret_kept__",
}

# Answers per endpoint; a scenario overrides what it needs. Anything absent
# answers 404, which every view already degrades on.
DEFAULTS = {
    "auth_state": {"first_run": False, "authenticated": True, "tenant": False},
    "bootstrap_status": {"ready": True},
    "list_deployments": [DEPLOYMENT],
    "build_info": {"version": "0.12.0", "commit": "0123456789abcdef", "image_digest": None},
    "auth_provider": AUTH_PROVIDER,
    # The #141 repro: an answer without `checks`.
    "test_auth_provider": {"ok": False, "error": "ldap.example.test:389: connection refused"},
    "reset_admin_password_random": {"username": "admin", "password": "n3w-Gener4ted-pw"},
}

TOOLTIPS = {  # lang -> (theme toggle while dark, language button)
    "pt": ("Tema claro", "Idioma"),
    "en": ("Light theme", "Language"),
    "es": ("Tema claro", "Idioma"),
}
COPY_TIP = {"pt": "copiar", "en": "copy", "es": "copiar"}

# Hide the Clipboard API the way a plain-HTTP origin does, keeping a handle so
# the check can still read what landed on the clipboard.
NO_CLIPBOARD = """
window.__clip = navigator.clipboard;
Object.defineProperty(Navigator.prototype, 'clipboard', { get: () => undefined });
"""
DENIED_CLIPBOARD = """
window.__clip = navigator.clipboard;
const denied = { writeText: () => Promise.reject(new DOMException('Write permission denied.', 'NotAllowedError')) };
Object.defineProperty(Navigator.prototype, 'clipboard', { get: () => denied });
"""
NO_EXECCOMMAND = "document.execCommand = () => false;"

faults = []
tolerated = []


def shot(page, name):
    if shots:
        os.makedirs(shots, exist_ok=True)
        page.screenshot(path=os.path.join(shots, f"{name}.png"), full_page=True)


def fail(msg):
    print("FAIL:", msg)
    sys.exit(1)


def open_page(browser, *, lang="en", answers=None, init=(), render_error=None):
    """A fresh context with the API stubbed, logged in, on the Status view.

    `render_error` is a regex for the one render error the scenario causes on
    purpose; React reports it on the console even when a boundary catches it.
    """
    ctx = browser.new_context()
    ctx.grant_permissions(["clipboard-read", "clipboard-write"], origin=base)
    ctx.add_init_script(f"try {{ localStorage.setItem('velox-lang', {json.dumps(lang)}); }} catch (e) {{}}")
    for s in init:
        ctx.add_init_script(s)
    page = ctx.new_page()
    stubs = {**DEFAULTS, **(answers or {})}

    def api(route):
        path = route.request.url.split("/api/", 1)[1].split("?", 1)[0]
        if path == "events":
            body = "data: " + json.dumps(stubs["list_deployments"]) + "\n\n"
            return route.fulfill(status=200, content_type="text/event-stream", body=body)
        if path in stubs:
            return route.fulfill(status=200, content_type="application/json", body=json.dumps(stubs[path]))
        route.fulfill(status=404, content_type="application/json", body='{"error":"not stubbed"}')

    page.route(re.compile(r".*/api/.*"), api)

    def on_console(m):
        if m.type != "error":
            return
        text = m.text[:300]
        # 404s are the unstubbed endpoints above, by design.
        if "status of 404" in text:
            tolerated.append("404 from an unstubbed endpoint")
        elif render_error and re.search(render_error, text):
            tolerated.append(text.splitlines()[0])
        else:
            faults.append(f"[{lang}] console error: {text}")

    page.on("console", on_console)
    page.on("pageerror", lambda e: faults.append(f"[{lang}] pageerror: {str(e)[:300]}"))
    page.goto(base + "/", wait_until="domcontentloaded")
    return ctx, page


def open_deployment(page, tab_label=None):
    page.wait_for_selector("nav.tabs", timeout=20000)
    page.locator(".card-actions button").first.click()
    page.wait_for_selector(".copyfield", timeout=15000)
    if tab_label:
        page.locator("button[aria-selected]", has_text=tab_label).first.click()


def check_copy(browser, name, init, expect_ok):
    ctx, page = open_page(browser, init=init)
    open_deployment(page)
    field = page.locator(".copyfield", has_text="logs.example.test").first
    field.wait_for(timeout=15000)
    text = DEPLOYMENT["dashboard_url"]
    field.locator("button").click()
    page.wait_for_selector(f'.copyfield[data-copy-state="{"copied" if expect_ok else "failed"}"]', timeout=5000)
    toast = page.locator(".toast.show").inner_text(timeout=5000)
    shot(page, f"copy-{name}")
    if expect_ok:
        pasted = page.evaluate("() => window.__clip.readText()")
        if pasted != text:
            fail(f"{name}: UI says copied but the clipboard holds {pasted!r}")
        if "copied" not in toast:
            fail(f"{name}: toast {toast!r}, expected 'copied'")
    else:
        if "copied" in toast or "Ctrl+C" not in toast:
            fail(f"{name}: toast {toast!r}, expected the failure message")
        if not page.locator(".toast.show svg.bad").count():
            fail(f"{name}: the failure toast is not marked as a failure")
        selected = page.evaluate("() => window.getSelection().toString()")
        if selected != text:
            fail(f"{name}: after a failed copy the selection is {selected!r}, expected the text")
    print(f"  copy [{name}] -> {'copied (verified on the clipboard)' if expect_ok else 'failure shown, text selected'}; toast {toast!r}")
    ctx.close()


with sync_playwright() as p:
    browser = p.chromium.launch()

    # 1. copy: denied / missing Clipboard API, with and without the fallback.
    check_copy(browser, "denied-fallback-ok", [DENIED_CLIPBOARD], True)
    check_copy(browser, "denied-fallback-fails", [DENIED_CLIPBOARD, NO_EXECCOMMAND], False)
    check_copy(browser, "no-api-fallback-ok", [NO_CLIPBOARD], True)
    check_copy(browser, "no-api-fallback-fails", [NO_CLIPBOARD, NO_EXECCOMMAND], False)

    # 1b. the generated password is shown once: a failed copy must not close it.
    for name, init, expect_open in (("fails", [DENIED_CLIPBOARD, NO_EXECCOMMAND], True),
                                    ("works", [DENIED_CLIPBOARD], False)):
        ctx, page = open_page(browser, init=init)
        open_deployment(page, "Security")
        page.click('[data-testid="reset-pass"]')
        modal = page.locator('div[style*="z-index: 300"]')  # ui.jsx Confirm
        modal.locator("button.btn-danger").click()  # confirm the reset
        dialog_copy = modal.locator("button.btn-primary", has_text="copy")
        dialog_copy.wait_for(timeout=10000)
        dialog_copy.click()
        page.wait_for_selector(".toast.show", timeout=5000)
        page.wait_for_timeout(300)
        still_open = modal.locator(".copyfield", has_text="n3w-Gener4ted-pw").count() > 0
        shot(page, f"fresh-password-copy-{name}")
        if still_open != expect_open:
            fail(f"fresh password, copy {name}: dialog {'open' if still_open else 'closed'}")
        print(f"  fresh password, copy {name} -> dialog {'stays open' if still_open else 'closes'}")
        ctx.close()

    # 2. auth-test answer without `checks`: the result renders, the app stays.
    ctx, page = open_page(browser)
    open_deployment(page, "Auth")
    page.click('[data-testid="auth-test"]')
    page.wait_for_selector('[data-testid="auth-probe-result"]', timeout=10000)
    if not page.locator("nav.tabs").count():
        fail("auth-test without checks: the app is gone")
    shot(page, "auth-test-no-checks")
    print("  auth-test without `checks` -> result rendered, app intact")
    ctx.close()

    # 3. a render error inside a view: the view boundary catches it.
    ctx, page = open_page(browser, render_error=r"TypeError: \w+\.map is not a function", answers={
        "auth_provider": {**AUTH_PROVIDER, "spec": {"kind": "ldap", "role_mappings": "not-a-list"}}})
    open_deployment(page, "Auth")
    page.wait_for_selector('[data-testid="error-boundary"]', timeout=10000)
    if not page.locator("header.topbar").count() or not page.locator("nav.tabs").count():
        fail("view render error: the header/navigation did not survive")
    shot(page, "boundary-view")
    page.locator("nav.tabs > button").first.click()
    page.wait_for_selector('[data-testid="error-boundary"]', state="detached", timeout=5000)
    print("  view render error -> view fallback, header+nav intact, leaving the view recovers")
    ctx.close()

    # 4. a render error in the app shell: the top-level boundary catches it.
    ctx, page = open_page(browser, lang="pt", render_error=r"TypeError: [\w.]+\.slice is not a function", answers={
        "build_info": {"version": "0.12.0", "commit": 7, "image_digest": None}})
    page.wait_for_selector('[data-testid="error-boundary"]', timeout=20000)
    body = page.locator('[data-testid="error-boundary"]').inner_text()
    if "Recarregar" not in body:
        fail(f"top-level fallback is not translated / has no reload: {body!r}")
    shot(page, "boundary-top")
    print("  shell render error -> top-level fallback (pt) with reload")
    ctx.close()

    # 5. tooltips, in each language, on the login screen and in the app.
    for lang, (theme_tip, lang_tip) in TOOLTIPS.items():
        ctx, page = open_page(browser, lang=lang, answers={
            "auth_state": {"first_run": False, "authenticated": False}})
        page.wait_for_selector('input[name="username"]', timeout=20000)
        prefs = page.locator(".prefs button")
        got = (prefs.nth(0).get_attribute("title"), prefs.nth(1).get_attribute("title"))
        if got != (theme_tip, lang_tip):
            fail(f"[{lang}] login tooltips {got}, expected {(theme_tip, lang_tip)}")
        ctx.close()

        ctx, page = open_page(browser, lang=lang)
        page.wait_for_selector('[data-testid="theme-toggle"]', timeout=20000)
        got = page.get_attribute('[data-testid="theme-toggle"]', "title")
        if got != theme_tip:
            fail(f"[{lang}] theme toggle tooltip {got!r}, expected {theme_tip!r}")
        open_deployment(page)
        copy_tip = page.locator(".copyfield button").first.get_attribute("title", timeout=15000)
        if copy_tip != COPY_TIP[lang]:
            fail(f"[{lang}] copy tooltip {copy_tip!r}, expected {COPY_TIP[lang]!r}")
        print(f"  [{lang}] tooltips: theme {theme_tip!r}, language {lang_tip!r}, copy {copy_tip!r}")
        ctx.close()

    browser.close()

print(f"{len(faults)} faults, {len(tolerated)} tolerated console lines")
for line in sorted(set(tolerated)):
    print(f"    tolerated x{tolerated.count(line)}: {line[:160]}")
if faults:
    print("FAIL — captured faults:")
    for x in faults:
        print("  ", x)
    sys.exit(1)
print("PASS — copy is truthful, render errors are contained, tooltips are translated")
