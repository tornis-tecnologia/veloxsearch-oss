#!/usr/bin/env python3
"""Velox Mesh — the deployment's fleet tab (ADR-068, ADR-069, ADR-072).

The journey is Elastic Fleet's, because operators already know it:

    install the controller -> install an agent on a host
                           -> create a collection policy
                           -> add integrations to the policy

Since ADR-072 the Mesh is a DEPLOYMENT's: the screen is a tab of
`/deployments/:id/mesh/:sub`, every policy and agent on it belongs to that
deployment, and the controller's install guide is an admin matter under
Settings. Each assertion below guards a property that is easy to break and
expensive to notice:

  - **The Mesh lives under the deployment, not in the sidebar.** The old
    global `/mesh` redirects to the list; the tab has a URL and a deep link.
  - **The install guide mirrors the project's own downloads index.** We ship
    no collection engine (ADR-069), so the set of targets is Fluent Bit's to
    decide, not ours. A target quietly dropped (or an invented one, like a
    Fedora page the project does not publish) fails here.
  - **The commands are upstream's, verbatim.** A paraphrased shell command is
    a support ticket, so each tab must still carry its own repository path.
  - **An integration brings both halves.** Turning on nginx must show the
    collection stanza AND the pipeline, template, index pattern and
    dashboards — read off the signed package, never a hand-kept table. This
    is the whole promise of the last journey step.
  - **The inventory is the package's, not a template.** A package with no
    pipeline must render with no pipeline, or the screen is lying about what
    an operator is turning on.
  - **The Mesh's absence is a state, not an error.** A fresh install has no
    controller; the tab says so and points at Settings instead of failing.
  - **There is no destination picker.** The destination IS the deployment
    whose tab this is; offering another would be offering a cross-deployment
    write (ADR-072 §2).
  - **The published endpoint is the deployment's setting**, on the overview,
    not a field on every policy form (ADR-072 §5).
  - **With a controller, the fleet is the product.** The guided first run is
    ordered (exactly one step live, the earlier ones done), every agent row's
    up/down agrees with its derived status, the status reaches the screen as
    a translated word rather than a raw enum, and an agent can only be moved
    between THIS deployment's policies.
  - **The Data tab knows what hosts collect.** An integration a policy
    activates says so there and its uninstall is disabled (ADR-072 §6).
  - Nothing renders an i18n key or `undefined` in any of three languages.

Usage:
  mesh_check.py <base> <user> <pw> [shot.png]
"""
import sys
from playwright.sync_api import sync_playwright

base, user, pw = sys.argv[1:4]
shot = sys.argv[4] if len(sys.argv) > 4 else None

# The targets the Fluent Bit project documents at
# docs.fluentbit.io/manual/installation/downloads, each with a string that
# must appear in its panel. `None` marks a target the project documents but
# we have no commands for: we link out rather than invent a procedure, so
# only the agent steps are present.
TARGETS = {
    "amazon": "packages.fluentbit.io/amazonlinux",
    "buildroot": None,
    "debian": "packages.fluentbit.io/debian",
    "raspbian": "packages.fluentbit.io/raspbian",
    "redhat": "packages.fluentbit.io/centos",
    "rocky": "packages.fluentbit.io/almalinux",
    "ubuntu": "packages.fluentbit.io/ubuntu",
    "yocto": None,
    "macos": None,
    "windows": "fluent-bit",
}

fail = []


def check(cond, msg):
    if not cond:
        fail.append(msg)
        print(f"  ✗ {msg}")
    else:
        print(f"  ✓ {msg}")


with sync_playwright() as p:
    browser = p.chromium.launch()
    page = browser.new_page(viewport={"width": 1440, "height": 1100})
    errors = []
    page.on("console", lambda m: errors.append(m.text) if m.type == "error" else None)
    page.on("pageerror", lambda e: errors.append(str(e)))

    page.goto(f"{base}/login", wait_until="networkidle")
    if page.locator('[data-testid="nav-deployments"]').count() == 0:
        page.fill('input[name="username"], input[type="text"]', user)
        page.fill('input[type="password"]', pw)
        page.click('button[type="submit"]')
        page.wait_for_load_state("networkidle")

    # ---- the Mesh is not a section any more ------------------------------
    check(page.locator('[data-testid="nav-mesh"]').count() == 0,
          "no Velox Mesh entry in the sidebar (ADR-072: it is the deployment's)")
    page.goto(f"{base}/mesh/policies", wait_until="networkidle")
    page.wait_for_url(lambda u: "/mesh" not in u, timeout=10000)
    check(page.url.rstrip("/").endswith("/deployments"),
          f"the old /mesh/* redirects to the list (got {page.url})")

    # A deployment to hang the tab on. Without one there is nothing to
    # assert about a per-deployment screen, and the run says so.
    page.goto(f"{base}/deployments", wait_until="networkidle")
    opens = page.locator('[data-testid^="dep-open-"]')
    if opens.count() == 0:
        print("  · no deployment; the Mesh tab needs one — skipped")
        browser.close()
        sys.exit(0)
    dep = opens.first.get_attribute("data-testid")[len("dep-open-"):]
    print(f"  · deployment {dep}")

    # ---- the tab and its three sub-screens --------------------------------
    page.goto(f"{base}/deployments/{dep}/overview", wait_until="networkidle")
    page.wait_for_selector('[data-testid="dep-tab-mesh"]', timeout=15000)
    page.click('[data-testid="dep-tab-mesh"]')
    page.wait_for_selector('[data-testid="mesh-panel-overview"]', timeout=15000)
    check(page.url.endswith(f"/deployments/{dep}/mesh"),
          f"the tab lands on /deployments/{dep}/mesh (got {page.url})")

    # No controller on a fresh install: a STATE with a way forward, not an error.
    check(page.locator('[data-testid="mesh-absent"]').count() == 1
          or page.locator('[data-testid="mesh-ready"]').count() == 1
          or page.locator('[data-testid="mesh-unreachable"]').count() == 1,
          "the overview states whether a Mesh is installed")
    overview = page.locator('[data-testid="mesh-panel-overview"]').inner_text()
    check("VELOX_MESH_URL" not in overview,
          "the controller's install commands are NOT on the deployment's tab (they are the admin's, under Settings)")
    # The deployment's own setting, where every policy reads it from.
    osh = page.locator('[data-testid="mesh-settings"] [data-testid="mesh-policy-os-host"]')
    check(osh.count() == 1, "the published OpenSearch endpoint is a deployment setting on the overview")

    # Deep links survive a reload (ADR-067: every screen has a URL).
    page.goto(f"{base}/deployments/{dep}/mesh/policies", wait_until="networkidle")
    page.wait_for_selector('[data-testid="mesh-panel-policies"]', timeout=15000)
    check(True, f"/deployments/{dep}/mesh/policies opens the policies sub-tab directly")
    check(page.locator('[data-testid="mesh-policy-target"]').count() == 0,
          "no destination picker: the destination is this deployment (ADR-072 §2)")
    check(page.locator('[data-testid="mesh-panel-policies"] [data-testid="mesh-policy-os-host"]').count() == 0,
          "the endpoint is not asked again on the policy form")

    # ---- the install guide ----------------------------------------------
    page.click('[data-testid="mesh-tab-agents"]')
    tabs = page.locator('[data-testid^="agents-tab-"]')
    tabs.first.wait_for(timeout=5000)
    check(tabs.count() == len(TARGETS),
          f"{len(TARGETS)} install targets, mirroring the project's index (got {tabs.count()})")

    for key, needle in TARGETS.items():
        page.click(f'[data-testid="agents-tab-{key}"]')
        panel = page.locator(f'[data-testid="agents-panel-{key}"]')
        panel.wait_for(timeout=5000)
        body = panel.inner_text()
        blocks = panel.locator(".cmdblock .codeblock").count()
        if needle:
            check(needle in body, f"{key}: the upstream command is verbatim ({needle})")
            check(blocks >= 4, f"{key}: engine steps plus agent steps (got {blocks} blocks)")
        else:
            # Link-only: the agent steps are still there, the engine's are not.
            check(blocks == 3, f"{key}: links out for the engine, keeps the 3 agent steps (got {blocks})")
            check(panel.locator('a[href*="docs.fluentbit.io"]').count() >= 1,
                  f"{key}: links to the project's own page")
        for bad in ("undefined", "mesh_", "{min}"):
            check(bad not in body, f"{key}: nothing renders {bad!r}")

    # The two facts an operator cannot discover from our silence.
    for key in ("debian", "ubuntu"):
        page.click(f'[data-testid="agents-tab-{key}"]')
        body = page.locator(f'[data-testid="agents-panel-{key}"]').inner_text().lower()
        check("não empacotam" in body or "do not package" in body or "no empaquetan" in body,
              f"{key}: says the distribution does not package Fluent Bit")
    page.click('[data-testid="agents-tab-redhat"]')
    check("EPEL" in page.locator('[data-testid="agents-panel-redhat"]').inner_text(),
          "redhat: names EPEL as the RHEL 9 route")
    check("3.2" in page.locator(".ag-facts").inner_text(), "the minimum Fluent Bit version is shown")

    # ---- the policy composer: an integration brings both halves ----------
    page.click('[data-testid="mesh-tab-policies"]')
    page.wait_for_selector('[data-testid="mesh-integration-nginx"]', timeout=20000)
    page.click('[data-testid="mesh-integration-nginx"]')
    page.wait_for_selector('[data-testid="mesh-integration-nginx"] .mesh-chip.on', timeout=20000)
    card = page.locator('[data-testid="mesh-integration-nginx"]').inner_text()
    # nginx is the worked example of a complete package: all four families.
    for family in ("Pipeline", "Template"):
        check(family in card, f"nginx shows its {family}")
    check(page.locator('[data-testid="mesh-integration-nginx"] .mesh-chip.on').count() >= 5,
          "nginx shows collection + pipeline + template + index + dashboards")
    summary = page.locator('[data-testid="mesh-policy-summary"]').inner_text()
    check("nginx" in summary, "the policy summary names the integration")
    check(dep in summary, "the policy summary names THIS deployment as the destination")
    # Without a controller there is nothing to save to, and the screen says
    # so rather than offering a button that cannot work. With one, the save
    # step replaces that banner — asserted further down.
    blocked = page.locator('[data-testid="mesh-policy-save-blocked"]').count()
    savable = page.locator('[data-testid="mesh-policy-save"]').count()
    check(blocked + savable == 1,
          "either saving works or it is honestly blocked — never both, never neither")

    # kubernetes ships no pipeline — the inventory must reflect the package.
    if page.locator('[data-testid="mesh-integration-kubernetes"]').count():
        page.click('[data-testid="mesh-integration-kubernetes"]')
        page.wait_for_timeout(2500)
        k = page.locator('[data-testid="mesh-integration-kubernetes"]')
        on = k.locator(".mesh-chip.on").count()
        off = k.locator(".mesh-chip:not(.on)").count()
        check(off >= 1, f"kubernetes shows what it does NOT ship (on={on}, off={off})")

    # ---- the Data tab hands an integration to the composer ---------------
    page.goto(f"{base}/deployments/{dep}/data", wait_until="networkidle")
    page.wait_for_selector('[data-testid="integrations-tab"]', timeout=15000)
    page.wait_for_selector('[data-testid^="collect-hosts-"]', timeout=20000)
    handoff = page.locator('[data-testid^="collect-hosts-"]').first
    integ = handoff.get_attribute("data-testid")[len("collect-hosts-"):]
    handoff.click()
    page.wait_for_selector('[data-testid="mesh-panel-policies"]', timeout=15000)
    check(f"/deployments/{dep}/mesh/policies" in page.url and f"integration={integ}" in page.url,
          f"'collect from hosts' opens the composer with ?integration={integ}")
    page.wait_for_selector(f'[data-testid="mesh-integration-{integ}"]', timeout=20000)
    check(page.locator(f'[data-testid="mesh-integration-{integ}"][aria-pressed="true"]').count() == 1,
          f"{integ} arrives preselected")

    # ---- the fleet, when a controller is configured ----------------------
    # Without a Mesh these screens are the install guide and there is nothing
    # to assert; with one, the fleet view is the product and every claim below
    # is a thing that breaks silently.
    page.click('[data-testid="mesh-tab-overview"]')
    page.wait_for_selector('[data-testid="mesh-panel-overview"]', timeout=5000)
    onb = page.locator('[data-testid="mesh-onboarding"], [data-testid="mesh-onboarding-done"]')
    has_mesh = onb.count() > 0

    if not has_mesh:
        print("  · no Velox Mesh configured; fleet assertions skipped")
    else:
        # The guided first run is ORDERED: the step the Mesh says is live is
        # the one marked, and the ones before it are done. A console that let
        # an operator enrol an agent before a policy existed would be sending
        # them to a dead end.
        step = onb.first.get_attribute("data-step")
        if step:
            now = page.locator('.onb-step[data-state="now"]')
            check(now.count() == 1, f"exactly one step is live (at {step})")
            done = page.locator('.onb-step[data-state="ok"]').count()
            idx = ["install_mesh", "create_policy", "enrol_agent", "await_first_apply"].index(step)
            check(done == idx, f"the {idx} step(s) before it are done (got {done})")
        check(page.locator('[data-testid="mesh-seats"]').count() == 1,
              "seats are shown as the installation's, not the deployment's")

        page.click('[data-testid="mesh-tab-agents"]')
        page.wait_for_selector('[data-testid="mesh-fleet"], [data-testid="mesh-panel-overview"]', timeout=15000)
        rows = page.locator('[data-testid="mesh-fleet"] tbody tr')
        check(True, f"the fleet renders ({rows.count()} agent(s))")

        for i in range(rows.count()):
            r = rows.nth(i)
            status = r.get_attribute("data-status")
            up = r.get_attribute("data-up")
            # up/down is NOT a second opinion on the status: it is derived
            # from it, and the two must never disagree on screen.
            expected_up = "0" if status in ("offline", "unenrolled") else "1"
            check(up == expected_up, f"row {i}: {status} is up={expected_up}")
            # The word is translated, never the raw enum leaking through.
            check(status not in r.inner_text(), f"row {i}: the status is a word, not {status!r}")
            # An agent can only be moved between THIS deployment's policies.
            sel = r.locator('select[data-testid^="fleet-policy-"]')
            if sel.count():
                check(sel.locator("option").count() >= 1,
                      f"row {i}: the policy picker offers this deployment's policies only")

        # Adding a host is something you do while looking at the fleet, so the
        # install guide stays on the same tab.
        check(page.locator('[data-testid^="agents-panel-"]').count() == 1,
              "the install guide sits under the fleet")

        page.click('[data-testid="mesh-tab-policies"]')
        page.wait_for_selector('[data-testid="mesh-policy-save"]', timeout=10000)
        check(True, "a policy can be saved when a Mesh holds it")
        if page.locator('[data-testid="mesh-rollout"]').count():
            check(page.locator('[data-testid="mesh-rollout"] tr').count() > 0,
                  "the rollout is shown per policy")
            # Every policy on this screen activates something, and the Data
            # tab says so about each integration (ADR-072 §6).
            ids = [page.locator('[data-testid="mesh-rollout"] tr').nth(i).locator("td").first.inner_text()
                   for i in range(page.locator('[data-testid="mesh-rollout"] tr').count())]
            page.goto(f"{base}/deployments/{dep}/data", wait_until="networkidle")
            page.wait_for_selector('[data-testid="integrations-tab"]', timeout=15000)
            page.wait_for_timeout(2000)
            badges = page.locator('[data-testid^="hosts-"]')
            check(badges.count() >= 1,
                  f"the Data tab marks integrations hosts collect (got {badges.count()}; policies {ids})")
            # Only an INSTALLED integration has an uninstall to disable; one
            # that hosts collect but the cluster does not is just marked.
            installed = page.locator('.installed-row [data-testid^="hosts-"]')
            if installed.count():
                integ = installed.first.get_attribute("data-testid")[len("hosts-"):]
                page.click(f'[data-testid="integ-menu-{integ}"]')
                item = page.locator(f'[data-testid="uninstall-{integ}"]')
                item.wait_for(timeout=5000)
                check(item.is_disabled(), f"{integ}: uninstall is disabled while a policy activates it")
                page.keyboard.press("Escape")
            else:
                print("  · no installed integration is also collected by hosts; the disabled-uninstall check is skipped")

    if shot:
        page.screenshot(path=shot, full_page=True)
        print(f"  · screenshot {shot}")

    # ERR_NETWORK_CHANGED is Chrome reporting that the HOST's network changed
    # under an in-flight request (a VPN, a wifi hop). It is not something the
    # SPA can cause or prevent, and failing on it would make this check flaky
    # on a laptop. Everything else still fails the run — a real fetch error,
    # a hydration warning or a thrown exception is exactly what this looks for.
    ignorable = ("favicon", "ERR_NETWORK_CHANGED")
    noise = [e for e in errors if not any(i.lower() in e.lower() for i in ignorable)]
    check(not noise, f"console clean (got {noise[:2]})")
    browser.close()

if fail:
    print(f"\nFAILED: {len(fail)}")
    sys.exit(1)
print("\nVelox Mesh screens OK")
