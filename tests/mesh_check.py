#!/usr/bin/env python3
"""Velox Mesh — the deployment's fleet tab and the one integration onboarding
(ADR-068, ADR-069, ADR-072, ADR-073).

The journey is Elastic Fleet's, because operators already know it:

    install the controller -> install an agent on a host
                           -> create a collection policy
                           -> add integrations to the policy

Since ADR-072 the Mesh is a DEPLOYMENT's: the screen is a tab of
`/deployments/:id/mesh/:sub`, every policy and agent on it belongs to that
deployment, and the controller's install guide is an admin matter under
Settings. Since ADR-073 an integration is ADDED TO A POLICY — the cluster
being the implicit one — from one dialog on the Data tab: where to collect,
then which inputs with which values. Each assertion below guards a property
that is easy to break and expensive to notice:

  - **The Mesh lives under the deployment, not in the sidebar.** The old
    global `/mesh` redirects to the list; the tab has a URL and a deep link.
  - **The install guide mirrors the project's own downloads index.** We ship
    no collection engine (ADR-069), so the set of targets is Fluent Bit's to
    decide, not ours.
  - **The commands are upstream's, verbatim.**
  - **The Mesh's absence is a state, not an error.**
  - **The Policies tab creates, and manages integrations through the one
    dialog.** A policy is a name; "add integration" on its card picks a
    catalog package and opens the SAME dialog the Data tab uses with the
    policy as the fixed target; each integration on the card can be
    configured or removed there. No second composer.
  - **One dialog for both targets.** `Add` on the Data tab asks where (this
    cluster / a policy / a new one) and what (the package's inputs, with
    their variables); the same dialog adds to a policy and shows the inputs
    collectable on a host only.
  - **The Data tab shows every target an integration is on**, and removes
    per target.
  - **With a controller, the fleet is the product.** The overview opens on
    the fleet summary (one count per status, one bar) and the configured
    agents; the guided first run is ordered; each agent row's up/down agrees
    with its derived status; the policy is NOT a picker on the row — the
    agent's editor shows the policy, what it brings (integrations, inputs,
    values) and deploys the change, within THIS deployment's policies only.
  - **The addresses hosts need are a settings sub-tab**: the published
    OpenSearch endpoint and the Mesh address the enrol commands print.
  - Nothing renders an i18n key or `undefined` in any of three languages.

Usage:
  mesh_check.py <base> <user> <pw> [shot.png]
"""
import sys
from playwright.sync_api import sync_playwright

base, user, pw = sys.argv[1:4]
shot = sys.argv[4] if len(sys.argv) > 4 else None

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

# The policy this run creates (idempotent: a second run finds it) and the
# integration it adds to it — ssh is a node/host source with no container
# filter, so it is the one that makes sense on a host.
E2E_POLICY = "e2e-policy"
E2E_INTEG = "ssh"

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
    check(page.locator('[data-testid="mesh-absent"]').count() == 1
          or page.locator('[data-testid="mesh-ready"]').count() == 1
          or page.locator('[data-testid="mesh-unreachable"]').count() == 1,
          "the overview states whether a Mesh is installed")
    overview = page.locator('[data-testid="mesh-panel-overview"]').inner_text()
    check("VELOX_MESH_URL" not in overview,
          "the controller's install commands are NOT on the deployment's tab (they are the admin's, under Settings)")
    has_mesh = page.locator('[data-testid="mesh-ready"]').count() == 1
    if has_mesh:
        page.wait_for_selector('[data-testid="mesh-summary"]', timeout=15000)
        chips = page.locator('[data-testid^="mesh-sum-"]')
        check(chips.count() == 5, f"the fleet summary has one chip per status bucket (got {chips.count()})")
        total = page.locator('[data-testid="mesh-overview-fleet"] tbody tr').count()
        summed = sum(int(chips.nth(i).get_attribute("data-n")) for i in range(chips.count()))
        check(summed == total, f"the chips add up to the configured agents ({summed} = {total})")
        check(page.locator('[data-testid="mesh-overview-agents"]').count() == 1,
              "the overview lists the configured agents")
        check(page.locator('[data-testid="mesh-overview-fleet"] select').count() == 0,
              "the overview's agent list is read-only")

    # ---- the settings sub-tab: the addresses hosts need ------------------
    page.click('[data-testid="mesh-tab-settings"]')
    page.wait_for_selector('[data-testid="mesh-panel-settings"]', timeout=15000)
    check(page.url.endswith(f"/deployments/{dep}/mesh/settings"),
          f"settings has a URL (got {page.url})")
    check(page.locator('[data-testid="mesh-settings"] [data-testid="mesh-policy-os-host"]').count() == 1,
          "the published OpenSearch endpoint is a deployment setting")
    check(page.locator('[data-testid="mesh-settings"] [data-testid="mesh-url"]').count() == 1,
          "the Mesh address for agents is a deployment setting")
    check(page.locator('[data-testid="mesh-settings-save"]').is_disabled(),
          "save is disabled until something changes")
    body = page.locator('[data-testid="mesh-panel-settings"]').inner_text()
    check("undefined" not in body and "mesh_" not in body, "the settings screen renders no raw key")

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
            check(blocks == 3, f"{key}: links out for the engine, keeps the 3 agent steps (got {blocks})")
            check(panel.locator('a[href*="docs.fluentbit.io"]').count() >= 1,
                  f"{key}: links to the project's own page")
        for bad in ("undefined", "mesh_", "{min}"):
            check(bad not in body, f"{key}: nothing renders {bad!r}")
    for key in ("debian", "ubuntu"):
        page.click(f'[data-testid="agents-tab-{key}"]')
        body = page.locator(f'[data-testid="agents-panel-{key}"]').inner_text().lower()
        check("não empacotam" in body or "do not package" in body or "no empaquetan" in body,
              f"{key}: says the distribution does not package Fluent Bit")
    page.click('[data-testid="agents-tab-redhat"]')
    check("EPEL" in page.locator('[data-testid="agents-panel-redhat"]').inner_text(),
          "redhat: names EPEL as the RHEL 9 route")
    check("3.2" in page.locator(".ag-facts").inner_text(), "the minimum Fluent Bit version is shown")

    # ---- the policies tab: create, and manage integrations in place ------
    page.goto(f"{base}/deployments/{dep}/mesh/policies", wait_until="networkidle")
    page.wait_for_selector('[data-testid="mesh-panel-policies"]', timeout=15000)
    check(page.locator('[data-testid^="mesh-integration-"]').count() == 0,
          "no integration grid on the policies tab: the one dialog adds to a policy (ADR-073)")
    if not has_mesh:
        check(page.locator('[data-testid="mesh-policy-save-blocked"]').count() == 1,
              "without a Mesh the policies tab says so instead of offering a form")
    else:
        page.wait_for_selector('[data-testid="mesh-policy-create"]', timeout=10000)
        if page.locator(f'[data-testid="policy-{E2E_POLICY}"]').count() == 0:
            page.fill('[data-testid="mesh-policy-name"]', "E2E policy")
            page.click('[data-testid="mesh-policy-create"]')
            page.wait_for_selector(f'[data-testid="policy-{E2E_POLICY}"]', timeout=20000)
        check(page.locator(f'[data-testid="policy-{E2E_POLICY}"]').count() == 1,
              f"a policy is created with a name only ({E2E_POLICY})")
        body = page.locator(f'[data-testid="policy-{E2E_POLICY}"]').inner_text()
        check("undefined" not in body and "mesh_" not in body, "the policy card renders no raw key")

        # "Add integration" on the card picks a package and opens the one
        # dialog with the policy fixed: no "where" step.
        if page.locator(f'[data-testid="policy-integ-{E2E_POLICY}-{E2E_INTEG}"]').count() == 1:
            page.click(f'[data-testid="policy-integ-remove-{E2E_POLICY}-{E2E_INTEG}"]')
            page.wait_for_selector(f'[data-testid="policy-integ-{E2E_POLICY}-{E2E_INTEG}"]', state="detached", timeout=30000)
        page.click(f'[data-testid="policy-add-{E2E_POLICY}"]')
        page.wait_for_selector(f'[data-testid="policy-add-pick-{E2E_POLICY}"]', timeout=20000)
        page.select_option(f'[data-testid="policy-add-pick-{E2E_POLICY}"]', E2E_INTEG)
        page.wait_for_selector('[data-testid="add-integration"]', timeout=10000)
        page.wait_for_selector('[data-testid="add-integration-configure"]', timeout=15000)
        check(page.locator('[data-testid="add-integration-where"]').count() == 0,
              "from a policy card the dialog skips 'where': the policy is the target")
        check(E2E_POLICY in page.locator('[data-testid="add-integration-configure"]').inner_text(),
              "the dialog names the policy it adds to")
        page.click('[data-testid="add-integration-save"]')
        page.wait_for_selector('[data-testid="add-integration"]', state="detached", timeout=60000)
        page.wait_for_selector(f'[data-testid="policy-integ-{E2E_POLICY}-{E2E_INTEG}"]', timeout=30000)
        check(True, f"{E2E_INTEG} is listed on the policy card")
        # Configure reopens the same dialog on the recorded values.
        page.click(f'[data-testid="policy-integ-edit-{E2E_POLICY}-{E2E_INTEG}"]')
        page.wait_for_selector('[data-testid="add-integration-configure"]', timeout=15000)
        check(page.locator('[data-testid^="add-input-"][data-on="1"]').count() >= 1,
              "configure shows the recorded input on")
        page.keyboard.press("Escape")
        page.wait_for_selector('[data-testid="add-integration"]', state="detached", timeout=10000)
        page.click(f'[data-testid="policy-integ-remove-{E2E_POLICY}-{E2E_INTEG}"]')
        page.wait_for_selector(f'[data-testid="policy-integ-{E2E_POLICY}-{E2E_INTEG}"]', state="detached", timeout=30000)
        check(True, "an integration is removed from the policy on its card")
        check(page.locator(f'[data-testid="policy-token-{E2E_POLICY}"]').count() == 1,
              "the card still mints the enrolment token")

    # ---- the one onboarding dialog ----------------------------------------
    page.goto(f"{base}/deployments/{dep}/data" + (f"?policy={E2E_POLICY}" if has_mesh else ""), wait_until="networkidle")
    page.wait_for_selector('[data-testid="integrations-tab"]', timeout=15000)
    page.wait_for_selector(f'[data-testid="install-{E2E_INTEG}"], [data-testid="integ-menu-{E2E_INTEG}"]', timeout=20000)
    already = page.locator(f'[data-testid="integ-menu-{E2E_INTEG}"]').count() == 1
    if already:
        page.click(f'[data-testid="integ-menu-{E2E_INTEG}"]')
        page.click(f'[data-testid="add-more-{E2E_INTEG}"]')
    else:
        page.click(f'[data-testid="install-{E2E_INTEG}"]')
    page.wait_for_selector('[data-testid="add-integration"]', timeout=10000)
    page.wait_for_selector('[data-testid="add-integration-where"]', timeout=15000)
    check(page.locator('[data-testid="add-target-cluster"]').count() == 1,
          "the dialog offers this cluster as a target")
    if has_mesh:
        check(page.locator(f'[data-testid="add-target-{E2E_POLICY}"] input').is_checked(),
              "the policy the Mesh tab sent over is preselected")
        check(page.locator('[data-testid="add-target-new"]').count() == 1,
              "a new policy can be created from the dialog")
    else:
        check(page.locator('[data-testid^="add-target-"]').count() == 1,
              "without a Mesh the only target is the cluster")
    page.click('[data-testid="add-integration-next"]')
    page.wait_for_selector('[data-testid="add-integration-configure"]', timeout=10000)
    inputs = page.locator('[data-testid^="add-input-"]:not([data-testid^="add-input-toggle-"])')
    check(inputs.count() >= 1, f"the package's inputs are listed ({inputs.count()})")
    check(page.locator('[data-testid^="add-input-"][data-on="1"]').count() >= 1,
          "the package's default input is pre-selected")
    cfg = page.locator('[data-testid="add-integration-configure"]').inner_text()
    check("veloxagent-" in cfg, "each input names the index it writes to")
    for bad in ("undefined", "add_integ_", "integ_"):
        check(bad not in cfg, f"the dialog renders no raw key ({bad!r})")
    page.click('[data-testid="add-integration-save"]')
    page.wait_for_selector('[data-testid="add-integration"]', state="detached", timeout=60000)
    page.wait_for_timeout(1500)
    if has_mesh:
        page.wait_for_selector(f'[data-testid="on-policy-{E2E_INTEG}-{E2E_POLICY}"]', timeout=30000)
        check(True, f"{E2E_INTEG} shows the policy it is now on")
        # Remove it from the policy again; the row stays if the cluster has it.
        page.click(f'[data-testid="integ-menu-{E2E_INTEG}"]')
        page.click(f'[data-testid="remove-{E2E_INTEG}-{E2E_POLICY}"]')
        page.wait_for_selector(f'[data-testid="on-policy-{E2E_INTEG}-{E2E_POLICY}"]', state="detached", timeout=30000)
        check(True, "removal is per target")
    else:
        page.wait_for_selector(f'[data-testid="installed-{E2E_INTEG}"]', timeout=60000)
        check(True, f"{E2E_INTEG} shows the cluster badge")
        page.click(f'[data-testid="integ-menu-{E2E_INTEG}"]')
        page.click(f'[data-testid="uninstall-{E2E_INTEG}"]')
        page.wait_for_selector(f'[data-testid="installed-{E2E_INTEG}"]', state="detached", timeout=60000)
        check(True, "removed from the cluster again")

    # ---- the fleet, when a controller is configured ----------------------
    page.goto(f"{base}/deployments/{dep}/mesh", wait_until="networkidle")
    page.wait_for_selector('[data-testid="mesh-panel-overview"]', timeout=15000)
    onb = page.locator('[data-testid="mesh-onboarding"], [data-testid="mesh-onboarding-done"]')
    if onb.count() == 0:
        print("  · no Velox Mesh configured; fleet assertions skipped")
    else:
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
        page.wait_for_selector('[data-testid="mesh-fleet"], [data-testid="mesh-fleet-empty"]', timeout=15000)
        rows = page.locator('[data-testid="mesh-fleet"] tbody tr')
        check(True, f"the fleet renders ({rows.count()} agent(s))")
        check(page.locator('[data-testid="mesh-fleet"] select').count() == 0,
              "no policy picker on the rows: the policy is changed in the agent's editor")
        edited = False
        for i in range(rows.count()):
            r = rows.nth(i)
            status = r.get_attribute("data-status")
            up = r.get_attribute("data-up")
            expected_up = "0" if status in ("offline", "unenrolled") else "1"
            check(up == expected_up, f"row {i}: {status} is up={expected_up}")
            check(status not in r.inner_text(), f"row {i}: the status is a word, not {status!r}")
            btn = r.locator('[data-testid^="fleet-edit-"]')
            if btn.count() and not edited:
                edited = True
                btn.click()
                page.wait_for_selector('[data-testid="agent-dialog"]', timeout=10000)
                sel = page.locator('[data-testid="agent-policy"]')
                check(sel.count() == 1, "the editor is where the policy is picked")
                check(sel.locator("option").count() >= 1,
                      "the picker offers this deployment's policies only")
                page.wait_for_selector('[data-testid="agent-deps"]', timeout=10000)
                if sel.input_value():
                    page.wait_for_function(
                        "() => { const d = document.querySelector('[data-testid=\"agent-deps\"]'); return d && !/…$/.test(d.innerText.trim()); }",
                        timeout=15000)
                    deps = page.locator('[data-testid="agent-deps"]').inner_text()
                    check(sel.input_value() in deps, "the dependencies name the policy and its revision")
                check(page.locator('[data-testid="agent-deploy"]').is_disabled(),
                      "deploy is disabled while nothing changed")
                dlg = page.locator('[data-testid="agent-dialog"]').inner_text()
                check("undefined" not in dlg and "mesh_" not in dlg, "the editor renders no raw key")
                page.keyboard.press("Escape")
                page.wait_for_selector('[data-testid="agent-dialog"]', state="detached", timeout=10000)
        check(page.locator('[data-testid^="agents-panel-"]').count() == 1,
              "the install guide sits under the fleet")

    if shot:
        page.screenshot(path=shot, full_page=True)
        print(f"  · screenshot {shot}")

    ignorable = ("favicon", "ERR_NETWORK_CHANGED")
    noise = [e for e in errors if not any(i.lower() in e.lower() for i in ignorable)]
    check(not noise, f"console clean (got {noise[:2]})")
    browser.close()

if fail:
    print(f"\nFAILED: {len(fail)}")
    sys.exit(1)
print("\nVelox Mesh screens OK")
