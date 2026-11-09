#!/usr/bin/env python3
"""The Agents screen — the per-distribution host install guide (ADR-069).

VeloxSearch deliberately does NOT ship the collection engines: the operator
installs Fluent Bit from their distribution's or the project's repository and
the Velox Agent supervises what it finds. That makes this screen part of the
product, not decoration — an install procedure nobody can find is a procedure
nobody follows — so it is checked like any other screen.

What this asserts, and why each one is a real regression:
  - the sidebar reaches it and the URL is /agents (ADR-067: every screen has
    a URL);
  - every distribution has a tab, and switching tabs swaps the panel;
  - each tab renders the full six-step procedure with a command block per
    step — a step whose command silently went missing would otherwise look
    like a styling change;
  - the commands are the UPSTREAM ones, verbatim: the repository URL for that
    distribution's family appears in the panel. A paraphrased shell command is
    a support ticket;
  - nothing renders an i18n key or `undefined` (the usual cost of a new
    screen in three languages);
  - Debian and Ubuntu carry the caveat that they do not package Fluent Bit,
    and RHEL carries the EPEL one. These are the two facts an operator cannot
    discover from our screen being silent.

Usage:
  agents_check.py <base> <user> <pw> [shot.png]
"""
import sys
from playwright.sync_api import sync_playwright

base, user, pw = sys.argv[1:4]
shot = sys.argv[4] if len(sys.argv) > 4 else None

# key -> (a string that must appear in that tab's panel, how many steps).
# The strings are the upstream repository paths (docs.fluentbit.io), so a
# well-meant edit that "tidies" a command into something that does not work
# fails here.
#
# Fedora has FIVE steps, not six: it is the one distribution that packages
# Fluent Bit itself, so the "add the repository" step is absent rather than
# present-and-pointless. That asymmetry is the screen telling the truth about
# each distribution, so it is asserted rather than smoothed over.
TABS = {
    "debian": ("packages.fluentbit.io/debian", 6),
    "ubuntu": ("packages.fluentbit.io/ubuntu", 6),
    "rhel": ("packages.fluentbit.io/centos", 6),
    "rocky": ("packages.fluentbit.io/almalinux", 6),
    "amazon": ("packages.fluentbit.io/amazonlinux", 6),
    "fedora": ("dnf install fluent-bit", 5),
    "raspbian": ("packages.fluentbit.io/raspbian", 6),
    "windows": ("fluent-bit", 6),
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
    page = browser.new_page(viewport={"width": 1440, "height": 1000})
    errors = []
    page.on("console", lambda m: errors.append(m.text) if m.type == "error" else None)
    page.on("pageerror", lambda e: errors.append(str(e)))

    page.goto(f"{base}/login", wait_until="networkidle")
    if page.locator('[data-testid="nav-agents"]').count() == 0:
        page.fill('input[name="username"], input[type="text"]', user)
        page.fill('input[type="password"]', pw)
        page.click('button[type="submit"]')
        page.wait_for_load_state("networkidle")

    page.click('[data-testid="nav-agents"]')
    page.wait_for_selector('[data-testid="agents-panel-debian"]', timeout=15000)
    check(page.url.rstrip("/").endswith("/agents"), f"the sidebar lands on /agents (got {page.url})")

    tabs = page.locator('[data-testid^="agents-tab-"]')
    check(tabs.count() == len(TABS), f"{len(TABS)} distribution tabs (got {tabs.count()})")

    for key, (needle, steps) in TABS.items():
        page.click(f'[data-testid="agents-tab-{key}"]')
        panel = page.locator(f'[data-testid="agents-panel-{key}"]')
        panel.wait_for(timeout=5000)
        body = panel.inner_text()
        check(panel.locator(".ag-step").count() == steps,
              f"{key}: {steps} steps (got {panel.locator('.ag-step').count()})")
        check(panel.locator(".cmdblock .codeblock").count() >= steps,
              f"{key}: a command block per step")
        check(needle in body, f"{key}: the upstream command is verbatim ({needle})")
        for bad in ("undefined", "agents_", "{min}"):
            check(bad not in body, f"{key}: nothing renders {bad!r}")

    # The two caveats that only exist because we checked what distributions
    # actually package: Debian/Ubuntu have no fluent-bit at all, RHEL 9 has no
    # upstream build and needs EPEL.
    for key in ("debian", "ubuntu"):
        page.click(f'[data-testid="agents-tab-{key}"]')
        body = page.locator(f'[data-testid="agents-panel-{key}"]').inner_text().lower()
        check("fluent bit" in body and ("não empacotam" in body or "do not package" in body
                                        or "no empaquetan" in body),
              f"{key}: says the distribution does not package Fluent Bit")

    page.click('[data-testid="agents-tab-rhel"]')
    rhel = page.locator('[data-testid="agents-panel-rhel"]').inner_text()
    check("EPEL" in rhel, "rhel: names EPEL as the RHEL 9 route")

    # The minimum version is the product fact this screen carries (ADR-069).
    check("3.2" in page.locator(".ag-facts").inner_text(), "the minimum Fluent Bit version is shown")

    if shot:
        page.screenshot(path=shot, full_page=True)
        print(f"  · screenshot {shot}")

    noise = [e for e in errors if "favicon" not in e.lower()]
    check(not noise, f"console clean (got {noise[:2]})")
    browser.close()

if fail:
    print(f"\nFAILED: {len(fail)}")
    sys.exit(1)
print("\nagents screen OK")
