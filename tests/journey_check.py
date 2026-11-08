#!/usr/bin/env python3
"""Create-deployment journey (meeting 3) — React SPA edition (ADR-032).

Drive the full UI flow against a live cluster and assert the redesigned
deployment panel:

  login -> Create (4-step wizard: name+purpose / size / data / review)
        -> DOUBLE-click create -> exactly one deployment appears (#56)
        -> open detail
        -> Overview / Data / Backup / Access / Configuration / Activity tabs
        -> Configuration shows the automatic JVM heap + no purpose cards
        -> Access shows the password reset (generated, never typed)
        -> clean up (delete via the confirm modal)

Every screen has a URL (frontend/router.jsx), and the widgets carry stable
data-testids: nav-*, create-deployment, dep-open-<id>, dep-tab-<key>.

Usage: journey_check.py <base_url> <user> <pw>
Requires the app connected to a real cluster (creates/deletes a CR).
"""
import sys
from playwright.sync_api import sync_playwright

base, user, pw = sys.argv[1], sys.argv[2], sys.argv[3]
errors, console = [], []


def detail_tab(key):
    """A deployment tab by its stable key: overview | data | backup | access | config | activity."""
    return page.locator(f'[data-testid="dep-tab-{key}"]')


def fail(msg):
    print("FAIL:", msg)
    sys.exit(1)


with sync_playwright() as p:
    b = p.chromium.launch()
    ctx = b.new_context(ignore_https_errors=True)
    page = ctx.new_page()
    page.on("console", lambda m: console.append(f"{m.type}: {m.text[:200]}"))
    page.on("pageerror", lambda e: errors.append(str(e)[:300]))

    # 1. login (the SPA mounts AuthView mode=login when auth_state reports an
    #    existing admin who isn't authenticated). No /login route to visit.
    page.goto(base, wait_until="domcontentloaded")
    page.wait_for_selector('input[name="username"]', timeout=20000)
    page.fill('input[name="username"]', user)
    page.fill('input[name="password"]', pw)
    page.click('button[type="submit"]')
    page.wait_for_selector('[data-testid="nav-deployments"]', timeout=600000)  # bootstrap gate may run

    # 2. "New deployment" -> the 4-step wizard (/deployments/new).
    #    Driven by the stable test hooks (#33): the name input[name="name"],
    #    the wizard-next nav button, and the create-submit button.
    #    The Backup step (ADR-049) is skipped, so this journey also asserts a
    #    deployment created WITHOUT snapshots is created exactly as before.
    page.click('[data-testid="create-deployment"], [data-testid="create-first"]')
    page.wait_for_selector(".stepper", timeout=10000)
    page.fill('input[name="name"]', "journeytest")  # step 1: name (+ default purpose)
    page.locator('[data-testid="wizard-next"]').click()  # -> size
    page.wait_for_timeout(300)
    # The 4-step wizard has NO default size: the next button stays disabled
    # until a sizing preset card is picked (ADR-016 — presets come from the
    # backend). Pick the second card (medium), i18n-proof via aria-pressed.
    page.locator('button.purpose:not([aria-pressed="true"])').nth(0).click()  # small — the only preset a 16Gi fixture fits
    page.wait_for_timeout(300)
    page.locator('[data-testid="wizard-next"]').click()  # -> backup (skipped)
    page.wait_for_timeout(300)
    page.locator('[data-testid="wizard-next"]').click()  # -> review
    page.wait_for_selector(".kvrow", timeout=10000)        # review summary rendered
    def journey_deployments():
        items = page.evaluate(
            "() => fetch('/api/list_deployments', {credentials: 'same-origin'})"
            ".then(r => r.json())")
        return {d["name"] for d in items if d["name"].startswith("journeytest-")}

    # Leftovers of an earlier, aborted run are not this run's duplicates.
    before = journey_deployments()
    # #56: DOUBLE-click. The second click must be a no-op (button disabled from
    # the first); asserted below as exactly one journeytest-* deployment.
    page.locator('[data-testid="create-submit"]').dblclick()  # -> create_cluster

    # 3. success: the SPA navigates to the new deployment's detail. The topbar
    #    crumb (.crumb .cur) only renders once the deployment shows up in the
    #    SSE list and the detail mounts — wait for it, then read the name.
    # Greenfield first-create includes the Longhorn bootstrap (ADR-031): the
    # wizard's storage-ready gate can hold the create for several MINUTES while
    # Longhorn installs and becomes the default SC. 15 min covers it.
    crumb = page.wait_for_selector(".crumb .cur", timeout=900000)
    dep_name = crumb.inner_text().strip()
    if not dep_name.startswith("journeytest-"):
        fail(f"unexpected generated name: {dep_name!r}")
    print(f"  created {dep_name}")

    # 3a. #56: the double click made ONE deployment. Each create request gets a
    # fresh suffix and spawns its own deferred provisioning task, so one
    # deployment is one task. Give a racing second create time to land.
    page.wait_for_timeout(10000)
    created = sorted(journey_deployments() - before)
    if created != [dep_name]:
        for extra in created:
            if extra != dep_name:  # never leave a duplicate running on a failed run
                page.evaluate(
                    "n => fetch('/api/delete_cluster', {method: 'POST', credentials: 'same-origin',"
                    " headers: {'Content-Type': 'application/json'}, body: JSON.stringify({name: n})})",
                    extra)
        fail(f"double-clicking create made {created}, expected only [{dep_name!r}]")
    print("  double click -> exactly one deployment")

    # 3b. provisioning settle: a fresh deployment is BUSY (nodes booting, PVCs
    # binding) and LOCKS its editing controls (activity.locks_edits). The tab
    # assertions below are about a MANAGED deployment, so wait for the lock to
    # lift. The sentinel (reset-pass) only RENDERS on the Security tab — go
    # there first, then wait for it enabled (it stays disabled while busy).
    page.wait_for_selector('[role="tablist"]', timeout=20000)
    detail_tab("access").click()
    page.wait_for_selector('[data-testid="reset-pass"]:not([disabled])', timeout=1800000)
    print(f"  {dep_name} settled (edits unlocked)")

    # 4. detail tabs: six, grouped by task (overview / data / backup / access /
    #    config / activity), each with its own URL.
    page.wait_for_selector('[role="tablist"]', timeout=20000)
    page.wait_for_timeout(1000)
    n_tabs = page.locator('[role="tab"]').count()
    if n_tabs != 6:
        fail(f"expected 6 deployment tabs, got {n_tabs}")
    if not page.url.endswith(f"/deployments/{dep_name}/access"):
        fail(f"the access tab has no URL of its own: {page.url}")

    # 5. Configuration tab: size <select>, the automatic JVM heap as a FACT
    #    (not a disabled input), NO purpose cards.
    detail_tab("config").click()
    page.wait_for_selector("select.select", timeout=10000)
    heap = page.locator('[data-testid="edit-heap"]').inner_text().strip()
    # The heap is operator-derived (ADR-035), shown in GiB/MiB.
    if not __import__("re").search(r"\d+(?:[.,]\d+)?\s*[GM]iB", heap):
        fail(f"Configuration tab missing the automatic-JVM fact (got {heap!r})")
    if page.locator(".purpose-grid").count() or page.locator(".profile-cards").count():
        fail("Configuration tab must NOT show the purpose cards (purpose is fixed)")

    # 6. Access tab: credentials are REVEALED on demand and the admin
    #    password is GENERATED on reset — never chosen — so there are no
    #    password inputs. Assert the reset action + its confirm modal exist
    #    (and cancel out: this journey must not rotate credentials).
    import re as _re
    detail_tab("access").click()
    page.wait_for_timeout(800)
    n_pw = page.locator('input[type="password"]').count()
    if n_pw != 0:
        fail(f"Security tab must have NO password inputs (generated, not chosen), got {n_pw}")
    page.locator('[data-testid="reset-pass"]').click()
    page.wait_for_timeout(600)
    # the confirm modal's cancel — a journey must not rotate credentials
    page.get_by_role("button", name=_re.compile("Cancelar|Cancel", _re.I)).last.click()
    page.wait_for_timeout(400)

    # 7. cleanup — delete via the stable hooks (#33): the delete-deployment
    #    button at the bottom of Configuration arms the modal, delete-confirm
    #    commits it. (It is NOT on every tab any more.)
    detail_tab("config").click()
    page.wait_for_selector('[data-testid="delete-deployment"]', timeout=10000)
    page.click('[data-testid="delete-deployment"]')        # arm: opens the Confirm modal
    page.wait_for_selector('[data-testid="delete-confirm"]', timeout=5000)
    # type-to-confirm: the delete stays disabled until the deployment's name is
    # typed (the Confirm modal's requireText) — an irreversible action names its target
    page.fill('[data-testid="confirm-text"]', dep_name)
    page.click('[data-testid="delete-confirm"]')           # confirm delete -> go(status)

    # Success = we left the detail: onDelete navigates to the list, so the
    # crumb detaches and the URL is the list again.
    page.wait_for_selector(".crumb .cur", state="detached", timeout=30000)
    if not page.url.rstrip("/").endswith("/deployments"):
        fail(f"after delete the app is not on the list: {page.url}")
    print(f"  deleted {dep_name}")

    b.close()

# Real failures only: page errors or panic/hydration console messages.
hyd = [c for c in console if "hydrat" in c.lower() or "panic" in c.lower()]
if hyd or errors:
    print("FAIL — console/page errors")
    for x in hyd + errors:
        print(" ", x)
    sys.exit(1)
print("PASS — create→manage→delete journey OK; six task tabs present, each with a URL")
print(f"  ({len(console)} console msgs, 0 page errors)")
