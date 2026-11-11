#!/usr/bin/env python3
"""Velox Mesh — the deployment's fleet tab and the one integration onboarding
(ADR-068, ADR-069, ADR-072, ADR-073).

The journey is Elastic Fleet's, because operators already know it:

    configure endpoints -> create a collection policy
                        -> add integrations to the policy
                        -> add an agent on a host, with the policy's token

Since ADR-072 the Mesh is a DEPLOYMENT's: the screen is a tab of
`/deployments/:id/mesh/:sub`, every policy and agent on it belongs to that
deployment, and the controller's install guide is an admin matter under
Settings. Since ADR-073 an integration is ADDED TO A POLICY — the cluster
being the implicit one — from one dialog on the Data tab. Since ADR-072's
second amendment the deployment's settings hold NAMED ENDPOINTS, every
policy is bound to one OpenSearch and one Mesh endpoint, and every agent
runs a policy. Each assertion below guards a property that is easy to break
and expensive to notice:

  - **The Mesh lives under the deployment, not in the sidebar.** The old
    global `/mesh` redirects to the list; the tab has a URL and a deep link.
  - **The overview is the fleet, nothing else.** Status counts, one bar, the
    configured agents, read-only. The Mesh's state ("ready") and the guided
    first run live on the settings sub-tab; the overview only raises what
    blocks the fleet (no Mesh, Mesh down, endpoints missing).
  - **Settings is where the endpoints live, and they are required.** Two
    lists (OpenSearch host[:port], Mesh URL); the daemons table shows the
    controller and every configured Mesh address with reachability.
  - **A policy needs both endpoints to exist.** The create form has the two
    pickers and stays disabled without them; the card shows the binding and
    changes it (a new revision); an unbound legacy policy is flagged.
  - **An agent is added through a dialog that starts with the policy.** The
    install guide (whose tab set mirrors the Fluent Bit project's own
    downloads index, ADR-069; commands upstream's, verbatim) is INSIDE it,
    and the enrol command carries the policy's Mesh address and the minted
    token. No inline guide on the Agents tab.
  - **The Policies tab manages integrations through the one dialog.**
  - **One dialog for both targets** on the Data tab, removal per target.
  - **With a controller, the fleet is the product**: each agent row's
    up/down agrees with its derived status; no policy picker on the row; the
    editor shows the policy, its endpoints and its integrations, and deploys
    within THIS deployment's policies only.
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

# What this run creates (idempotent: a second run finds them): two endpoints,
# a policy bound to them, and ssh on that policy — a node/host source with no
# container filter, so it is the one that makes sense on a host.
E2E_OS = "e2e-os"
E2E_MESH = "e2e-mesh"
E2E_MESH_URL = "http://127.0.0.1:4320"
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

    # ---- the tab and its overview ------------------------------------------
    page.goto(f"{base}/deployments/{dep}/overview", wait_until="networkidle")
    page.wait_for_selector('[data-testid="dep-tab-mesh"]', timeout=15000)
    page.click('[data-testid="dep-tab-mesh"]')
    page.wait_for_selector('[data-testid="mesh-panel-overview"]', timeout=15000)
    check(page.url.endswith(f"/deployments/{dep}/mesh"),
          f"the tab lands on /deployments/{dep}/mesh (got {page.url})")
    page.wait_for_selector('[data-testid="mesh-summary"], [data-testid="mesh-absent"], [data-testid="mesh-unreachable"]', timeout=15000)
    has_mesh = page.locator('[data-testid="mesh-summary"]').count() == 1
    overview = page.locator('[data-testid="mesh-panel-overview"]').inner_text()
    check("VELOX_MESH_URL" not in overview,
          "the controller's install commands are NOT on the deployment's tab (they are the admin's, under Settings)")
    check(page.locator('[data-testid="mesh-ready"]').count() == 0,
          "'Mesh ready' is not on the overview (it lives on the settings sub-tab)")
    check(page.locator('[data-testid="mesh-onboarding"], [data-testid="mesh-onboarding-done"]').count() == 0,
          "the guided first run is not on the overview (it lives on the settings sub-tab)")
    check(page.locator('[data-testid^="agents-panel-"]').count() == 0,
          "no install guide on the overview")
    if has_mesh:
        chips = page.locator('[data-testid^="mesh-sum-"]')
        check(chips.count() == 5, f"the fleet summary has one chip per status bucket (got {chips.count()})")
        total = page.locator('[data-testid="mesh-overview-fleet"] tbody tr').count()
        summed = sum(int(chips.nth(i).get_attribute("data-n")) for i in range(chips.count()))
        check(summed == total, f"the chips add up to the configured agents ({summed} = {total})")
        check(page.locator('[data-testid="mesh-overview-agents"]').count() == 1,
              "the overview lists the configured agents")
        check(page.locator('[data-testid="mesh-overview-fleet"] select').count() == 0,
              "the overview's agent list is read-only")
        check(page.locator('[data-testid="mesh-seats"]').count() == 1,
              "seats are shown as the installation's, not the deployment's")

    # ---- the settings sub-tab: state, first run, daemons, endpoints -------
    page.click('[data-testid="mesh-tab-settings"]')
    page.wait_for_selector('[data-testid="mesh-panel-settings"]', timeout=15000)
    check(page.url.endswith(f"/deployments/{dep}/mesh/settings"),
          f"settings has a URL (got {page.url})")
    check(page.locator('[data-testid="mesh-absent"], [data-testid="mesh-ready"], [data-testid="mesh-unreachable"]').count() == 1,
          "the settings sub-tab states whether a Mesh is installed")
    check(page.locator('[data-testid="mesh-os-endpoints"]').count() == 1
          and page.locator('[data-testid="mesh-mesh-endpoints"]').count() == 1,
          "the OpenSearch and Mesh endpoint lists are deployment settings")
    page.wait_for_function("() => !document.querySelector('[data-testid=\"mesh-os-endpoints-name\"]').disabled", timeout=15000)
    check(page.locator('[data-testid="mesh-settings-save"]').is_disabled(),
          "save is disabled until something changes")
    check(page.locator('[data-testid="mesh-os-endpoints-suggest"]').count() == 1,
          "the in-cluster address is OFFERED as an endpoint (never assumed)")
    if has_mesh:
        onb = page.locator('[data-testid="mesh-onboarding"], [data-testid="mesh-onboarding-done"]')
        check(onb.count() == 1, "the guided first run is on the settings sub-tab")
        step = onb.first.get_attribute("data-step")
        if step:
            now = page.locator('.onb-step[data-state="now"]')
            check(now.count() == 1, f"exactly one step is live (at {step})")
            done = page.locator('.onb-step[data-state="ok"]').count()
            idx = ["install_mesh", "create_policy", "enrol_agent", "await_first_apply"].index(step)
            check(done == idx, f"the {idx} step(s) before it are done (got {done})")
        check(page.locator('[data-testid="mesh-daemons"]').count() == 1
              and page.locator('[data-testid="mesh-daemon-controller"]').count() == 1,
              "the daemons table lists the controller this console uses")
        # Make sure the e2e endpoints exist (idempotent).
        changed = False
        if page.locator(f'[data-testid="mesh-os-endpoints-row-{E2E_OS}"]').count() == 0:
            page.fill('[data-testid="mesh-os-endpoints-name"]', "E2E OS")
            page.fill('[data-testid="mesh-os-endpoints-addr"]', "10.43.207.250")
            page.click('[data-testid="mesh-os-endpoints-add"]')
            changed = True
        if page.locator(f'[data-testid="mesh-mesh-endpoints-row-{E2E_MESH}"]').count() == 0:
            page.fill('[data-testid="mesh-mesh-endpoints-name"]', "E2E Mesh")
            page.fill('[data-testid="mesh-mesh-endpoints-addr"]', E2E_MESH_URL)
            page.click('[data-testid="mesh-mesh-endpoints-add"]')
            changed = True
        if changed:
            check(page.locator('[data-testid="mesh-settings-save"]').is_enabled(), "adding an endpoint enables save")
            page.click('[data-testid="mesh-settings-save"]')
            page.wait_for_function("() => document.querySelector('[data-testid=\"mesh-settings-save\"]').disabled", timeout=20000)
        check(page.locator(f'[data-testid="mesh-os-endpoints-row-{E2E_OS}"]').count() == 1
              and page.locator(f'[data-testid="mesh-mesh-endpoints-row-{E2E_MESH}"]').count() == 1,
              "the e2e endpoints are configured (id = the name's slug)")
        page.wait_for_selector(f'[data-testid="mesh-daemon-{E2E_MESH}"][data-reachable="1"], [data-testid="mesh-daemon-{E2E_MESH}"][data-reachable="0"]', timeout=20000)
        check(page.locator(f'[data-testid="mesh-daemon-{E2E_MESH}"]').count() == 1,
              "a configured Mesh address is a daemon row with a probe result")
    body = page.locator('[data-testid="mesh-panel-settings"]').inner_text()
    check("undefined" not in body and "mesh_" not in body, "the settings screen renders no raw key")

    # ---- the policies tab: endpoints required, integrations in place -----
    page.goto(f"{base}/deployments/{dep}/mesh/policies", wait_until="networkidle")
    page.wait_for_selector('[data-testid="mesh-panel-policies"]', timeout=15000)
    check(page.locator('[data-testid^="mesh-integration-"]').count() == 0,
          "no integration grid on the policies tab: the one dialog adds to a policy (ADR-073)")
    if not has_mesh:
        check(page.locator('[data-testid="mesh-policy-save-blocked"]').count() == 1,
              "without a Mesh the policies tab says so instead of offering a form")
    else:
        page.wait_for_selector('[data-testid="mesh-policy-create"]', timeout=10000)
        check(page.locator('[data-testid="mesh-policy-os-endpoint"]').count() == 1
              and page.locator('[data-testid="mesh-policy-mesh-endpoint"]').count() == 1,
              "creating a policy asks for its OpenSearch and Mesh endpoints")
        if page.locator(f'[data-testid="policy-{E2E_POLICY}"]').count() == 0:
            page.fill('[data-testid="mesh-policy-name"]', "E2E policy")
            check(page.locator('[data-testid="mesh-policy-create"]').is_disabled()
                  or page.locator('[data-testid="mesh-policy-os-endpoint"]').input_value() != "",
                  "a name alone does not create: both endpoints are required")
            page.select_option('[data-testid="mesh-policy-os-endpoint"]', E2E_OS)
            page.select_option('[data-testid="mesh-policy-mesh-endpoint"]', E2E_MESH)
            page.click('[data-testid="mesh-policy-create"]')
            page.wait_for_selector(f'[data-testid="policy-{E2E_POLICY}"]', timeout=20000)
        check(page.locator(f'[data-testid="policy-{E2E_POLICY}"]').count() == 1,
              f"the policy exists ({E2E_POLICY})")
        # A policy from before the amendment has no endpoints: the card says
        # so, and binding it is a dialog that makes a new revision.
        eps = page.locator(f'[data-testid="policy-endpoints-{E2E_POLICY}"]')
        if eps.get_attribute("data-complete") != "1":
            check(page.locator(f'[data-testid="policy-endpoints-missing-{E2E_POLICY}"]').count() == 1,
                  "an unbound policy is flagged on its card")
            page.click(f'[data-testid="policy-endpoints-edit-{E2E_POLICY}"]')
            page.wait_for_selector('[data-testid="policy-endpoints-dialog"]', timeout=10000)
            page.select_option('[data-testid="policy-ep-os-endpoint"]', E2E_OS)
            page.select_option('[data-testid="policy-ep-mesh-endpoint"]', E2E_MESH)
            page.click('[data-testid="policy-ep-save"]')
            page.wait_for_selector('[data-testid="policy-endpoints-dialog"]', state="detached", timeout=30000)
            page.wait_for_selector(f'[data-testid="policy-endpoints-{E2E_POLICY}"][data-complete="1"]', timeout=30000)
        check(page.locator(f'[data-testid="policy-endpoints-{E2E_POLICY}"]').get_attribute("data-complete") == "1",
              "the card shows the policy bound to both endpoints")
        check(E2E_OS not in page.locator(f'[data-testid="policy-endpoints-{E2E_POLICY}"]').inner_text()
              or "10.43.207.250" in page.locator(f'[data-testid="policy-endpoints-{E2E_POLICY}"]').inner_text(),
              "the binding is shown by name and address, not by id")
        body = page.locator(f'[data-testid="policy-{E2E_POLICY}"]').inner_text()
        check("undefined" not in body and "mesh_" not in body, "the policy card renders no raw key")
        check(page.locator(f'[data-testid="policy-add-agent-{E2E_POLICY}"]').count() == 1
              and page.locator(f'[data-testid="policy-token-{E2E_POLICY}"]').count() == 0,
              "the card adds an agent through the dialog; no bare token button")

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
        page.click(f'[data-testid="policy-integ-edit-{E2E_POLICY}-{E2E_INTEG}"]')
        page.wait_for_selector('[data-testid="add-integration-configure"]', timeout=15000)
        check(page.locator('[data-testid^="add-input-"][data-on="1"]').count() >= 1,
              "configure shows the recorded input on")
        page.keyboard.press("Escape")
        page.wait_for_selector('[data-testid="add-integration"]', state="detached", timeout=10000)
        page.click(f'[data-testid="policy-integ-remove-{E2E_POLICY}-{E2E_INTEG}"]')
        page.wait_for_selector(f'[data-testid="policy-integ-{E2E_POLICY}-{E2E_INTEG}"]', state="detached", timeout=30000)
        check(True, "an integration is removed from the policy on its card")

    # ---- the agents tab: the fleet and the add-agent dialog ---------------
    page.goto(f"{base}/deployments/{dep}/mesh/agents", wait_until="networkidle")
    page.wait_for_selector('[data-testid="mesh-panel-agents"]', timeout=15000)
    check(page.locator('[data-testid^="agents-panel-"]').count() == 0,
          "no inline install guide on the Agents tab: it is behind 'Add agent'")
    if has_mesh:
        page.wait_for_selector('[data-testid="mesh-fleet"], [data-testid="mesh-fleet-empty"]', timeout=15000)
        page.click('[data-testid="mesh-add-agent"]')
        page.wait_for_selector('[data-testid="add-agent-dialog"]', timeout=10000)
        check(page.locator('[data-testid="add-agent-policy"]').count() == 1,
              "the dialog starts with the policy: every agent runs one")
        page.select_option('[data-testid="add-agent-policy"]', E2E_POLICY)
        page.wait_for_selector('[data-testid="add-agent-endpoints"][data-complete="1"]', timeout=10000)
        check(True, "the chosen policy's endpoints are shown")
        cmd = page.locator('[data-testid="enroll-cmd"]').inner_text()
        check(E2E_MESH_URL in cmd and "TOKEN" in cmd,
              f"the enrol command carries the policy's Mesh address and a token placeholder ({cmd.strip()})")
        page.click('[data-testid="add-agent-token"]')
        page.wait_for_selector('[data-testid="add-agent-token-value"]', timeout=20000)
        tok = page.locator('[data-testid="add-agent-token-value"] pre').inner_text().strip()
        cmd = page.locator('[data-testid="enroll-cmd"]').inner_text()
        check(tok and tok in cmd and "TOKEN" not in cmd, "minting the token completes the enrol command")
        tabs = page.locator('[data-testid^="agents-tab-"]')
        check(tabs.count() == len(TARGETS),
              f"{len(TARGETS)} install targets inside the dialog, mirroring the project's index (got {tabs.count()})")
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
        check("3.2" in page.locator('[data-testid="add-agent-dialog"] .ag-facts').inner_text(),
              "the minimum Fluent Bit version is shown")
        dlg = page.locator('[data-testid="add-agent-dialog"]').inner_text()
        check("undefined" not in dlg and "mesh_" not in dlg, "the add-agent dialog renders no raw key")
        page.click('[data-testid="add-agent-close"]')
        page.wait_for_selector('[data-testid="add-agent-dialog"]', state="detached", timeout=10000)

        # From a policy card too, with the policy preselected.
        page.goto(f"{base}/deployments/{dep}/mesh/policies", wait_until="networkidle")
        page.wait_for_selector(f'[data-testid="policy-add-agent-{E2E_POLICY}"]', timeout=15000)
        page.click(f'[data-testid="policy-add-agent-{E2E_POLICY}"]')
        page.wait_for_selector('[data-testid="add-agent-dialog"]', timeout=10000)
        check(page.locator('[data-testid="add-agent-policy"]').input_value() == E2E_POLICY,
              "a policy card opens the dialog with that policy chosen")
        page.keyboard.press("Escape")
        page.wait_for_selector('[data-testid="add-agent-dialog"]', state="detached", timeout=10000)

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
        page.wait_for_function("() => !document.querySelector('[data-testid=\"add-target-new\"] input').disabled", timeout=10000)
        check(True, "with endpoints configured the new-policy option is enabled")
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
    if has_mesh:
        page.goto(f"{base}/deployments/{dep}/mesh/agents", wait_until="networkidle")
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
                    page.wait_for_selector('[data-testid="agent-endpoints"]', timeout=15000)
                    deps = page.locator('[data-testid="agent-deps"]').inner_text()
                    check(sel.input_value() in deps, "the dependencies name the policy and its revision")
                    check(True, "the dependencies include the policy's endpoints")
                check(page.locator('[data-testid="agent-deploy"]').is_disabled(),
                      "deploy is disabled while nothing changed")
                dlg = page.locator('[data-testid="agent-dialog"]').inner_text()
                check("undefined" not in dlg and "mesh_" not in dlg, "the editor renders no raw key")
                page.keyboard.press("Escape")
                page.wait_for_selector('[data-testid="agent-dialog"]', state="detached", timeout=10000)

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
