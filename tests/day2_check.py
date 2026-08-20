#!/usr/bin/env python3
"""Day-2 operations functional validation (#52).

Exercises every supported day-2 operation on a deployment against a LIVE
VeloxSearch install (app + real cluster), and proves each UNSUPPORTED
operation is refused with an informative error instead of a broken apply:

  supported (must succeed and be reflected in the deployment spec):
    - disk resize, GROW only          (save_cluster, ADR-031)
    - node scaling up and back down   (save_cluster)
    - memory up and back down         (save_cluster)
    - extra OpenSearch dashboard      (apply_recipe on a running deployment —
                                       imports the recipe's dashboard objects)
    - admin password change           (reset_admin_password, #44; proven
                                       end-to-end by monitoring_status working
                                       again with the NEW credentials)

  unsupported (must be refused, message asserted):
    - disk shrink                     ("cannot shrink"; PVCs can't shrink)
    - scale to 0 nodes                (points at delete instead)
    - garbage node count / quantities ("abc", "4GB" — refused, not defaulted)
    - memory outside the operator-heap bounds (1Gi floor / 62Gi ceiling —
                                       heap = memory/2 is operator-derived,
                                       #55/ADR-035)
    - weak admin password (<8 chars)
    - password reset on a deployment that doesn't exist (existence-first)

  The namespace-first guard (missing app namespace → actionable refusal)
  cannot be exercised here without breaking the live install; it is enforced
  in src/k8s.rs::ensure_namespace_exists and exercised by the conformance
  fleet's greenfield lanes where the namespace genuinely doesn't exist yet.

Needs a cluster: run it from CI/an operator box that can reach the app (the
app itself talks to the cluster). Stdlib only — no Playwright, no requests.

Usage:
  day2_check.py <base_url> <user> <pw>                  # creates + deletes 'day2'
  day2_check.py <base_url> <user> <pw> --deployment X   # reuse existing (kept)
  day2_check.py <base_url> <user> <pw> --keep           # keep the created one
  day2_check.py ... --green-timeout 900 --reset-timeout 300
"""
import http.cookiejar
import json
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

_JAR = http.cookiejar.CookieJar()
_OPENER = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(_JAR))
BASE = None
PASSED = []


def fail(msg):
    print("FAIL:", msg)
    sys.exit(1)


def ok(what):
    PASSED.append(what)
    print("  ok:", what)


def api(path, body=None):
    """POST (or GET when body is None) /api/<path> → (status, parsed-json)."""
    url = f"{BASE}/api/{path}"
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(url, data=data)
    if data is not None:
        req.add_header("Content-Type", "application/json")
    try:
        with _OPENER.open(req, timeout=60) as r:
            raw = r.read()
            return r.status, (json.loads(raw) if raw.strip() else None)
    except urllib.error.HTTPError as e:
        raw = e.read()
        try:
            return e.code, json.loads(raw)
        except ValueError:
            return e.code, {"error": raw.decode(errors="replace")}


def expect_refusal(what, path, body, *needles):
    """The op must be REFUSED (>=400) with every needle in the error text."""
    status, resp = api(path, body)
    if status < 400:
        fail(f"{what}: expected a refusal, got HTTP {status} {resp}")
    err = (resp or {}).get("error", "")
    for n in needles:
        if n.lower() not in err.lower():
            fail(f"{what}: refusal lacks '{n}' — not informative: {err!r}")
    ok(f"{what} → refused: {err[:90]}")


def get_deployment(name):
    status, d = api("get_deployment", {"name": name})
    if status != 200:
        fail(f"get_deployment {name}: HTTP {status} {d}")
    return d


def save(d, **changes):
    """Echo the deployment's current spec through save_cluster with changes."""
    body = _echo(d)
    body.update(changes)
    return api("save_cluster", body)


def save_reflected(name, what, field, want, **changes):
    """A supported op: save must 200 and the spec must reflect the change."""
    d = get_deployment(name)
    status, resp = save(d, **changes)
    if status != 200:
        fail(f"{what}: HTTP {status} {resp}")
    deadline = time.time() + 120
    while time.time() < deadline:
        cur = get_deployment(name)
        if cur and str(cur.get(field)) == str(want):
            ok(f"{what} → applied ({field}={want})")
            return
        time.sleep(5)
    fail(f"{what}: accepted but {field} never became {want!r}")


def wait_green(name, secs):
    deadline = time.time() + secs
    while time.time() < deadline:
        d = get_deployment(name)
        if d and d.get("health") == "green":
            return
        time.sleep(15)
    fail(f"{name} not green within {secs}s — cannot validate recipe/password ops")


def grow(qty, extra_gi):
    n, unit = int("".join(filter(str.isdigit, qty))), qty.lstrip("0123456789")
    return f"{n + extra_gi}{unit}", f"{max(n - 1, 1)}{unit}"


def main():
    global BASE
    args = sys.argv[1:]
    if len(args) < 3:
        print(__doc__)
        sys.exit(2)
    BASE, user, pw = args[0].rstrip("/"), args[1], args[2]

    def opt(flag, default=None):
        return args[args.index(flag) + 1] if flag in args else default

    reuse = opt("--deployment")
    keep = "--keep" in args or reuse is not None
    green_timeout = int(opt("--green-timeout", "900"))
    reset_timeout = int(opt("--reset-timeout", "300"))

    status, resp = api("login", {"username": user, "password": pw})
    if status != 200:
        fail(f"login: HTTP {status} {resp}")
    ok("login")

    if reuse:
        name = reuse
        if get_deployment(name) is None:
            fail(f"--deployment {name}: no such deployment")
    else:
        status, name = api("create_cluster", {
            "name": "day2", "size": "small", "purpose": "observability",
            "nodes": "", "memory": "", "disk": "", "config": "", "monitors": None,
        })
        if status != 200:
            fail(f"create_cluster: HTTP {status} {name}")
        ok(f"created deployment {name}")

    d = get_deployment(name)
    if d is None:
        fail(f"{name} not visible after create")
    base_mem, base_disk, base_nodes = d["memory"], d["disk"], d["replicas"]
    bigger_disk, smaller_disk = grow(base_disk, 5)

    # ── unsupported ops: refusals BEFORE anything is mutated ────────────────
    expect_refusal("disk shrink", "save_cluster", {
        **_echo(d), "disk": smaller_disk}, "shrink")
    expect_refusal("scale to 0 nodes", "save_cluster", {
        **_echo(d), "nodes": "0"}, "0 nodes", "delete")
    expect_refusal("garbage node count", "save_cluster", {
        **_echo(d), "nodes": "abc"}, "abc")
    expect_refusal("non-Kubernetes memory quantity", "save_cluster", {
        **_echo(d), "memory": "4GB"}, "4GB")
    # Memory bounds (#55/ADR-035): the operator derives heap = memory/2, so the
    # floor is 1Gi (heap 512Mi) and the ceiling 62Gi (heap 31g).
    expect_refusal("memory below operator-heap floor", "save_cluster", {
        **_echo(d), "memory": "512Mi"}, "1Gi")
    expect_refusal("memory above operator-heap ceiling", "save_cluster", {
        **_echo(d), "memory": "100Gi"}, "62Gi")
    expect_refusal("weak admin password", "reset_admin_password", {
        "name": name, "new_password": "short"}, "8 characters")
    expect_refusal("password reset on missing deployment", "reset_admin_password", {
        "name": "day2-does-not-exist", "new_password": "LongEnough1_"},
        "no deployment")
    for f in ("memory", "disk", "replicas"):
        if str(get_deployment(name)[f]) != str({"memory": base_mem, "disk": base_disk,
                                                "replicas": base_nodes}[f]):
            fail(f"a refused op still mutated the spec ({f} changed)")
    ok("refused ops left the spec untouched")

    # ── supported ops: spec-level (operator reconciles in the background) ───
    mem_up, _ = grow(base_mem, 1)
    save_reflected(name, f"memory up {base_mem}→{mem_up}", "memory", mem_up,
                   memory=mem_up)
    save_reflected(name, f"memory back down →{base_mem}", "memory", base_mem,
                   memory=base_mem)
    nodes_up = base_nodes + 1
    save_reflected(name, f"scale up {base_nodes}→{nodes_up} nodes", "replicas",
                   nodes_up, nodes=str(nodes_up))
    save_reflected(name, f"scale back down →{base_nodes}", "replicas", base_nodes,
                   nodes=str(base_nodes))
    # Disk GROW: legal only when the default StorageClass can expand volumes —
    # on a non-expandable class the guard must refuse INFORMATIVELY instead of
    # applying a change the CSI would silently ignore. Both outcomes pass.
    dd = get_deployment(name)
    status, resp = save(dd, disk=bigger_disk)
    if status == 200:
        save_reflected(name, f"disk grow {base_disk}→{bigger_disk} (reconfirm)",
                       "disk", bigger_disk, disk=bigger_disk)
    else:
        err = (resp or {}).get("error", "")
        if "allowvolumeexpansion" not in err.lower():
            fail(f"disk grow: refused without naming the SC limitation: {err!r}")
        ok(f"disk grow → honestly refused (SC can't expand): {err[:80]}")

    # ── ops that need OpenSearch answering: extra dashboard + password ──────
    wait_green(name, green_timeout)
    ok("deployment green")

    recipe = "k8s-events"
    if recipe in get_deployment(name).get("monitors", []):
        recipe = "postgres"  # any not-yet-enabled recipe imports its dashboard
    status, resp = api("apply_recipe", {"deployment": name, "recipe": recipe})
    if status != 200:
        fail(f"extra dashboard (apply_recipe {recipe}): HTTP {status} {resp}")
    if recipe not in get_deployment(name).get("monitors", []):
        fail(f"recipe {recipe} applied but not recorded in monitors")
    status, resp = api("monitoring_status", {"deployment": name, "recipe": recipe})
    if status != 200:
        fail(f"monitoring_status after {recipe}: HTTP {status} {resp}")
    ok(f"extra dashboard via recipe '{recipe}' (applied + status readable)")

    new_pw = "Day2-Check-" + str(int(time.time()))
    status, resp = api("reset_admin_password", {"name": name, "new_password": new_pw})
    if status != 200:
        fail(f"password change: HTTP {status} {resp}")
    status, creds = api("dashboard_credentials", {"name": name})
    if status != 200 or creds.get("password") != new_pw:
        fail(f"password change not reflected in credentials: {status} {creds}")
    # End-to-end proof: the app talks to OpenSearch with the stored creds, so
    # monitoring_status succeeding again means the operator re-seeded the hash.
    deadline = time.time() + reset_timeout
    while True:
        status, _ = api("monitoring_status", {"deployment": name, "recipe": recipe})
        if status == 200:
            break
        if time.time() > deadline:
            fail(f"new admin password never became usable within {reset_timeout}s")
        time.sleep(10)
    ok("admin password change (stored + usable against OpenSearch)")

    if not keep:
        status, _ = api("delete_cluster", {"name": name})
        if status != 200:
            fail(f"cleanup delete_cluster: HTTP {status}")
        ok(f"deleted {name}")

    print(f"PASS: {len(PASSED)} day-2 checks")


def _echo(d):
    return {
        "name": d["name"], "size": d["size"], "purpose": d["purpose"],
        "nodes": str(d["replicas"]), "memory": d["memory"], "disk": d["disk"],
        "config": d.get("extra_config", ""),
        "monitors": ",".join(d.get("monitors", [])),
    }


if __name__ == "__main__":
    main()
