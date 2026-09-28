#!/usr/bin/env bash
# Copyright (C) 2026 Tornis Desenvolvimento
# SPDX-License-Identifier: AGPL-3.0-only
#
# Classify what an upgrade changes beyond the image.
#
#   deploy/manifest-changes.sh <old-install.yaml> <new-install.yaml>
#
# Compares two install manifests document by document, keyed by
# kind/namespace/name, with the veloxsearch-oss image reference (tag or
# digest) ignored. Prints one classification line, then what changed:
#
#   classification: none           only the image moved: `kubectl set image`
#                                  is enough
#   classification: rbac, config   anything else: apply upgrade.yaml
#
#   rbac    Role/ClusterRole rules, or binding roleRef/subjects
#   config  ConfigMap data, or a container's env/envFrom
#   other   everything else, including added and removed documents
#
# Comments, key order and quoting are not changes. RBAC rules compare as sets,
# so reordering rules or verbs is not a change either. Output is sorted, so the
# same pair always prints the same bytes.
#
# release.yml puts the result at the top of each release's notes; ci.yml
# writes it to every PR's job summary. It always exits 0 once it has read both
# files: a manifest it cannot parse is reported as `other`, never an error,
# so the release is never blocked by this report.
#
# The manifests are parsed with the stdlib only (no PyYAML on the release
# runner is assumed): the block-style YAML subset install.yaml is written in,
# plus one-line flow collections. A document outside that subset is reported
# as `other: could not parse`.
set -euo pipefail

[ $# -eq 2 ] || { echo "usage: $0 <old-install.yaml> <new-install.yaml>" >&2; exit 2; }
for f in "$1" "$2"; do
  [ -r "$f" ] || { echo "manifest-changes: cannot read $f" >&2; exit 2; }
done

exec python3 - "$1" "$2" <<'PY'
import json
import re
import sys

IMAGE_REPO = "docker.io/tornistecnologia/veloxsearch-oss"


class ParseError(Exception):
    pass


# ── a stdlib parser for the YAML subset install.yaml uses ─────────────────────

def strip_comment(s):
    """Drop a trailing ` # comment` that is outside quotes."""
    quote = None
    for i, c in enumerate(s):
        if quote:
            if c == quote:
                quote = None
        elif c in "\"'":
            quote = c
        elif c == "#" and (i == 0 or s[i - 1] in " \t"):
            return s[:i].rstrip()
    return s.rstrip()


def split_flow(s):
    """Split the inside of a flow collection on top-level commas."""
    parts, depth, quote, cur = [], 0, None, ""
    for c in s:
        if quote:
            cur += c
            if c == quote:
                quote = None
            continue
        if c in "\"'":
            quote = c
        elif c in "[{":
            depth += 1
        elif c in "]}":
            depth -= 1
        elif c == "," and depth == 0:
            parts.append(cur.strip())
            cur = ""
            continue
        cur += c
    if cur.strip():
        parts.append(cur.strip())
    return parts


def find_colon(s):
    """Index of the `:` that ends a mapping key, or -1."""
    quote, depth = None, 0
    for i, c in enumerate(s):
        if quote:
            if c == quote:
                quote = None
        elif c in "\"'":
            quote = c
        elif c in "[{":
            depth += 1
        elif c in "]}":
            depth -= 1
        elif c == ":" and depth == 0 and (i + 1 == len(s) or s[i + 1] == " "):
            return i
    return -1


def scalar(s):
    s = s.strip()
    if len(s) >= 2 and s[0] == s[-1] == '"':
        return json.loads(s)
    if len(s) >= 2 and s[0] == s[-1] == "'":
        return s[1:-1].replace("''", "'")
    if s[:1] in ("&", "*", "!", "|", ">"):
        raise ParseError(f"unsupported scalar {s!r}")
    return s


def inline(s):
    s = s.strip()
    if s.startswith("["):
        if not s.endswith("]"):
            raise ParseError(f"multi-line flow sequence {s!r}")
        return [inline(p) for p in split_flow(s[1:-1])]
    if s.startswith("{"):
        if not s.endswith("}"):
            raise ParseError(f"multi-line flow mapping {s!r}")
        out = {}
        for p in split_flow(s[1:-1]):
            k = find_colon(p)
            if k < 0:
                raise ParseError(f"flow mapping entry without a key {p!r}")
            out[scalar(p[:k])] = inline(p[k + 1:])
        return out
    return scalar(s)


class Doc:
    def __init__(self, text):
        self.lines = text.split("\n")

    def next_sig(self, i):
        """Index of the next line that is neither blank nor a comment."""
        while i < len(self.lines):
            t = self.lines[i].strip()
            if t and not t.startswith("#"):
                return i
            i += 1
        return i

    def indent(self, i):
        line = self.lines[i]
        if "\t" in line[: len(line) - len(line.lstrip())]:
            raise ParseError("tab indentation")
        return len(line) - len(line.lstrip(" "))

    def content(self, i):
        return self.lines[i].strip(" ")

    def parse(self):
        i = self.next_sig(0)
        if i >= len(self.lines):
            return None
        value, i = self.node(i, self.indent(i))
        if self.next_sig(i) < len(self.lines):
            raise ParseError(f"unexpected line {self.lines[self.next_sig(i)]!r}")
        return value

    def node(self, i, ind):
        c = self.content(i)
        if c == "-" or c.startswith("- "):
            return self.seq(i, ind)
        return self.mapping(i, ind)

    def value_after(self, i, ind, rest, in_seq=False):
        """The value of `key:` / `-` whose inline text is `rest`."""
        rest = strip_comment(rest).strip()
        if rest in ("|", "|-", "|+", ">", ">-", ">+"):
            return self.block_scalar(i + 1, ind, rest)
        if rest:
            return inline(rest), i + 1
        j = self.next_sig(i + 1)
        if j >= len(self.lines):
            return None, j
        jind = self.indent(j)
        jc = self.content(j)
        if jind > ind or (not in_seq and jind == ind and (jc == "-" or jc.startswith("- "))):
            return self.node(j, jind)
        return None, i + 1

    def block_scalar(self, i, ind, style):
        body, j = [], i
        while j < len(self.lines):
            line = self.lines[j]
            if line.strip() and self.indent(j) <= ind:
                break
            body.append(line)
            j += 1
        while body and not body[-1].strip():
            body.pop()
        nonblank = [len(l) - len(l.lstrip(" ")) for l in body if l.strip()]
        cut = min(nonblank) if nonblank else 0
        text_lines = [l[cut:] for l in body]
        if style.startswith(">"):
            text = " ".join(l.strip() for l in text_lines)
        else:
            text = "\n".join(text_lines)
        if not style.endswith("-") and text:
            text += "\n"
        return text, j

    def mapping(self, i, ind):
        out = {}
        while True:
            i = self.next_sig(i)
            if i >= len(self.lines) or self.indent(i) != ind:
                return out, i
            c = self.content(i)
            if c == "-" or c.startswith("- "):
                return out, i
            k = find_colon(c)
            if k < 0:
                raise ParseError(f"expected `key:` in {c!r}")
            key = scalar(c[:k])
            if key in out:
                raise ParseError(f"duplicate key {key!r}")
            out[key], i = self.value_after(i, ind, c[k + 1:])
            if i < len(self.lines) and self.lines[i].strip() and self.indent(i) > ind \
                    and not self.content(i).startswith("#"):
                raise ParseError(f"unexpected indentation at {self.lines[i]!r}")

    def seq(self, i, ind):
        out = []
        while True:
            i = self.next_sig(i)
            if i >= len(self.lines) or self.indent(i) != ind:
                return out, i
            c = self.content(i)
            if not (c == "-" or c.startswith("- ")):
                return out, i
            rest = c[1:].lstrip(" ")
            col = ind + (len(c) - len(rest))
            if rest and not rest.startswith("#") and rest[0] not in "[{\"'" and find_colon(strip_comment(rest)) >= 0:
                # `- key: value` opens a mapping at the column of `key`.
                self.lines[i] = " " * col + rest
                item, i = self.mapping(i, col)
            else:
                item, i = self.value_after(i, ind, rest, in_seq=True)
            out.append(item)


def documents(path):
    """[(key, parsed-or-None, error, text)] for every non-empty document."""
    with open(path, encoding="utf-8") as f:
        text = f.read()
    chunks, cur = [], []
    for line in text.split("\n"):
        if line.rstrip() == "---":
            chunks.append("\n".join(cur))
            cur = []
        else:
            cur.append(line)
    chunks.append("\n".join(cur))

    out, seen = [], {}
    for n, chunk in enumerate(chunks):
        try:
            doc = Doc(chunk).parse()
            err = None
        except (ParseError, ValueError) as e:
            doc, err = None, str(e)
        if doc is None and err is None:
            continue
        if err is None and not isinstance(doc, dict):
            doc, err = None, "document is not a mapping"
        if err is None:
            meta = doc.get("metadata") or {}
            parts = [str(doc.get("kind", "?")), meta.get("namespace"), str(meta.get("name", "?"))]
        else:
            # Still key it by its identity lines, so it pairs with its
            # counterpart and is reported as unparseable, not added/removed.
            def grab(pattern):
                m = re.search(pattern, chunk, re.M)
                try:
                    return scalar(strip_comment(m.group(1))) if m else None
                except (ParseError, ValueError):
                    return None
            parts = [grab(r"^kind:(.*)$"), grab(r"^  namespace:(.*)$"), grab(r"^  name:(.*)$")]
            if not parts[0] or not parts[2]:
                parts = [f"document #{n}"]
        key = "/".join(p for p in parts if p)
        seen[key] = seen.get(key, 0) + 1
        if seen[key] > 1:
            key = f"{key} (#{seen[key]})"
        out.append((key, doc, err, chunk))
    return out


# ── comparison ────────────────────────────────────────────────────────────────

def canon(v):
    return json.dumps(v, sort_keys=True, separators=(",", ":"))


def flatten(v, prefix=""):
    """{path: scalar} with list items addressed by `name` when they have one."""
    out = {}
    if isinstance(v, dict):
        if not v:
            out[prefix] = "{}"
        for k in v:
            out.update(flatten(v[k], f"{prefix}.{k}" if prefix else str(k)))
    elif isinstance(v, list):
        if not v:
            out[prefix] = "[]"
        names = [x.get("name") for x in v if isinstance(x, dict)]
        by_name = len(names) == len(v) and None not in names and len(set(names)) == len(names)
        for n, x in enumerate(v):
            out.update(flatten(x, f"{prefix}[{x['name'] if by_name else n}]"))
    else:
        out[prefix] = v
    return out


def changed_paths(a, b):
    fa, fb = flatten(a), flatten(b)
    missing = object()
    return sorted(p for p in set(fa) | set(fb) if fa.get(p, missing) != fb.get(p, missing))


def summarize(paths, limit=6):
    shown = ", ".join(paths[:limit])
    return shown + (f", … (+{len(paths) - limit} more)" if len(paths) > limit else "")


def rule_text(rule):
    if not isinstance(rule, dict):
        return canon(rule)
    order = ["apiGroups", "resources", "resourceNames", "nonResourceURLs", "verbs"]
    keys = [k for k in order if k in rule] + sorted(k for k in rule if k not in order)
    parts = []
    for k in keys:
        v = rule[k]
        if isinstance(v, list):
            v = "[" + ",".join(json.dumps(x) if x == "" else str(x) for x in v) + "]"
        parts.append(f"{k}={v}")
    return " ".join(parts)


def norm_rule(rule):
    if isinstance(rule, dict):
        return {k: sorted(v, key=str) if isinstance(v, list) else v for k, v in rule.items()}
    return rule


def rule_set(doc):
    return {rule_text(norm_rule(r)) for r in (doc or {}).get("rules") or []}


def subject_text(s):
    if not isinstance(s, dict):
        return canon(s)
    ns = f"{s['namespace']}/" if s.get("namespace") else ""
    return f"{s.get('kind', '?')} {ns}{s.get('name', '?')}"


def roleref_text(r):
    return f"{r.get('kind', '?')}/{r.get('name', '?')}" if isinstance(r, dict) else canon(r)


def pod_specs(doc):
    """The pod spec(s) of a workload, by path, so env can be split out."""
    spec = doc.get("spec")
    if not isinstance(spec, dict):
        return []
    tmpl = spec.get("template")
    if isinstance(tmpl, dict) and isinstance(tmpl.get("spec"), dict):
        return [tmpl["spec"]]
    job = spec.get("jobTemplate")
    if isinstance(job, dict):
        tmpl = (job.get("spec") or {}).get("template")
        if isinstance(tmpl, dict) and isinstance(tmpl.get("spec"), dict):
            return [tmpl["spec"]]
    return []


def containers(doc):
    for ps in pod_specs(doc):
        for field in ("initContainers", "containers"):
            for c in ps.get(field) or []:
                if isinstance(c, dict):
                    yield c


def image_repo(image):
    """`repo:tag@digest` → repo (the tag and digest are what an upgrade moves)."""
    repo = image.split("@", 1)[0]
    last = repo.rsplit("/", 1)[-1]
    if ":" in last:
        repo = repo[: len(repo) - len(last)] + last.split(":", 1)[0]
    return repo


def without_image_and_env(doc):
    """A deep copy with the veloxsearch image pinned and env removed."""
    doc = json.loads(json.dumps(doc))
    for c in containers(doc):
        img = c.get("image")
        if isinstance(img, str) and image_repo(img) == IMAGE_REPO:
            c["image"] = IMAGE_REPO
        c.pop("env", None)
        c.pop("envFrom", None)
    return doc


def env_changes(old, new):
    out = []
    oc = {c.get("name"): c for c in containers(old)}
    nc = {c.get("name"): c for c in containers(new)}
    for name in sorted(set(oc) & set(nc), key=str):
        def env_map(c):
            return {e.get("name") if isinstance(e, dict) else canon(e): canon(e) for e in c.get("env") or []}
        oe, ne = env_map(oc[name]), env_map(nc[name])
        marks = [f"+{k}" for k in sorted(set(ne) - set(oe), key=str)]
        marks += [f"-{k}" for k in sorted(set(oe) - set(ne), key=str)]
        marks += [f"~{k}" for k in sorted(set(oe) & set(ne), key=str) if oe[k] != ne[k]]
        if marks:
            out.append(f"container {name} env: {' '.join(marks)}")
        of = sorted(canon(x) for x in oc[name].get("envFrom") or [])
        nf = sorted(canon(x) for x in nc[name].get("envFrom") or [])
        if of != nf:
            out.append(f"container {name} envFrom changed")
    return out


def data_changes(old, new):
    marks = []
    for field in ("data", "binaryData"):
        od, nd = old.get(field) or {}, new.get(field) or {}
        marks += [f"+{k}" for k in sorted(set(nd) - set(od))]
        marks += [f"-{k}" for k in sorted(set(od) - set(nd))]
        marks += [f"~{k}" for k in sorted(set(od) & set(nd)) if od[k] != nd[k]]
    return marks


RBAC_ROLES = {"Role", "ClusterRole"}
RBAC_BINDINGS = {"RoleBinding", "ClusterRoleBinding"}


def compare(old_path, new_path):
    rbac, config, other = [], [], []
    old = {k: (d, e, t) for k, d, e, t in documents(old_path)}
    new = {k: (d, e, t) for k, d, e, t in documents(new_path)}

    for key in sorted(set(old) | set(new)):
        if key not in new:
            other.append(f"- {key} (removed)" + (f", could not parse ({old[key][1]})" if old[key][1] else ""))
            doc = old[key][0] or {}
            if doc.get("kind") in RBAC_ROLES:
                rbac += [f"{key} - {r}" for r in sorted(rule_set(doc))]
            elif doc.get("kind") in RBAC_BINDINGS:
                rbac.append(f"{key} (binding removed)")
            continue
        if key not in old:
            other.append(f"+ {key} (added)" + (f", could not parse ({new[key][1]})" if new[key][1] else ""))
            doc = new[key][0] or {}
            if doc.get("kind") in RBAC_ROLES:
                rbac += [f"{key} + {r}" for r in sorted(rule_set(doc))]
            elif doc.get("kind") in RBAC_BINDINGS:
                rbac.append(f"{key} (binding added)")
            continue

        (od, oe, ot), (nd, ne, nt) = old[key], new[key]
        if (oe or ne) and ot == nt:
            continue
        if oe or ne:
            other.append(f"{key}: could not parse ({oe or ne}) — compare by hand")
            continue
        if canon(od) == canon(nd):
            continue
        kind = nd.get("kind")
        rest_old, rest_new = od, nd

        if kind in RBAC_ROLES:
            ors, nrs = rule_set(od), rule_set(nd)
            rbac += [f"{key} + {r}" for r in sorted(nrs - ors)]
            rbac += [f"{key} - {r}" for r in sorted(ors - nrs)]
            rest_old = {k: v for k, v in od.items() if k != "rules"}
            rest_new = {k: v for k, v in nd.items() if k != "rules"}
        elif kind in RBAC_BINDINGS:
            if canon(od.get("roleRef")) != canon(nd.get("roleRef")):
                rbac.append(f"{key} roleRef {roleref_text(od.get('roleRef'))} -> {roleref_text(nd.get('roleRef'))}")
            os_ = {subject_text(s) for s in od.get("subjects") or []}
            ns_ = {subject_text(s) for s in nd.get("subjects") or []}
            rbac += [f"{key} + subject {s}" for s in sorted(ns_ - os_)]
            rbac += [f"{key} - subject {s}" for s in sorted(os_ - ns_)]
            rest_old = {k: v for k, v in od.items() if k not in ("roleRef", "subjects")}
            rest_new = {k: v for k, v in nd.items() if k not in ("roleRef", "subjects")}
        elif kind == "ConfigMap":
            marks = data_changes(od, nd)
            if marks:
                config.append(f"{key} data: {' '.join(marks)}")
            rest_old = {k: v for k, v in od.items() if k not in ("data", "binaryData")}
            rest_new = {k: v for k, v in nd.items() if k not in ("data", "binaryData")}
        elif pod_specs(nd) or pod_specs(od):
            config += [f"{key} {m}" for m in env_changes(od, nd)]
            rest_old, rest_new = without_image_and_env(od), without_image_and_env(nd)

        paths = changed_paths(rest_old, rest_new)
        if paths:
            other.append(f"{key}: {summarize(paths)}")

    return rbac, config, other


def main(old_path, new_path):
    try:
        rbac, config, other = compare(old_path, new_path)
    except (OSError, UnicodeDecodeError) as e:
        print("classification: other")
        print("other:")
        print(f"  - manifest diff failed ({e}) — compare by hand")
        return
    classes = [(n, items) for n, items in (("rbac", rbac), ("config", config), ("other", other)) if items]
    if not classes:
        print("classification: none")
        print("only the veloxsearch image changed: `kubectl set image` is enough")
        return
    print("classification: " + ", ".join(n for n, _ in classes))
    for n, items in classes:
        print(f"{n}:")
        for item in items:
            print(f"  - {item}")


main(sys.argv[1], sys.argv[2])
PY
