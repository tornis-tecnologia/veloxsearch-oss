#!/usr/bin/env python3
"""Browser smoke + network gate for the React SPA (ADR-032, issue #32).

Beyond the old console/pageerror hydration check, this also watches the
NETWORK and fails on any captured fault:
  * any console message of type "error" (plus the panic/hydration filter)
  * any uncaught pageerror
  * any failed request (requestfailed), and
  * any non-2xx (>=400) response for an /api/* call or a same-origin asset

It prints a one-line summary `N console / M requests / K errors` and exits
non-zero if K > 0.

The UI is a single-URL SPA: screens are swapped by React state off
`GET /api/auth_state`, there are no routes to visit. This drives the boot
journey by the rendered widgets:
  login (or first-run setup) -> bootstrap/conformity gate -> home mount
  -> the Create wizard.

The Create wizard is stepped to the review screen but NOT submitted: a
smoke test must not provision a real OpenSearch cluster — the actual
create+delete round-trip is journey_check.py's job (it cleans up).

Usage: browser_check.py <base_url> <user> <pw>
"""
import re
import sys
from playwright.sync_api import sync_playwright

base, user, pw = sys.argv[1], sys.argv[2], sys.argv[3]

console = []          # (type, text) for every console message
errors = []           # uncaught pageerrors
requests = []         # every request URL (for the M count)
bad_responses = []    # >=400 on /api/* or same-origin assets
failed_requests = []  # requestfailed, minus benign teardown aborts


def on_response(resp):
    try:
        url = resp.url
        if resp.status >= 400 and ("/api/" in url or url.startswith(base)):
            bad_responses.append(f"{resp.status} {resp.request.method} {url[:160]}")
    except Exception:
        pass


def on_requestfailed(req):
    # An SSE stream / in-flight asset aborts when we close the page or the SPA
    # tears a screen down — net::ERR_ABORTED is teardown noise, not a fault.
    f = req.failure or ""
    if "ERR_ABORTED" in f:
        return
    failed_requests.append(f"{req.method} {req.url[:160]} — {f}")


with sync_playwright() as p:
    b = p.chromium.launch()
    ctx = b.new_context(ignore_https_errors=True)
    page = ctx.new_page()
    page.on("console", lambda m: console.append((m.type, m.text[:200])))
    page.on("pageerror", lambda e: errors.append(str(e)[:300]))
    page.on("request", lambda r: requests.append(r.url))
    page.on("response", on_response)
    page.on("requestfailed", on_requestfailed)

    # 1. boot: the SPA probes /api/auth_state and mounts setup (first_run) or
    #    login. networkidle never fires (SSE) — settle on the rendered widget.
    page.goto(base, wait_until="domcontentloaded")
    page.wait_for_selector('input[name="username"], nav.tabs', timeout=30000)
    is_setup = page.locator('input[name="confirm"]').count() > 0
    print(f"  boot screen: {'setup (first-run)' if is_setup else 'login'}")

    # 1a. exercise the pre-auth .prefs bar (theme button first, lang second).
    if page.locator(".prefs button").count() >= 1:
        before = page.evaluate("document.documentElement.getAttribute('data-theme')")
        page.click(".prefs button:nth-child(1)")  # theme toggle
        page.wait_for_timeout(250)
        after = page.evaluate("document.documentElement.getAttribute('data-theme')")
        assert before != after, f"theme toggle did not flip: {before!r} -> {after!r}"

    # 2. authenticate (setup creates the admin + auto-logins; login signs in).
    page.fill('input[name="username"]', user)
    page.fill('input[name="password"]', pw)
    if is_setup:
        page.fill('input[name="confirm"]', pw)
    page.click('button[type="submit"]')

    # 3. bootstrap/conformity gate (if shown) -> home. cert-manager + operator
    #    install can take minutes on a fresh cluster; on a ready one the gate is
    #    skipped and nav.tabs comes straight up.
    if page.locator("ul.req-list").count() == 0:
        try:
            page.wait_for_selector("ul.req-list", timeout=3000)
            print("  passed through the bootstrap/conformity gate")
        except Exception:
            pass  # already ready — no gate
    page.wait_for_selector("nav.tabs", timeout=600000)

    # 4. home mount: top tabs render (Status / Create / Settings).
    page.wait_for_timeout(1500)
    if page.locator("nav.tabs button").count() < 3:
        raise AssertionError("home tabs missing")
    body = page.inner_text("body")
    assert "Status" in body, f"Status tab missing: {body[:200]}"

    # 5. create flow: drive the wizard up to review (do NOT submit).
    #    The Backup step (ADR-049) is skipped by just clicking next — that IS
    #    the assertion that it is optional (invariant 6).
    #
    #    FOUR steps since ADR-053 rev. 9-10: purpose -> size -> backup -> review.
    #    The data-sources step was removed deliberately (enabling an integration
    #    is a day-2 decision that belongs to the Integrations tab), and this walk
    #    still clicked through it — landing on review, where `snap-toggle` does
    #    not exist, and timing out after 30s.
    #
    #    Asserted rather than counted, so the next change to the wizard's shape
    #    fails here with a sentence instead of a Playwright timeout.
    page.click("nav.tabs button:nth-child(2)")  # Create
    page.wait_for_selector(".stepper", timeout=10000)
    # `.stepper` interleaves `.step` with `.line` separators (4 steps + 3 lines),
    # so count the steps themselves, not the container's children.
    steps = page.locator(".stepper .step").count()
    assert steps == 4, f"create wizard should have 4 steps (ADR-053 rev. 9-10), found {steps}"
    page.fill('input[name="name"]', "smoke-test")   # step 1: name
    page.locator('[data-testid="wizard-next"]').click()  # -> size
    page.wait_for_timeout(300)
    page.locator('[data-testid="wizard-next"]').click()  # -> backup (optional)
    page.wait_for_timeout(300)
    assert not page.locator('[data-testid="snap-toggle"]').is_checked(), \
        "the Backup step must default to off"
    page.locator('[data-testid="wizard-next"]').click()  # -> review
    page.wait_for_selector(".kvrow", timeout=10000)
    assert page.locator(".kvrow").count() > 0, "review summary did not render"
    print("  create wizard reached review (not submitted)")

    # 6. activity locks (ADR-050). Whatever state the first deployment is in,
    #    the two rules must hold together: a busy deployment disables its save
    #    controls and says why, and delete is NEVER disabled — a provision that
    #    never finishes is exactly when the user needs it (invariant 4).
    page.click("nav.tabs button:nth-child(1)")  # Status
    page.wait_for_timeout(800)
    if page.locator(".cluster-card").count() > 0:
        page.locator(".cluster-card [data-testid], .cluster-card button").first.click()
        page.wait_for_selector("nav.tabs", timeout=15000)
        page.wait_for_timeout(1200)
        busy = page.locator('[data-testid="activity-panel"]').count() > 0

        assert page.locator('[data-testid="delete-deployment"]').count() == 0 or \
            not page.locator('[data-testid="delete-deployment"]').is_disabled(), \
            "delete must never be disabled, busy or not (ADR-050 invariant 4)"

        if busy:
            panel = page.locator('[data-testid="activity-panel"]')
            kind = panel.get_attribute("data-activity-kind")

            # 6a. no counter on the panel may exceed its own total (issue
            #     #131). "6/3" and "8/3" were on screen for hours because the
            #     numerator came off the live StatefulSet and the denominator
            #     off the CR spec, with nothing holding them together.
            for testid in ("activity-nodes", "activity-detail"):
                el = page.locator(f'[data-testid="{testid}"]')
                if el.count() == 0:
                    continue
                m = re.search(r"(\d+)\s*/\s*(\d+)", el.inner_text())
                if m:
                    done, total = int(m.group(1)), int(m.group(2))
                    assert done <= total, \
                        f"{testid} reads {done}/{total} — a counter cannot exceed its total"

            # 6b. a stalled activity explains itself where it is, never in a
            #     toast and never as "this is taking longer than usual" alone
            #     (ADR-050 UI rule 5).
            if panel.get_attribute("data-activity-stalled") == "true":
                stall = page.locator('[data-testid="activity-stall"]')
                assert stall.count() > 0, \
                    "a stalled deployment must say WHY, in place (ADR-050 UI rule 5)"
                assert stall.locator("li").count() > 0, \
                    "the stall notice rendered no reason at all"
                print(f"  stall reason rendered: {stall.inner_text()[:120]!r}")

            assert page.locator('[data-testid="lock-notice"]').count() >= 0
            page.locator('nav.tabs button:nth-child(2)').click()  # Edit tab
            page.wait_for_timeout(600)
            assert page.locator('[data-testid="edit-save"]').is_disabled(), \
                f"save must be disabled while the deployment is {kind}"
            assert page.locator('[data-testid="lock-notice"]').count() > 0, \
                "a locked tab must say why, not just disable the button"
            print(f"  activity lock verified (deployment is {kind})")
        else:
            print("  every deployment is settled — the lock assertions were not exercised")
    else:
        print("  no deployments — the activity-lock assertions were not exercised")

    b.close()

# ── verdict ──────────────────────────────────────────────────────────────
console_errors = [
    f"{ty}: {tx}" for ty, tx in console
    if ty == "error" or "panic" in tx.lower() or "hydrat" in tx.lower()
]
faults = console_errors + errors + bad_responses + failed_requests

print(f"{len(console)} console / {len(requests)} requests / {len(faults)} errors")
if faults:
    print("FAIL — captured faults:")
    for x in faults:
        print("  ", x)
    sys.exit(1)
print("PASS — clean console + network; login→bootstrap→home→create OK")
