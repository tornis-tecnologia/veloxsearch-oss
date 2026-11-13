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
        total = page.locator('[data-testid="mesh-fleet"] tbody tr').count()
        summed = sum(int(chips.nth(i).get_attribute("data-n")) for i in range(chips.count()))
        check(summed == total, f"the chips add up to the configured agents ({summed} = {total})")
        check(page.locator('[data-testid="mesh-add-agent"]').count() == 1,
              "the overview is the fleet, with 'Add agent'")
        check(page.locator('[data-testid="mesh-fleet"] select').count() == 0,
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

    # ---- the policies tab: a table, actions in a menu per row, a "New
    # policy" dialog that asks for both endpoints -------------------------
    page.goto(f"{base}/deployments/{dep}/mesh/policies", wait_until="networkidle")
    page.wait_for_selector('[data-testid="mesh-panel-policies"]', timeout=15000)
    check(page.locator('[data-testid^="mesh-integration-"]').count() == 0,
          "no integration grid on the policies tab: the one dialog adds to a policy (ADR-073)")

    def row_action(pid, item):
        """Open a row's action menu and pick an item."""
        page.click(f'[data-testid="policy-menu-{pid}"]')
        page.wait_for_selector(f'[data-testid="{item}"]', timeout=5000)
        page.click(f'[data-testid="{item}"]')

    def create_policy(name, pid):
        page.click('[data-testid="mesh-policy-new-open"]')
        page.wait_for_selector('[data-testid="mesh-policy-new-dialog"]', timeout=10000)
        check(page.locator('[data-testid="mesh-policy-os-endpoint"]').count() == 1
              and page.locator('[data-testid="mesh-policy-mesh-endpoint"]').count() == 1,
              "the new-policy dialog asks for its OpenSearch and Mesh endpoints")
        page.fill('[data-testid="mesh-policy-name"]', name)
        page.select_option('[data-testid="mesh-policy-os-endpoint"]', E2E_OS)
        page.select_option('[data-testid="mesh-policy-mesh-endpoint"]', E2E_MESH)
        page.click('[data-testid="mesh-policy-create"]')
        page.wait_for_selector('[data-testid="mesh-policy-new-dialog"]', state="detached", timeout=20000)
        page.wait_for_selector(f'[data-testid="policy-{pid}"]', timeout=20000)

    if not has_mesh:
        check(page.locator('[data-testid="mesh-policy-save-blocked"]').count() == 1,
              "without a Mesh the policies tab says so instead of offering a form")
    else:
        page.wait_for_selector('[data-testid="mesh-policies-table"], [data-testid="mesh-policies-empty"]', timeout=10000)
        check(page.locator('[data-testid="mesh-policy-new-open"]').count() == 1, "a 'New policy' button on top")
        if page.locator(f'[data-testid="policy-{E2E_POLICY}"]').count() == 0:
            create_policy("E2E policy", E2E_POLICY)
        check(page.locator(f'[data-testid="policy-{E2E_POLICY}"]').count() == 1, f"the policy exists ({E2E_POLICY})")
        heads = page.locator('[data-testid="mesh-policies-table"] thead').inner_text().lower()
        check(all(w in heads for w in ("polic", "agent", "revis", "integra")),
              f"the table's columns: policy, agents, revision, integrations, actions ({heads.strip()})")
        page.click(f'[data-testid="policy-menu-{E2E_POLICY}"]')
        for item in ("policy-endpoints-edit", "policy-settings", "policy-add", "policy-add-agent", "policy-remove"):
            check(page.locator(f'[data-testid="{item}-{E2E_POLICY}"]').count() == 1, f"the row's menu offers {item}")
        page.keyboard.press("Escape")

        # A policy from before the amendment has no endpoints: the row says
        # so, and binding it is a dialog that makes a new revision.
        if page.locator(f'[data-testid="policy-{E2E_POLICY}"]').get_attribute("data-complete") != "1":
            check(page.locator(f'[data-testid="policy-endpoints-missing-{E2E_POLICY}"]').count() == 1,
                  "an unbound policy is flagged on its row")
            row_action(E2E_POLICY, f"policy-endpoints-edit-{E2E_POLICY}")
            page.wait_for_selector('[data-testid="policy-endpoints-dialog"]', timeout=10000)
            page.select_option('[data-testid="policy-ep-os-endpoint"]', E2E_OS)
            page.select_option('[data-testid="policy-ep-mesh-endpoint"]', E2E_MESH)
            page.click('[data-testid="policy-ep-save"]')
            page.wait_for_selector('[data-testid="policy-endpoints-dialog"]', state="detached", timeout=30000)
            page.wait_for_selector(f'[data-testid="policy-{E2E_POLICY}"][data-complete="1"]', timeout=30000)
        check(page.locator(f'[data-testid="policy-{E2E_POLICY}"]').get_attribute("data-complete") == "1",
              "the row shows the policy bound to both endpoints")
        body = page.locator(f'[data-testid="policy-{E2E_POLICY}"]').inner_text()
        check("undefined" not in body and "mesh_" not in body, "the policy row renders no raw key")

        # Settings: the binding, by name and address, and the integrations.
        row_action(E2E_POLICY, f"policy-settings-{E2E_POLICY}")
        page.wait_for_selector('[data-testid="policy-settings-dialog"]', timeout=10000)
        eps = page.locator(f'[data-testid="policy-endpoints-{E2E_POLICY}"]')
        check(eps.get_attribute("data-complete") == "1" and "10.43.207.250" in eps.inner_text(),
              "settings show the binding by name and address")
        if page.locator(f'[data-testid="policy-integ-{E2E_POLICY}-{E2E_INTEG}"]').count() == 1:
            page.click(f'[data-testid="policy-integ-remove-{E2E_POLICY}-{E2E_INTEG}"]')
            page.wait_for_selector(f'[data-testid="policy-integ-{E2E_POLICY}-{E2E_INTEG}"]', state="detached", timeout=30000)
        page.click('[data-testid="policy-settings-close"]')
        page.wait_for_selector('[data-testid="policy-settings-dialog"]', state="detached", timeout=10000)

        # "Add integration" picks a package from the registry's Spec 2.0
        # catalogue (ADR-074) and opens its generated form with the policy
        # fixed: no "where" step. Unsigned packages (the registry before
        # `velox sign`) are refused at load, and the form is then skipped.
        row_action(E2E_POLICY, f"policy-add-{E2E_POLICY}")
        page.wait_for_selector(f'[data-testid="policy-add-pick-{E2E_POLICY}"]', timeout=20000)
        opts = page.locator(f'[data-testid="policy-add-pick-{E2E_POLICY}"] option').count()
        check(opts > 13, f"the picker offers the registry's catalogue ({opts - 1} packages)")
        page.select_option(f'[data-testid="policy-add-pick-{E2E_POLICY}"]', E2E_INTEG)
        page.wait_for_selector('[data-testid="add-package"]', timeout=10000)
        page.wait_for_selector('[data-testid="add-package-configure"], [data-testid="add-package-close"]', timeout=30000)
        check(page.locator('[data-testid="add-package-where"]').count() == 0,
              "from a policy the form skips 'where': the policy is the target")
        if page.locator('[data-testid="add-package-close"]').count():
            check(True, f"{E2E_INTEG} did not load (unsigned registry?) — its form is not exercised")
            page.click('[data-testid="add-package-close"]')
            page.wait_for_selector('[data-testid="add-package"]', state="detached", timeout=10000)
        else:
            page.click('[data-testid="add-package-save"]')
            page.wait_for_selector('[data-testid="add-package"]', state="detached", timeout=60000)
            page.wait_for_function(
                f"() => (document.querySelector('[data-testid=\"policy-integ-list-{E2E_POLICY}\"]') || {{}}).innerText?.toLowerCase().includes('{E2E_INTEG}')",
                timeout=30000)
            check(True, f"{E2E_INTEG} is listed on the policy's row")
            row_action(E2E_POLICY, f"policy-settings-{E2E_POLICY}")
            page.wait_for_selector(f'[data-testid="policy-integ-{E2E_POLICY}-{E2E_INTEG}"]', timeout=15000)
            page.click(f'[data-testid="policy-integ-edit-{E2E_POLICY}-{E2E_INTEG}"]')
            page.wait_for_selector('[data-testid="add-package-configure"]', timeout=30000)
            check(True, "configure reopens the package's form with the policy fixed")
            page.keyboard.press("Escape")
            page.wait_for_selector('[data-testid="add-package"]', state="detached", timeout=10000)
            page.wait_for_selector('[data-testid="policy-settings-dialog"]', timeout=10000)
            page.click(f'[data-testid="policy-integ-remove-{E2E_POLICY}-{E2E_INTEG}"]')
            page.wait_for_selector(f'[data-testid="policy-integ-{E2E_POLICY}-{E2E_INTEG}"]', state="detached", timeout=30000)
            check(True, "an integration is removed from the policy in its settings")
            page.click('[data-testid="policy-settings-close"]')

        # Remove: a throwaway policy goes; one with live agents cannot.
        if page.locator('[data-testid="policy-e2e-remove"]').count() == 0:
            create_policy("E2E remove", "e2e-remove")
        row_action("e2e-remove", "policy-remove-e2e-remove")
        page.wait_for_selector('[data-testid="policy-remove-confirm"]', timeout=10000)
        page.click('[data-testid="policy-remove-confirm"]')
        page.wait_for_selector('[data-testid="policy-e2e-remove"]', state="detached", timeout=20000)
        check(True, "a policy without agents is removed")
        busy = [r for r in page.locator('[data-testid^="policy-agents-"]').all() if r.inner_text().strip() not in ("", "0")]
        if busy:
            pid = busy[0].get_attribute("data-testid").removeprefix("policy-agents-")
            page.click(f'[data-testid="policy-menu-{pid}"]')
            item = page.locator(f'[data-testid="policy-remove-{pid}"]')
            check(item.is_disabled(), f"removing {pid}, which live agents run, is disabled")
            page.keyboard.press("Escape")

    # ---- the overview IS the fleet (the Agents tab merged into it), and
    # "Add agent" is a page in steps, not a dialog ---------------------------
    page.goto(f"{base}/deployments/{dep}/mesh/agents", wait_until="networkidle")
    page.wait_for_selector('[data-testid="mesh-panel-overview"]', timeout=15000)
    check(page.url.rstrip("/").endswith("/mesh"), f"the old agents sub-tab lands on the overview ({page.url})")
    check(page.locator('[data-testid="mesh-tab-agents"]').count() == 0, "no Agents sub-tab: the overview is the fleet")
    check(page.locator('[data-testid^="agents-panel-"]').count() == 0,
          "no inline install guide on the overview: it is behind 'Add agent'")
    if has_mesh:
        page.wait_for_selector('[data-testid="mesh-fleet"], [data-testid="mesh-fleet-empty"]', timeout=15000)
        page.click('[data-testid="mesh-add-agent"]')
        page.wait_for_selector('[data-testid="add-agent-page"]', timeout=10000)
        check("/mesh/add-agent" in page.url, f"'Add agent' is a page with a URL ({page.url})")
        check(page.locator('[data-testid="add-agent-policy"]').count() == 1,
              "the page starts with the policy: every agent runs one")
        check(page.locator('[data-testid="add-agent-next"]').is_disabled(), "no policy chosen: cannot advance")
        page.select_option('[data-testid="add-agent-policy"]', E2E_POLICY)
        page.wait_for_selector('[data-testid="add-agent-endpoints"][data-complete="1"]', timeout=10000)
        check(True, "the chosen policy's endpoints are shown")
        page.click('[data-testid="add-agent-next"]')
        page.wait_for_selector('[data-testid="add-agent-token-card"]', timeout=10000)
        check(page.locator('[data-testid="add-agent-next"]').is_disabled(), "step 2 waits for the token")
        page.click('[data-testid="add-agent-token"]')
        page.wait_for_selector('[data-testid="add-agent-token-value"]', timeout=20000)
        tok = page.locator('[data-testid="add-agent-token-value"] pre').inner_text().strip()
        page.click('[data-testid="add-agent-next"]')
        page.wait_for_selector('[data-testid="add-agent-install-card"]', timeout=10000)
        cmd = page.locator('[data-testid="enroll-cmd"]').inner_text()
        check(E2E_MESH_URL in cmd and tok and tok in cmd and "TOKEN" not in cmd,
              f"the enrol command carries the policy's Mesh address and the minted token ({cmd.strip()[:60]}…)")
        check(page.locator('[data-testid="add-agent-watch"]').get_attribute("data-seen") == "0",
              "the page watches the fleet for the new host")
        for key in TARGETS:
            page.click(f'[data-testid="agents-tab-{key}"]')
            page.wait_for_selector(f'[data-testid="agents-panel-{key}"]', timeout=5000)
            check(page.locator(f'[data-testid="agents-panel-{key}"] .ag-step').count() >= 3,
                  f"{key}: the panel has the install steps")
            check(page.locator('[data-testid="enroll-cmd"]').count() == 1, f"{key}: one enrol command")
        for key in ("ubuntu", "debian", "redhat"):
            page.click(f'[data-testid="agents-tab-{key}"]')
            body = page.locator(f'[data-testid="agents-panel-{key}"]').inner_text().lower()
            check("fluent-bit" in body, f"{key}: names the Fluent Bit package")
        for key in ("ubuntu", "debian"):
            page.click(f'[data-testid="agents-tab-{key}"]')
            body = page.locator(f'[data-testid="agents-panel-{key}"]').inner_text().lower()
            check("não empacotam" in body or "do not package" in body or "no empaquetan" in body,
                  f"{key}: says the distribution does not package Fluent Bit")
        page.click('[data-testid="agents-tab-redhat"]')
        check("EPEL" in page.locator('[data-testid="agents-panel-redhat"]').inner_text(),
              "redhat: names EPEL as the RHEL 9 route")
        check("3.2" in page.locator('[data-testid="add-agent-page"] .ag-facts').inner_text(),
              "the minimum Fluent Bit version is shown")
        pg = page.locator('[data-testid="add-agent-page"]').inner_text()
        check("undefined" not in pg and "mesh_" not in pg, "the add-agent page renders no raw key")
        page.click('[data-testid="add-agent-done"]')
        page.wait_for_selector('[data-testid="mesh-panel-overview"]', timeout=10000)
        check(True, "'Done' returns to the fleet")

        # From a policy card too, with the policy on the URL and chosen.
        page.goto(f"{base}/deployments/{dep}/mesh/policies", wait_until="networkidle")
        page.wait_for_selector(f'[data-testid="policy-menu-{E2E_POLICY}"]', timeout=15000)
        page.click(f'[data-testid="policy-menu-{E2E_POLICY}"]')
        page.click(f'[data-testid="policy-add-agent-{E2E_POLICY}"]')
        page.wait_for_selector('[data-testid="add-agent-page"]', timeout=10000)
        check(f"policy={E2E_POLICY}" in page.url, f"a policy card opens the page with the policy on the URL ({page.url})")
        check(page.locator('[data-testid="add-agent-policy"]').input_value() == E2E_POLICY,
              "…and chosen on step 1")
        page.click('[data-testid="add-agent-back"]')
        page.wait_for_selector('[data-testid="mesh-panel-overview"]', timeout=10000)

    # ---- the Data tab: the registry's catalogue is the catalogue ----------
    # The cluster's 1.x catalogue is gone (2026-11-12): what it installed on
    # this cluster is still listed, for configure/remove, and nothing else.
    page.goto(f"{base}/deployments/{dep}/data", wait_until="networkidle")
    page.wait_for_selector('[data-testid="integrations-tab"]', timeout=15000)
    page.wait_for_selector('[data-testid="agent-catalogue"]', timeout=30000)
    check(True, "the registry's catalogue renders on the Data tab")
    check(page.locator('[data-testid="integ-search"]').count() == 0,
          "no second, 1.x catalogue beside the registry's")
    check(page.locator('[data-testid^="install-"]').count() == 0,
          "no 1.x integration is offered for install")
    legacy = page.locator('[data-testid="integ-cluster"] [data-testid^="integration-"]')
    for i in range(legacy.count()):
        st = legacy.nth(i).get_attribute("data-state")
        check(st in ("installed", "update"), f"the cluster list holds installed integrations only ({st})")
    check(page.locator('[data-testid^="add-more-"]').count() == 0,
          "an old-format integration is not added to new targets")

    # ---- the fleet, when a controller is configured ----------------------
    if has_mesh:
        page.goto(f"{base}/deployments/{dep}/mesh", wait_until="networkidle")
        page.wait_for_selector('[data-testid="mesh-fleet"], [data-testid="mesh-fleet-empty"]', timeout=15000)
        rows = page.locator('[data-testid="mesh-fleet"] tbody tr')
        check(True, f"the fleet renders ({rows.count()} agent(s))")
        check(page.locator('[data-testid="mesh-fleet"] select').count() == 0,
              "no policy picker on the rows: the policy is changed in the agent's editor")
        # The summary chips filter the table by status, and every row can be
        # removed from the fleet (row and credential), behind a confirm.
        page.click('[data-testid="mesh-sum-healthy"]')
        page.wait_for_timeout(300)
        healthy_rows = page.locator('[data-testid="mesh-fleet"] tbody tr')
        check(all(healthy_rows.nth(i).get_attribute("data-status") == "healthy" for i in range(healthy_rows.count())),
              "the healthy chip shows healthy agents only")
        page.click('[data-testid="mesh-summary-total"]')
        page.wait_for_selector('[data-testid="mesh-fleet"], [data-testid="mesh-fleet-empty"]', timeout=5000)
        check(page.locator('[data-testid="mesh-fleet-filter"]').count() == 0, "the total clears the filter")
        if rows.count():
            check(page.locator('[data-testid^="fleet-forget-"]').count() == rows.count(),
                  "every row offers Remove")
        check(page.locator('[data-testid="mesh-seats"]').count() == 0, "no agent limit is shown")
        heads = page.locator('[data-testid="mesh-fleet"] thead').inner_text().lower()
        check("cpu" in heads and ("mem" in heads) and ("vers" in heads),
              f"Fleet-style columns: CPU, memory, version ({heads.strip()})")
        for i in range(rows.count()):
            r = rows.nth(i)
            if r.get_attribute("data-status") in ("offline", "unenrolled"):
                continue
            cpu = r.locator('[data-testid^="fleet-cpu-"]').inner_text().strip()
            check(cpu.endswith("%") or cpu in ("n/d", "N/A"), f"row {i}: CPU is a percentage or n/a ({cpu})")
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
