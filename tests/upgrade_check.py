#!/usr/bin/env python3
"""N-1 -> N upgrade contract check (#54, ADR-057).

Drives the upgrade lane in .github/workflows/upgrade.yml. The contract: rolling
out a new VeloxSearch changes the VeloxSearch control plane and nothing it
manages. This script holds the steps that need logic; the workflow does the
kubectl/minikube plumbing between them.

It talks to the app over HTTP (stdlib, same shape as day2_check.py) and to the
cluster through `kubectl` on PATH. It is written for a THROWAWAY CI cluster:
it creates an admin account, a deployment, and reads cluster-wide objects.
Never point it at a cluster you care about.

Subcommands:
  prepare  <base> <user> <pw> <state.json>
      First run on the N-1 install: create the admin, drive the bootstrap the
      way the SPA does, create a small deployment, wait for green. Records the
      deployment name in <state.json>.
  snapshot <out.json>
      Record what the upgrade must not change: the operator Deployment (.spec,
      image), cert-manager Deployments, operator + cert-manager CRDs (.spec),
      every OpenSearchCluster .spec, and the `veloxsearch-bootstrap` field
      manager's entries on each of them.
  compare  <before.json> <after.json>
      Fail on any difference in the above.
  green    <base> <user> <pw> <state.json> [--timeout S]
      Log in and wait for the recorded deployment to be green.
  binding  gone|present [--timeout S]
      Wait until the `veloxsearch-bootstrap` ClusterRoleBinding is gone, or
      assert it is still present.
  ensure-noop <base> <user> <pw> [--observe S]
      With the operator scaled to 0: bootstrap must report the operator as
      installed-but-not-ready, and a bootstrap_ensure (what the SPA sends) must
      apply nothing — the operator Deployment stays at 0 replicas for the whole
      observation window.
"""
import http.cookiejar
import json
import subprocess
import sys
import time
import urllib.error
import urllib.request

APP_NS = "veloxsearch-system"
OPERATOR = "opensearch-operator"
CERT_MANAGER_NS = "cert-manager"
CERT_MANAGER_DEPLOYS = ["cert-manager", "cert-manager-webhook", "cert-manager-cainjector"]
BOOTSTRAP_MANAGER = "veloxsearch-bootstrap"
CRD_GROUPS = ("opensearch.org", "opensearch.opster.io", "cert-manager.io")

_JAR = http.cookiejar.CookieJar()
_OPENER = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(_JAR))
BASE = None


def fail(msg):
    print("FAIL:", msg)
    sys.exit(1)


def ok(msg):
    print("  ok:", msg)


def opt(args, flag, default):
    return args[args.index(flag) + 1] if flag in args else default


# ── app API ──────────────────────────────────────────────────────────────────

def api(path, body=None, timeout=60):
    """POST (or GET when body is None) /api/<path> -> (status, parsed-json)."""
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(f"{BASE}/api/{path}", data=data)
    if data is not None:
        req.add_header("Content-Type", "application/json")
    try:
        with _OPENER.open(req, timeout=timeout) as r:
            raw = r.read()
            return r.status, (json.loads(raw) if raw.strip() else None)
    except urllib.error.HTTPError as e:
        raw = e.read()
        try:
            return e.code, json.loads(raw)
        except ValueError:
            return e.code, {"error": raw.decode(errors="replace")}
    except (urllib.error.URLError, OSError) as e:
        return 0, {"error": str(e)}


def wait_up(secs=600):
    deadline = time.time() + secs
    while time.time() < deadline:
        status, _ = api("auth_state")
        if status == 200:
            return
        time.sleep(5)
    fail(f"the app never answered /api/auth_state within {secs}s")


def login(user, pw):
    wait_up()
    status, state = api("auth_state")
    if state and state.get("first_run"):
        status, resp = api("setup_admin", {"username": user, "password": pw, "confirm": pw})
        if status != 200:
            fail(f"setup_admin: HTTP {status} {resp}")
        ok("admin created (first run)")
        return
    status, resp = api("login", {"username": user, "password": pw})
    if status != 200:
        fail(f"login: HTTP {status} {resp}")
    ok("logged in")


def wait_green(name, secs):
    deadline = time.time() + secs
    last = None
    while time.time() < deadline:
        status, d = api("get_deployment", {"name": name})
        if status == 200 and d:
            last = d.get("health")
            if last == "green":
                ok(f"{name} is green")
                return
        time.sleep(15)
    fail(f"{name} not green within {secs}s (last health: {last})")


# ── cluster reads ────────────────────────────────────────────────────────────

def kubectl_json(*args):
    out = subprocess.run(["kubectl", *args, "-o", "json"], check=True,
                         capture_output=True, text=True).stdout
    return json.loads(out)


def bootstrap_entries(obj):
    """The veloxsearch-bootstrap field manager's managedFields entries.

    Objects VeloxSearch installed on first bootstrap legitimately carry this
    manager already, so "absent" is not the checkable claim. "Unchanged" is: an
    apply of the bundle during the upgrade would re-stamp or extend them.
    """
    return sorted(
        (e for e in obj["metadata"].get("managedFields", [])
         if e.get("manager") == BOOTSTRAP_MANAGER),
        key=lambda e: json.dumps(e, sort_keys=True),
    )


def record(obj, **extra):
    return {"spec": obj.get("spec"), "bootstrap_managed": bootstrap_entries(obj), **extra}


def snapshot():
    snap = {}
    op = kubectl_json("-n", APP_NS, "get", "deployment", OPERATOR)
    images = [c.get("image") for c in op["spec"]["template"]["spec"]["containers"]]
    snap[f"deployment/{APP_NS}/{OPERATOR}"] = record(op, images=images)
    for d in CERT_MANAGER_DEPLOYS:
        obj = kubectl_json("-n", CERT_MANAGER_NS, "get", "deployment", d)
        snap[f"deployment/{CERT_MANAGER_NS}/{d}"] = record(obj)
    for crd in kubectl_json("get", "customresourcedefinitions")["items"]:
        if crd["spec"]["group"] in CRD_GROUPS:
            snap[f"crd/{crd['metadata']['name']}"] = record(crd)
    for cr in kubectl_json("get", "opensearchclusters.opensearch.org", "-A")["items"]:
        m = cr["metadata"]
        snap[f"opensearchcluster/{m['namespace']}/{m['name']}"] = record(cr)
    if not any(k.startswith("opensearchcluster/") for k in snap):
        fail("snapshot found no OpenSearchCluster — the lane has nothing to protect")
    return snap


def first_difference(a, b, path=""):
    if type(a) is not type(b):
        return path or "/"
    if isinstance(a, dict):
        for k in sorted(set(a) | set(b)):
            if k not in a or k not in b:
                return f"{path}/{k}"
            d = first_difference(a[k], b[k], f"{path}/{k}")
            if d:
                return d
        return None
    if isinstance(a, list):
        if len(a) != len(b):
            return f"{path}[len {len(a)} != {len(b)}]"
        for i, (x, y) in enumerate(zip(a, b)):
            d = first_difference(x, y, f"{path}[{i}]")
            if d:
                return d
        return None
    return None if a == b else path


def compare(before, after):
    bad = []
    for key in sorted(set(before) | set(after)):
        if key not in after:
            bad.append(f"{key}: disappeared")
            continue
        if key not in before:
            # A NEW operator/cert-manager CRD is an upgrade changing CRDs.
            bad.append(f"{key}: appeared during the upgrade")
            continue
        for part in ("spec", "images", "bootstrap_managed"):
            d = first_difference(before[key].get(part), after[key].get(part))
            if d is not None:
                bad.append(f"{key}: {part} changed at {d}")
    if bad:
        for b in bad:
            print("  DIFF:", b)
        fail(f"{len(bad)} object(s) the upgrade must not change were changed (ADR-057)")
    ok(f"{len(before)} protected objects unchanged (specs, images, {BOOTSTRAP_MANAGER} fields)")


def binding_present():
    r = subprocess.run(["kubectl", "get", "clusterrolebinding", BOOTSTRAP_MANAGER],
                       capture_output=True, text=True)
    if r.returncode == 0:
        return True
    if "NotFound" in r.stderr:
        return False
    fail(f"could not read clusterrolebinding {BOOTSTRAP_MANAGER}: {r.stderr.strip()}")


def operator_replicas():
    op = kubectl_json("-n", APP_NS, "get", "deployment", OPERATOR)
    return op["spec"].get("replicas")


# ── subcommands ──────────────────────────────────────────────────────────────

def cmd_prepare(args):
    global BASE
    BASE, user, pw, state_file = args[0].rstrip("/"), args[1], args[2], args[3]
    login(user, pw)

    # The SPA's conformity screen: poll status, send ensure once when it is not
    # ready and nothing is installing (frontend/views_bootstrap.jsx).
    deadline = time.time() + 1500
    ensured = False
    while True:
        status, s = api("bootstrap_status")
        if status == 200 and s:
            if s.get("ready"):
                ok("bootstrap ready")
                break
            if s.get("unsupported"):
                fail(f"cluster reported unsupported: {s.get('requirements')}")
            if not ensured and not s.get("installing"):
                api("bootstrap_ensure", {})
                ensured = True
                ok("bootstrap_ensure sent")
            if s.get("error"):
                print("  bootstrap error so far:", s["error"])
        if time.time() > deadline:
            fail(f"bootstrap never became ready: {s}")
        time.sleep(10)

    # Creation runs the storage gate synchronously (ensure_storage_ready): a
    # cluster with no default StorageClass installs Longhorn inside this
    # request, hence the long timeout. minikube's node-local default needs none
    # (ADR-061).
    status, name = api("create_cluster", {
        "name": "upg", "size": "small", "purpose": "search",
        "nodes": "", "memory": "", "disk": "", "config": "", "monitors": None,
    }, timeout=1800)
    if status != 200:
        fail(f"create_cluster: HTTP {status} {name}")
    ok(f"created deployment {name}")
    with open(state_file, "w") as f:
        json.dump({"deployment": name}, f)
    wait_green(name, 1800)


def cmd_green(args):
    global BASE
    BASE, user, pw, state_file = args[0].rstrip("/"), args[1], args[2], args[3]
    login(user, pw)
    with open(state_file) as f:
        name = json.load(f)["deployment"]
    wait_green(name, int(opt(args, "--timeout", "900")))


def cmd_binding(args):
    want = args[0]
    if want == "present":
        if not binding_present():
            fail(f"{BOOTSTRAP_MANAGER} was revoked while bootstrap was not complete")
        ok(f"{BOOTSTRAP_MANAGER} kept while bootstrap is not complete")
        return
    secs = int(opt(args, "--timeout", "300"))
    deadline = time.time() + secs
    while binding_present():
        if time.time() > deadline:
            fail(f"{BOOTSTRAP_MANAGER} cluster-admin binding still present {secs}s after the upgrade")
        time.sleep(10)
    ok(f"no {BOOTSTRAP_MANAGER} binding left behind")


def cmd_ensure_noop(args):
    global BASE
    BASE, user, pw = args[0].rstrip("/"), args[1], args[2]
    observe = int(opt(args, "--observe", "180"))
    if operator_replicas() != 0:
        fail("ensure-noop needs the operator scaled to 0 first")
    login(user, pw)
    status, s = api("bootstrap_status")
    if status != 200:
        fail(f"bootstrap_status: HTTP {status} {s}")
    if not (s.get("operator_installed") and not s.get("operator_ready")):
        fail(f"expected operator installed-but-not-ready, got {s}")
    ok("bootstrap reports the operator installed but not ready")

    status, s = api("bootstrap_ensure", {})
    if status != 200:
        fail(f"bootstrap_ensure: HTTP {status} {s}")
    ok("bootstrap_ensure sent (what the SPA does on this screen)")

    saw_wait = False
    deadline = time.time() + observe
    while time.time() < deadline:
        if operator_replicas() != 0:
            fail("bootstrap re-applied the operator bundle: replicas went back to 1 (ADR-057)")
        status, s = api("bootstrap_status")
        step = (s or {}).get("installing") or ""
        if step.startswith("wait:"):
            saw_wait = True
        if step == OPERATOR or step == "cert-manager":
            fail(f"bootstrap started INSTALLING {step} over an installed component")
        time.sleep(5)
    if not saw_wait:
        fail("bootstrap never reported waiting on the installed operator")
    ok(f"for {observe}s bootstrap waited and applied nothing; operator stayed at 0 replicas")


def main():
    args = sys.argv[1:]
    if not args:
        print(__doc__)
        sys.exit(2)
    cmd, rest = args[0], args[1:]
    if cmd == "prepare":
        cmd_prepare(rest)
    elif cmd == "snapshot":
        with open(rest[0], "w") as f:
            json.dump(snapshot(), f, indent=1, sort_keys=True)
        ok(f"snapshot written to {rest[0]}")
    elif cmd == "compare":
        with open(rest[0]) as a, open(rest[1]) as b:
            compare(json.load(a), json.load(b))
    elif cmd == "green":
        cmd_green(rest)
    elif cmd == "binding":
        cmd_binding(rest)
    elif cmd == "ensure-noop":
        cmd_ensure_noop(rest)
    else:
        print(__doc__)
        sys.exit(2)
    print("PASS:", cmd)


if __name__ == "__main__":
    main()
