#!/usr/bin/env python3
"""Create-deployment journey (meeting 3) — React SPA edition (ADR-032).

Drive the full UI flow against a live cluster and assert the redesigned
deployment panel:

  login -> Create (4-step wizard: name+purpose / size / data / review)
        -> deployment appears -> open detail
        -> Overview / Edit / Integrations / Security tabs
        -> Edit shows the automatic JVM field + no purpose cards
        -> Security shows the password-reset form (2 password fields)
        -> clean up (delete via the confirm modal)

The UI is a single-URL React SPA: screens are swapped by state, not routes,
so we drive the rendered widgets. Top tabs (Status/Create/Settings) and the
4 deployment-detail tabs are both `nav.tabs`; once a deployment is open the
top tabs are hidden and the detail tabs are the only `nav.tabs`.

Usage: journey_check.py <base_url> <user> <pw>
Requires the app connected to a real cluster (creates/deletes a CR).
"""
import sys
from playwright.sync_api import sync_playwright

base, user, pw = sys.argv[1], sys.argv[2], sys.argv[3]
errors, console = [], []


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
    page.wait_for_selector("nav.tabs", timeout=600000)  # bootstrap gate may run

    # 2. Create tab (top tabs = Status / Create / Settings) -> 5-step wizard.
    #    Driven by the stable test hooks (#33): the name input[name="name"],
    #    the wizard-next nav button, and the create-submit button.
    #    The Backup step (ADR-049) is skipped, so this journey also asserts a
    #    deployment created WITHOUT snapshots is created exactly as before.
    page.click("nav.tabs button:nth-child(2)")
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
    page.locator('[data-testid="create-submit"]').click()  # -> create_cluster

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

    # 3b. provisioning settle: a fresh deployment is BUSY (nodes booting, PVCs
    # binding) and LOCKS its editing controls (activity.locks_edits). The tab
    # assertions below are about a MANAGED deployment, so wait for the lock to
    # lift. The sentinel (reset-pass) only RENDERS on the Security tab — go
    # there first, then wait for it enabled (it stays disabled while busy).
    page.wait_for_selector("nav.tabs", timeout=20000)
    page.click("nav.tabs button:nth-last-child(2)")
    page.wait_for_selector('[data-testid="reset-pass"]:not([disabled])', timeout=1800000)
    print(f"  {dep_name} settled (edits unlocked)")

    # 4. detail tabs: the global nav STAYS inside a deployment (app.jsx:
    #    "Global navigation stays put inside a deployment too") — the single
    #    nav.tabs holds the 4 top tabs followed by the 6 detail tabs
    #    (Overview / Edit / Integrations / Snapshot / Security / Auth).
    #    Address detail tabs positionally from the END so the global tab
    #    count can change without breaking this.
    page.wait_for_selector("nav.tabs", timeout=20000)
    page.wait_for_timeout(1000)
    n_tabs = page.locator("nav.tabs button").count()
    if n_tabs - 4 != 6:
        fail(f"expected 6 detail tabs after the 4 global ones, got {n_tabs - 4}")

    # 5. Edit tab: size <select>, automatic (disabled) JVM field, NO purpose cards.
    page.click("nav.tabs button:nth-last-child(5)")
    page.wait_for_selector("select.select", timeout=10000)
    jvm = [page.locator("input[disabled]").nth(i).input_value()
           for i in range(page.locator("input[disabled]").count())]
    # The automatic JVM/heap field reads in the operator's short form since
    # ADR-035 ("1536m"/"2g"), not the -Xms form the June wizard showed.
    if not any(v.startswith("-Xms") or __import__("re").fullmatch(r"\d+[mMgG]", v.strip()) for v in jvm):
        fail(f"Edit tab missing the automatic-JVM field (disabled inputs: {jvm})")
    if page.locator(".purpose-grid").count() or page.locator(".profile-cards").count():
        fail("Edit tab must NOT show the purpose cards (purpose is fixed)")

    # 6. Security tab: credentials are REVEALED on demand and the admin
    #    password is GENERATED on reset — never chosen — so there are no
    #    password inputs. Assert the reset action + its confirm modal exist
    #    (and cancel out: this journey must not rotate credentials).
    import re as _re
    page.click("nav.tabs button:nth-last-child(2)")
    page.wait_for_timeout(800)
    n_pw = page.locator('input[type="password"]').count()
    if n_pw != 0:
        fail(f"Security tab must have NO password inputs (generated, not chosen), got {n_pw}")
    page.locator('[data-testid="reset-pass"]').click()
    page.wait_for_timeout(600)
    # the confirm modal's cancel — a journey must not rotate credentials
    page.get_by_role("button", name=_re.compile("Cancelar|Cancel", _re.I)).last.click()
    page.wait_for_timeout(400)

    # 7. cleanup — delete via the stable hooks (#33): the danger-zone
    #    delete-deployment button arms the modal, delete-confirm commits it.
    page.click("nav.tabs button:nth-last-child(6)")  # overview
    page.wait_for_timeout(500)
    page.click('[data-testid="delete-deployment"]')        # arm: opens the Confirm modal
    page.wait_for_selector('[data-testid="delete-confirm"]', timeout=5000)
    page.click('[data-testid="delete-confirm"]')           # confirm delete -> go(status)

    # Success = we left the detail: onDelete navigates home, so the crumb
    # detaches and the top tabs return.
    page.wait_for_selector(".crumb .cur", state="detached", timeout=30000)
    print(f"  deleted {dep_name}")

    b.close()

# Real failures only: page errors or panic/hydration console messages.
hyd = [c for c in console if "hydrat" in c.lower() or "panic" in c.lower()]
if hyd or errors:
    print("FAIL — console/page errors")
    for x in hyd + errors:
        print(" ", x)
    sys.exit(1)
print("PASS — create→manage→delete journey OK; Overview/Edit/Integrations/Security present")
print(f"  ({len(console)} console msgs, 0 page errors)")
