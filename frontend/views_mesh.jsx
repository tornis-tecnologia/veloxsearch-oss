// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
/* ============================================================
   views_mesh.jsx — Velox Mesh: the deployment's fleet tab.

   The Mesh is a DEPLOYMENT's, not the installation's (ADR-072): this is the
   `mesh` tab of `/deployments/:id/mesh/:sub`, and every policy, agent and
   token on it belongs to `d`. The controller itself is installed once per
   installation — that guide lives under Settings (`MeshInstallGuide`,
   admin-only), not here.

   The journey is Elastic Fleet's, because operators already know it and it
   is the right shape:

     configure endpoints  →  create a collection policy
                          →  add integrations to the policy
                          →  add an agent on a host (with the policy's token)

   Three rules hold the model together (ADR-072 amendment 2):

     1. The deployment's SETTINGS hold named endpoints: where hosts reach its
        OpenSearch (host[:port]) and where they reach the Mesh (a URL). They
        are required — there is no implicit in-cluster default, because a
        host that cannot resolve the service name fails silently.
     2. Every POLICY is bound to one endpoint of each kind. The OpenSearch
        address is compiled into its collectors' output; the Mesh address is
        the `--mesh` of its enrolment command. Changing either is a new
        revision.
     3. Every AGENT runs a policy. The enrolment token is the policy's, so an
        agent cannot exist without one, and the policy is what applies the
        integrations' rules and the two addresses to the host.

   Four sub-screens, matching the four questions an operator has:
     overview  — the fleet at a glance (status counts, bar, configured agents)
     agents    — the fleet, each agent's editor, and "Add agent": a dialog
                 that picks the policy, mints its token and carries the
                 Fluent Bit + Velox Agent install steps per distribution
     policies  — what a group of hosts runs: integrations and endpoints
     settings  — the Mesh's state, the guided first run, the Mesh daemons
                 the console and the hosts reach, and the endpoint lists

   The install guide mirrors the targets the Fluent Bit project itself
   documents (docs.fluentbit.io/manual/installation/downloads), including
   the ones we have no commands for (Buildroot, Yocto, macOS) — those link
   out rather than inventing a procedure. We ship no engine (ADR-069), and
   the commands are data, not i18n strings.
   ============================================================ */
import { useEffect, useState } from "react";
import { STR } from "./i18n.jsx";
import { API } from "./api.jsx";
import { Banner, Btn, Confirm, Dialog, Field, Icon, Menu, Tabs, copyText, fmtBytes } from "./ui.jsx";
import { AddIntegrationDialog, locText } from "./views_integration_add.jsx";
import { AddPackageDialog } from "./views_integration_v2.jsx";

/* ADR-069 decision 3; `velox_agent::preflight::MIN_FLUENT_BIT` is the same
   number on the other side. */
const MIN_FLUENT_BIT = "3.2";
const FB_DOWNLOADS = "https://docs.fluentbit.io/manual/installation/downloads";
const FB_KEY_FPR = "C3C0 A285 34B9 293E AF51 FABD 9F9D DC08 3888 C1CD";

/* ---- install targets, mirroring the project's own downloads index -------
   `repo`/`install` are verbatim from the matching docs.fluentbit.io page.
   A target with `linkOnly` is one the project documents but we have no
   commands for: we send the operator there instead of guessing. ------- */
const DEB_KEY =
  "sudo sh -c 'curl https://packages.fluentbit.io/fluentbit.key | gpg --dearmor > /usr/share/keyrings/fluentbit-keyring.gpg'";
const DEB_CODENAME =
  "codename=$(grep -oP '(?<=VERSION_CODENAME=).*' /etc/os-release 2>/dev/null || lsb_release -cs 2>/dev/null)";
const DOCS = "https://docs.fluentbit.io/manual/installation/downloads";

function aptRepo(flavour) {
  return [DEB_KEY, DEB_CODENAME,
    `echo "deb [signed-by=/usr/share/keyrings/fluentbit-keyring.gpg] https://packages.fluentbit.io/${flavour}/$codename $codename main" | sudo tee /etc/apt/sources.list.d/fluent-bit.list`,
    "sudo apt-get update"].join("\n");
}
function yumRepo(flavour, releasever) {
  return ["sudo tee /etc/yum.repos.d/fluent-bit.repo <<'EOF'", "[fluent-bit]", "name=Fluent Bit",
    `baseurl=https://packages.fluentbit.io/${flavour}/${releasever}/`,
    "gpgcheck=1", "gpgkey=https://packages.fluentbit.io/fluentbit.key",
    "repo_gpgcheck=1", "enabled=1", "EOF"].join("\n");
}

/* The agent release the guide installs (ADR-069 amendment 1): the
   `velox-agent-v<version>` GitHub release — static musl tarballs, SHA256SUMS,
   install.sh — and the image built from the same Dockerfile. Bumped with
   veloxagent/Cargo.toml. */
const AGENT_VERSION = "0.2.0";
const AGENT_RELEASE = `https://github.com/tornis-tecnologia/veloxsearch-oss/releases/download/velox-agent-v${AGENT_VERSION}`;
const AGENT_IMAGE = `docker.io/tornistecnologia/velox-agent:${AGENT_VERSION}`;

/* The three ways onto a host. `script` does everything (distribution,
   Fluent Bit from upstream, the agent, the velox-agent user, the systemd
   unit, the enrol); `docker` is the image; `manual` is the per-distribution
   procedure, ending in the same install.sh with the engine already there. */
function scriptCmd(mesh, tok, insecure) {
  return [`curl -fsSLO ${AGENT_RELEASE}/install.sh`,
    `sudo bash install.sh --mesh ${mesh} --token ${tok}${insecure ? " --insecure" : ""}`].join("\n");
}
function manualAgentCmd(mesh, tok, insecure) {
  const tgz = `velox-agent-${AGENT_VERSION}-$(uname -m)-unknown-linux-musl.tar.gz`;
  return [`curl -fsSLO ${AGENT_RELEASE}/${tgz}`,
    `curl -fsSLO ${AGENT_RELEASE}/SHA256SUMS`,
    `curl -fsSLO ${AGENT_RELEASE}/install.sh`,
    "sha256sum -c --ignore-missing SHA256SUMS",
    `tar -xzf ${tgz} velox-agent`,
    `sudo bash install.sh --binary ./velox-agent --skip-fluent-bit --mesh ${mesh} --token ${tok}${insecure ? " --insecure" : ""}`].join("\n");
}
function dockerCmd(mesh, tok, insecure) {
  return ["docker run -d --name velox-agent --restart unless-stopped \\",
    `  -e VELOX_MESH_URL=${mesh} -e VELOX_ENROLL_TOKEN=${tok} \\`,
    ...(insecure ? ["  -e VELOX_AGENT_INSECURE=true \\"] : []),
    '  -e VELOX_AGENT_HOSTNAME="$(hostname)" \\',
    "  -v velox-agent-state:/opt/velox-agent/state \\",
    "  -v /var/log:/var/log:ro \\",
    "  -v /var/lib/docker/containers:/var/lib/docker/containers:ro \\",
    "  -v /etc/machine-id:/etc/machine-id:ro \\",
    "  --user 0:0 --cap-drop ALL --cap-add DAC_READ_SEARCH \\",
    "  --security-opt no-new-privileges:true --read-only --tmpfs /tmp \\",
    `  ${AGENT_IMAGE}`].join("\n");
}

const TARGETS = [
  {
    key: "amazon", label: "Amazon Linux",
    docs: `${DOCS}/linux/amazon-linux`,
    repo: yumRepo("amazonlinux", "2023"), install: "sudo dnf install fluent-bit", host: true,
  },
  {
    key: "buildroot", label: "Buildroot", linkOnly: true,
    docs: `${DOCS}/linux/buildroot-embedded-linux`,
    note: "mesh_note_embedded", agent: "# build velox-agent for the target and install it with the image",
  },
  {
    key: "debian", label: "Debian",
    docs: `${DOCS}/linux/debian`, note: "mesh_note_no_pkg",
    repo: aptRepo("debian"), install: "sudo apt-get install fluent-bit", host: true,
  },
  {
    key: "raspbian", label: "Raspbian · Raspberry Pi",
    docs: `${DOCS}/linux/raspbian-raspberry-pi`, note: "mesh_note_aptkey",
    repo: ["sudo sh -c 'curl https://packages.fluentbit.io/fluentbit.key | sudo apt-key add - '",
      'echo "deb https://packages.fluentbit.io/raspbian/bookworm bookworm main" | sudo tee /etc/apt/sources.list.d/fluent-bit.list',
      "sudo apt-get update"].join("\n"),
    install: "sudo apt-get install fluent-bit", host: true,
  },
  {
    key: "redhat", label: "Red Hat · CentOS",
    docs: `${DOCS}/linux/redhat-centos`, note: "mesh_note_rhel9",
    repo: yumRepo("centos", "$releasever"), install: "sudo dnf install fluent-bit",
    alt: { label: "EPEL (RHEL 9)", cmd: "sudo dnf install epel-release\nsudo dnf install fluent-bit" },
    host: true,
  },
  {
    key: "rocky", label: "Rocky · Alma",
    docs: `${DOCS}/linux/alma-rocky`,
    repo: yumRepo("almalinux", "$releasever"), install: "sudo dnf install fluent-bit", host: true,
  },
  {
    key: "ubuntu", label: "Ubuntu",
    docs: `${DOCS}/linux/ubuntu`, note: "mesh_note_no_pkg",
    repo: aptRepo("ubuntu"), install: "sudo apt-get install fluent-bit", host: true,
  },
  {
    key: "yocto", label: "Yocto", linkOnly: true,
    docs: `${DOCS}/linux/yocto-embedded-linux`,
    note: "mesh_note_embedded", agent: "# build velox-agent for the target and install it with the image",
  },
  {
    key: "macos", label: "macOS", linkOnly: true,
    docs: `${DOCS}/macos`, note: "mesh_note_macos",
    agent: "# the velox-agent package for macOS follows the Linux one",
  },
  {
    key: "windows", label: "Windows",
    docs: `${DOCS}/windows`, note: "mesh_note_windows", windows: true,
    repo: ["# Take the installer and its checksum from the official downloads page,",
      "# then compare the hash before running it:",
      "Get-FileHash .\\fluent-bit-<version>-win64.msi"].join("\n"),
    install: "msiexec /i fluent-bit-<version>-win64.msi /quiet",
    agent: "msiexec /i velox-agent-0.1.0-win64.msi /quiet",
    enroll: "velox-agent.exe enroll --mesh MESH_URL --token TOKEN",
    verify: "velox-agent.exe doctor",
  },
];

/* ---- endpoints (ADR-072 amendment 2) -----------------------------------
   The settings hold the lists; a policy row (`os_endpoint`/`mesh_endpoint`)
   or a policy document (`endpoints.opensearch`/`endpoints.mesh`) names one
   of each by id. Resolution happens here, once, so every screen agrees on
   what "incomplete" and "removed" look like. ------------------------------ */
function epOf(list, id) {
  if (!id) return null;
  return (list || []).find(e => e.id === id) || null;
}

function policyEndpoints(ids, settings) {
  const osId = ids ? (ids.os_endpoint || ids.opensearch || null) : null;
  const meshId = ids ? (ids.mesh_endpoint || ids.mesh || null) : null;
  const os = epOf(settings && settings.os_endpoints, osId);
  const mesh = epOf(settings && settings.mesh_endpoints, meshId);
  return { osId, meshId, os, mesh, complete: !!(os && mesh) };
}

/* An endpoint's name as shown: the core names the derived public-route
   endpoint in English (it is data on the wire), the screen in its language. */
function epName(t, e) {
  return e && e.managed ? t.mesh_ep_route_name : (e ? e.name : "");
}

function slugOf(n) {
  return n.trim().toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "").slice(0, 40);
}

/* What Fluent Bit's opensearch output will be given for an OpenSearch
   address — the same reading as velox_policy::OsAddress::parse, for the
   screen only (the server validates and binds): a bare host is 9200 over
   TLS (the package default), host:port keeps TLS, an http(s) URL takes the
   port from the scheme (80/443) and a path as the output's `Path`. */
export function osAddressOf(input) {
  const s = (input || "").trim();
  if (!s || /\s/.test(s)) return null;
  let tls = true, port = 9200, rest = s;
  const m = s.match(/^([a-zA-Z]+):\/\/(.*)$/);
  if (m) {
    const scheme = m[1].toLowerCase();
    if (scheme === "http") { tls = false; port = 80; } else if (scheme === "https") { port = 443; } else return null;
    rest = m[2];
  }
  if (/[?#@]/.test(rest)) return null;
  const slash = rest.indexOf("/");
  const authority = slash >= 0 ? rest.slice(0, slash) : rest;
  let path = slash >= 0 ? rest.slice(slash).replace(/\/+$/, "") : "";
  let host = authority, p = null;
  const v6 = authority.match(/^\[([^\]]+)\](?::(.*))?$/);
  if (v6) { host = v6[1]; p = v6[2] === undefined ? null : v6[2]; }
  else {
    const i = authority.lastIndexOf(":");
    if (i >= 0) { host = authority.slice(0, i); p = authority.slice(i + 1); if (host.includes(":")) return null; }
  }
  if (!host) return null;
  if (p !== null) { if (!/^\d+$/.test(p)) return null; port = Number(p); if (port < 1 || port > 65535) return null; }
  return { host, port, tls, path: path || null };
}

/* The address as the four keys, in words: "porta 443 · TLS · /opensearch". */
function OsAddressHint({ t, value, testid }) {
  const a = osAddressOf(value);
  if (!value || !value.trim()) return null;
  if (!a) return <span className="hint" data-testid={testid} data-valid="0" style={{ color: "var(--danger-text)" }}>{t.mesh_ep_invalid}</span>;
  return (
    <span className="hint" data-testid={testid} data-valid="1">
      {a.host} · {t.mesh_ep_port} {a.port} · {a.tls ? t.mesh_ep_tls_on : t.mesh_ep_tls_off}{a.path ? ` · ${a.path}` : ""}
    </span>
  );
}

/* One endpoint as a value: its name and address, or why it cannot be shown. */
function EndpointValue({ t, id, ep }) {
  if (!id) return <span className="hint" data-state="missing">{t.mesh_ep_missing}</span>;
  if (!ep) return <span className="hint" data-state="gone" title={id}><span className="mono">{id}</span> · {t.mesh_ep_gone}</span>;
  return <span data-state="ok">{epName(t, ep)} <span className="mono hint">{ep.host || ep.url}</span></span>;
}

/* The two endpoints of a policy, as the card, the agent's editor and the
   add-agent dialog all show them. */
function PolicyEndpointsLine({ t, ids, settings, testid }) {
  const r = policyEndpoints(ids, settings);
  return (
    <div className="ep-line" data-testid={testid} data-complete={r.complete ? "1" : "0"}>
      <span><span className="k">{t.mesh_pol_os_endpoint}</span> <EndpointValue t={t} id={r.osId} ep={r.os} /></span>
      <span><span className="k">{t.mesh_pol_mesh_endpoint}</span> <EndpointValue t={t} id={r.meshId} ep={r.mesh} /></span>
    </div>
  );
}

/* ---- the fleet ------------------------------------------------------
   The Mesh derives every status from facts it stored (last check-in, what
   the agent echoed back); the console only renders the word and never
   recomputes the rule, so "why is this agent red?" has one answer. --- */
const STATUS_TONE = {
  healthy: "green", degraded: "yellow", updating: "yellow",
  failed: "red", offline: "red", unenrolled: "unknown",
};

/* The design system's `StatusPill` maps a word through `t.health_*`, which is
   the CLUSTER's vocabulary (green/yellow/red). An agent's status is a
   different set — updating, failed, offline — so this borrows the `.pill`
   look and keeps its own words rather than bending health onto it. */
function AgentPill({ status, t }) {
  const tone = STATUS_TONE[status] || "unknown";
  return (
    <span className={`pill ${tone} sm`} data-status={status}>
      <span className="dot" />{t[`mesh_st_${status}`] || status}
    </span>
  );
}

function seenAgo(ts, t) {
  const d = Math.max(0, Math.floor(Date.now() / 1000) - (ts || 0));
  if (d < 5) return t.mesh_seen_now;
  if (d < 60) return t.mesh_seen_s.replace("{n}", d);
  if (d < 3600) return t.mesh_seen_m.replace("{n}", Math.floor(d / 60));
  return t.mesh_seen_h.replace("{n}", Math.floor(d / 3600));
}

/* The fleet at a glance, Fleet-style: one count per status word and one
   stacked bar. The buckets are the Mesh's status set folded the way an
   operator reads it — degraded and failed are both "unhealthy" here, the
   row below still says which. Counted on the client from the rows the
   Mesh already returned; no second call. */
const SUMMARY = [
  { key: "healthy", tone: "green", of: s => s === "healthy" },
  { key: "unhealthy", tone: "red", of: s => s === "degraded" || s === "failed" },
  { key: "updating", tone: "yellow", of: s => s === "updating" },
  { key: "offline", tone: "unknown", of: s => s === "offline" },
  { key: "unenrolled", tone: "unknown", of: s => s === "unenrolled" },
];

/* The chips are also the fleet's status filter (Fleet-style): one pressed
   chip shows only its bucket, pressing it again — or the total — shows all. */
function FleetSummary({ t, agents, policies, filter, onFilter }) {
  const rows = agents || [];
  const counts = SUMMARY.map(b => ({ ...b, n: rows.filter(a => b.of(a.status)).length }));
  const total = rows.length;
  const pick = (k) => onFilter && onFilter(filter === k ? null : k);
  return (
    <div className="card pad" data-testid="mesh-summary" style={{ marginBottom: 18 }}>
      <div className="fleet-sum">
        <button type="button" className="total chip-btn" data-testid="mesh-summary-total" aria-pressed={!filter}
          onClick={() => onFilter && onFilter(null)} title={t.mesh_filter_all}>
          {t.mesh_sum_total.replace("{n}", total).replace("{p}", (policies || []).length)}
        </button>
        {counts.map(b => (
          <button type="button" key={b.key} className={`chip chip-btn ${b.tone}`} data-testid={`mesh-sum-${b.key}`} data-n={b.n}
            aria-pressed={filter === b.key} onClick={() => pick(b.key)} title={t.mesh_filter_by}>
            <span className="dot" />{b.key === "unhealthy" ? t.mesh_sum_unhealthy : t[`mesh_st_${b.key}`]} <b>{b.n}</b>
          </button>
        ))}
      </div>
      <div className="fleet-bar" aria-hidden="true">
        {total > 0 && counts.filter(b => b.n > 0).map(b => (
          <span key={b.key} className={b.tone} style={{ width: `${(100 * b.n) / total}%` }} />
        ))}
      </div>
    </div>
  );
}

/* The deployment's fleet, Fleet-style: status, host, policy and revision,
   what the agent costs the host (CPU as a share of one core over its last
   poll interval, resident memory — reported by velox-agent's usage.rs on
   every check-in), when it last reported, its version and the engine's.
   Each row opens the agent's editor (where the policy is picked) or
   unenrols; the policy is a word here, never a picker. */
// The engine key as `velox_policy::Engine` serialises it → how it is spelled.
const ENGINE_NAMES = { fluent_bit: "Fluent Bit", otel_collector: "OpenTelemetry Collector" };

function attr(a, key) {
  const v = a && a.attributes ? a.attributes[key] : undefined;
  return v === undefined || v === null || v === "" ? null : String(v);
}

function FleetTable({ t, lang, agents, onEdit, onUnenrol, onForget, emptyText, testid = "mesh-fleet" }) {
  if (!agents || agents.length === 0) return <p className="hint" data-testid={`${testid}-empty`}>{emptyText || t.mesh_fleet_empty}</p>;
  const actions = !!(onEdit || onUnenrol || onForget);
  const na = <span className="hint">{t.mesh_na}</span>;
  return (
    <table className="fleet" data-testid={testid}>
      <thead>
        <tr>
          <th>{t.mesh_fleet_status}</th><th>{t.mesh_fleet_host}</th>
          <th>{t.mesh_fleet_policy}</th>
          <th>{t.mesh_fleet_cpu}</th><th>{t.mesh_fleet_mem}</th>
          <th>{t.mesh_fleet_seen}</th><th>{t.mesh_fleet_version}</th>
          {actions && <th>{t.mesh_fleet_actions}</th>}
        </tr>
      </thead>
      <tbody>
        {agents.map(a => {
          const short = a.instance_uid.slice(0, 8);
          const live = a.status !== "unenrolled";
          const cpu = attr(a, "velox.agent.cpu_percent");
          const rss = attr(a, "velox.agent.rss_bytes");
          const ver = attr(a, "velox.agent.version");
          const engine = attr(a, "velox.engine");
          const engineVer = attr(a, "velox.engine.version");
          const os = [attr(a, "velox.os"), attr(a, "velox.arch")].filter(Boolean).join(" · ");
          return (
            <tr key={a.instance_uid} data-testid={`fleet-row-${short}`}
              data-status={a.status} data-up={a.up ? "1" : "0"}>
              <td>
                <AgentPill status={a.status} t={t} />
                {a.config_error && (
                  <div className="mono fleet-err" title={a.config_error}>{a.config_error}</div>
                )}
              </td>
              <td>
                <div className="mono">{a.hostname || a.instance_uid.slice(0, 12)}</div>
                {os && <div className="hint" style={{ fontSize: 11.5 }}>{os}</div>}
              </td>
              <td>
                <span className="mono" data-testid={`fleet-policy-${short}`}>{a.policy_id || "—"}</span>
                <div className="hint mono" style={{ fontSize: 11.5 }}>
                  {a.applied_revision != null ? t.mesh_revision.replace("{n}", a.applied_revision) : "—"}
                  {a.current_revision != null && a.applied_revision !== a.current_revision
                    && <span> → {a.current_revision}</span>}
                </div>
              </td>
              <td className="tnum" data-testid={`fleet-cpu-${short}`}>{live && cpu !== null ? `${cpu} %` : na}</td>
              <td className="tnum" data-testid={`fleet-mem-${short}`}>{live && rss !== null ? fmtBytes(Number(rss), lang) : na}</td>
              <td className="hint">{seenAgo(a.last_seen, t)}{live && <div style={{ fontSize: 11.5 }}>{a.up ? t.mesh_up : t.mesh_down}</div>}</td>
              <td>
                <span className="mono" data-testid={`fleet-version-${short}`}>{ver || "—"}</span>
                {engine && <div className="hint" style={{ fontSize: 11.5 }}>{ENGINE_NAMES[engine] || engine}{engineVer ? ` ${engineVer}` : ""}</div>}
              </td>
              {actions && (
                <td>
                  <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
                    {onEdit && live && (
                      <Btn variant="outline" size="sm" icon="sliders" data-testid={`fleet-edit-${short}`}
                        onClick={() => onEdit(a)}>{t.mesh_fleet_edit}</Btn>
                    )}
                    {onUnenrol && live && (
                      <Btn variant="ghost" size="sm" icon="x" data-testid={`fleet-unenrol-${short}`}
                        onClick={() => onUnenrol(a.instance_uid)}>{t.mesh_fleet_unenrol}</Btn>
                    )}
                    {onForget && (
                      <Btn variant="ghost" size="sm" icon="trash" data-testid={`fleet-forget-${short}`}
                        onClick={() => onForget(a)}>{t.mesh_fleet_forget}</Btn>
                    )}
                  </div>
                </td>
              )}
            </tr>
          );
        })}
      </tbody>
    </table>
  );
}

function fmtTs(ts, lang) {
  if (!ts) return "—";
  try { return new Date(ts * 1000).toLocaleString(lang === "en" ? "en-GB" : lang === "es" ? "es" : "pt-BR"); }
  catch { return String(ts); }
}

/* An agent's editor: the facts the host reported, the policy it runs (the
   one place it is picked — and every agent runs one), and what that policy
   brings — its endpoints, its integrations, inputs and values — read off the
   stored policy document so the operator sees the dependencies BEFORE
   deploying. "Deploy" records the assignment; nothing is pushed, the agent
   applies on its next check-in (ADR-068). A policy without both endpoints
   cannot be deployed to a host: it has nowhere to send. */
function AgentDialog({ open, onClose, t, lang, d, agent, policies, settings, onDone, onToast }) {
  const [policy, setPolicy] = useState("");
  const [doc, setDoc] = useState(null);
  const [docErr, setDocErr] = useState(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState(null);
  useEffect(() => {
    if (!open || !agent) return;
    setPolicy(agent.policy_id || ""); setErr(null);
  }, [open, agent && agent.instance_uid]);
  useEffect(() => {
    if (!open || !policy) { setDoc(null); setDocErr(null); return undefined; }
    let alive = true;
    setDoc(null); setDocErr(null);
    API.meshPolicy(d.id, policy)
      .then(x => { if (alive) setDoc(x); })
      .catch(e => { if (alive) setDocErr(e.message); });
    return () => { alive = false; };
  }, [open, d.id, policy]);
  if (!open || !agent) return null;
  const attrs = agent.attributes || {};
  const host = agent.hostname || agent.instance_uid.slice(0, 12);
  const dirty = (policy || "") !== (agent.policy_id || "");
  const body = (doc && doc.body) || {};
  const acts = body.activations || [];
  const eps = doc ? policyEndpoints(body.endpoints || {}, settings) : null;
  const kv = (k, v, testid) => (
    <div className="kvrow"><span className="k">{k}</span><span className="v mono" data-testid={testid}>{v}</span></div>
  );
  async function deploy() {
    setBusy(true); setErr(null);
    try {
      await API.meshAssignPolicy(d.id, agent.instance_uid, policy);
      if (onToast) onToast(t.mesh_agent_deployed.replace("{policy}", policy).replace("{host}", host), true);
      if (onDone) onDone();
      if (onClose) onClose();
    } catch (e) { setErr(e.message); }
    finally { setBusy(false); }
  }
  return (
    <Dialog open={open} onClose={busy ? undefined : onClose} width={600} busy={busy} testid="agent-dialog"
      title={t.mesh_agent_dlg_title.replace("{host}", host)}>
      <div style={{ display: "flex", gap: 8, alignItems: "center", marginBottom: 12 }}>
        <AgentPill status={agent.status} t={t} />
        <span className="hint">{agent.up ? t.mesh_up : t.mesh_down} · {seenAgo(agent.last_seen, t)}</span>
      </div>

      <h4 className="section-title">{t.mesh_agent_facts_h}</h4>
      {kv(t.mesh_agent_uid, agent.instance_uid, "agent-uid")}
      {kv(t.mesh_agent_os, [attrs["velox.os"], attrs["velox.arch"]].filter(Boolean).join(" / ") || "—")}
      {kv(t.mesh_agent_version, attrs["velox.agent.version"] || "—")}
      {kv(t.mesh_agent_enrolled, fmtTs(agent.enrolled_at, lang))}
      {kv(t.mesh_fleet_rev, t.mesh_agent_rev
        .replace("{applied}", agent.applied_revision != null ? agent.applied_revision : "—")
        .replace("{current}", agent.current_revision != null ? agent.current_revision : "—"), "agent-rev")}
      {agent.config_error && (
        <Banner tone="danger" title={t.mesh_agent_cfg_err} style={{ marginTop: 8 }}>
          <span className="mono" style={{ fontSize: 12 }}>{agent.config_error}</span>
        </Banner>
      )}

      <h4 className="section-title" style={{ marginTop: 16 }}>{t.mesh_agent_policy_h}</h4>
      <p className="hint" style={{ margin: "0 0 8px" }}>{t.mesh_agent_policy_p}</p>
      <select className="select mono" value={policy} aria-label={t.mesh_fleet_policy} data-testid="agent-policy"
        onChange={e => setPolicy(e.target.value)} disabled={busy} style={{ maxWidth: 320 }}>
        {!agent.policy_id && <option value="">{t.mesh_agent_no_policy}</option>}
        {(policies || []).map(p => <option key={p.id} value={p.id}>{p.id}{p.name && p.name !== p.id ? ` — ${p.name}` : ""}</option>)}
      </select>

      <h4 className="section-title" style={{ marginTop: 16 }}>{t.mesh_agent_deps_h}</h4>
      <p className="hint" style={{ margin: "0 0 8px" }}>{t.mesh_agent_deps_p}</p>
      <div data-testid="agent-deps">
        {!policy && <p className="hint">{t.mesh_agent_no_policy}</p>}
        {policy && !doc && !docErr && <p className="hint">{t.mesh_agent_deps_loading}</p>}
        {docErr && <Banner tone="danger" title={t.mesh_save_err}>{docErr}</Banner>}
        {doc && (
          <>
            <p className="hint mono" style={{ margin: "0 0 6px" }}>{policy} · {t.mesh_revision.replace("{n}", doc.revision)}</p>
            <div className="mesh-card" style={{ marginBottom: 8 }} data-testid="agent-endpoints" data-complete={eps && eps.complete ? "1" : "0"}>
              <div className="mesh-card-head"><b>{t.mesh_agent_ep_h}</b></div>
              <PolicyEndpointsLine t={t} ids={body.endpoints || {}} settings={settings} />
              {eps && !eps.complete && <span className="hint" style={{ fontSize: 12, color: "var(--danger-text)" }}>{t.mesh_add_agent_incomplete}</span>}
            </div>
            {acts.length === 0 && <p className="hint">{t.mesh_agent_deps_none}</p>}
            {acts.map(a => {
              const inputs = Object.entries(a.inputs || {});
              return (
                <div key={a.package_id} className="mesh-card on" style={{ marginBottom: 8 }} data-testid={`agent-dep-${a.package_id}`}>
                  <div className="mesh-card-head">
                    <b className="mono">{a.package_id}</b>
                    <span className="badge mono">{a.package_version}</span>
                  </div>
                  {inputs.length === 0 && <span className="hint" style={{ fontSize: 12 }}>{t.mesh_agent_dep_defaults}</span>}
                  {inputs.length > 0 && (
                    <div className="mesh-chips">
                      {inputs.map(([id, v]) => (
                        <span key={id} className="mesh-chip on" title={Object.entries((v && v.vars) || {}).map(([k, x]) => `${k}=${x}`).join("\n")}>
                          {id}{Object.keys((v && v.vars) || {}).length ? ` · ${Object.entries(v.vars).map(([k, x]) => `${k}=${x}`).join(", ")}` : ""}
                        </span>
                      ))}
                    </div>
                  )}
                </div>
              );
            })}
          </>
        )}
      </div>

      {err && <Banner tone="danger" title={t.mesh_save_err} style={{ marginTop: 10 }}>{err}</Banner>}
      <div className="dialog-actions">
        <Btn variant="ghost" onClick={onClose} disabled={busy}>{t.cancel}</Btn>
        <Btn variant="primary" icon="check" disabled={busy || !dirty || !policy || !(eps && eps.complete)} data-testid="agent-deploy" onClick={deploy}>
          {busy ? t.mesh_agent_deploying : t.mesh_agent_deploy}
        </Btn>
      </div>
    </Dialog>
  );
}

/* The guided first run. The order is a product rule, not a layout: a policy
   with no controller to hold it, or an agent with no policy to run, is a
   dead end. The Mesh decides which step is live (`fleet::onboarding`); this
   only draws it. Lives on the settings sub-tab, beside the Mesh's state. */
const ONB_STEPS = ["install_mesh", "create_policy", "enrol_agent", "await_first_apply"];

function Onboarding({ t, step, onTab }) {
  if (step === "done") {
    return <Banner tone="ok" title={t.mesh_onb_h} testid="mesh-onboarding-done" style={{ marginBottom: 18 }}>{t.mesh_onb_done}</Banner>;
  }
  const at = ONB_STEPS.indexOf(step);
  const labels = [t.mesh_j1_h, t.mesh_j3_h, t.mesh_j2_h, t.mesh_j4_h];
  const goes = [null, "policies", "add-agent", "policies"];
  return (
    <div className="onb card pad" data-testid="mesh-onboarding" data-step={step} style={{ marginBottom: 18 }}>
      <h3 className="ag-h">{t.mesh_onb_h}</h3>
      <p className="hint" style={{ margin: "0 0 14px", maxWidth: "70ch" }}>{t.mesh_onb_p}</p>
      <ol className="onb-steps">
        {labels.map((label, i) => {
          const state = i < at ? "ok" : i === at ? "now" : "todo";
          return (
            <li key={label} className={`onb-step ${state}`} data-state={state}>
              <span className="onb-n">{state === "ok" ? <Icon name="check" size={12} /> : i + 1}</span>
              <span className="onb-label">{label}</span>
              {state === "now" && <span className="badge yellow">{t.mesh_step_now}</span>}
              {state === "ok" && <span className="hint">{t.mesh_step_ok}</span>}
              {state === "now" && goes[i] && (
                <Btn variant="ghost" size="sm" onClick={() => onTab(goes[i])}
                  data-testid={`onb-go-${goes[i]}`}>{goes[i] === "add-agent" ? t.mesh_add_agent : t[`mesh_tab_${goes[i]}`]}</Btn>
              )}
            </li>
          );
        })}
      </ol>
    </div>
  );
}

/* A multi-line command with its own copy button. `Copyable` in ui.jsx is for
   single-line values (it ellipsises and never wraps); a repository stanza
   needs its lines kept. */
function CmdBlock({ text, t, onToast, testid }) {
  const [state, setState] = useState("idle");
  async function copy() {
    const ok = await copyText(text);
    setState(ok ? "copied" : "failed");
    if (onToast) onToast(ok ? t.copied : t.copy_failed, ok);
    setTimeout(() => setState("idle"), 1500);
  }
  return (
    <div className="cmdblock" data-testid={testid}>
      <pre className="codeblock">{text}</pre>
      <button type="button" className="cmd-copy" onClick={copy}
        title={state === "failed" ? t.copy_failed : t.copy} aria-label={t.copy}>
        <Icon name={state === "copied" ? "check" : "copy"} size={13} />
      </button>
    </div>
  );
}

function Step({ n, title, soon, t, children }) {
  return (
    <section className="ag-step">
      <div className="ag-step-head">
        <span className="ag-step-n">{n}</span>
        <h3>{title}</h3>
        {soon && <span className="badge yellow">{t.mesh_soon}</span>}
      </div>
      <div className="ag-step-body">{children}</div>
    </section>
  );
}

function DocsLink({ href, t }) {
  return (
    <a className="btn btn-ghost" href={href} target="_blank" rel="noreferrer">
      <Icon name="book" size={15} />{t.mesh_docs}<Icon name="ext" size={13} />
    </a>
  );
}

/* The three states of the controller, as banners. Shared by the deployment
   tab and the Settings block (which carries the install commands). A
   tenant's status has no URL and no error: the core strips them (ADR-072
   §7), so the texts fall back to the fact. `ready` = also show the green
   "Mesh ready" confirmation; the overview shows only the two problems. */
function MeshStatusBanners({ t, status, ready = true }) {
  const notConfigured = !status || !status.configured;
  const unreachable = status && status.configured && status.reachable === false;
  return (
    <>
      {notConfigured && (
        <Banner tone="warn" title={t.mesh_absent_h} testid="mesh-absent" style={{ marginBottom: 18 }}>
          {t.mesh_absent_p} {t.mesh_absent_settings}
        </Banner>
      )}
      {unreachable && (
        <Banner tone="danger" title={t.mesh_unreachable_h} testid="mesh-unreachable" style={{ marginBottom: 18 }}>
          {t.mesh_unreachable_p.replace("{url}", status.url || "—")}
          {status.error && <div className="mono" style={{ marginTop: 6, fontSize: 12 }}>{status.error}</div>}
        </Banner>
      )}
      {ready && status && status.reachable === true && (
        <Banner tone="ok" title={t.mesh_ready_h} testid="mesh-ready" style={{ marginBottom: 18 }}>
          {t.mesh_ready_p.replace("{url}", status.url || "—")}
        </Banner>
      )}
    </>
  );
}

/* Installing the controller is an INSTALLATION step (one Mesh serves every
   deployment, ADR-072 §1), so this sits under Settings for the admin. The
   button (ADR-072 amendment 5) has the core apply deploy/mesh/mesh.yaml into
   the `velox-mesh` namespace — database, Secret, workload — and find it; a
   second press reapplies (an upgrade to the image the core names). The
   kubectl way stays, for a GitOps install. */
function MeshInstallGuide({ lang, onToast }) {
  const t = STR[lang] || STR.pt;
  const [status, setStatus] = useState(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState(null);
  const [tick, setTick] = useState(0);
  const [removing, setRemoving] = useState(false);
  const [purge, setPurge] = useState(false);
  useEffect(() => {
    let alive = true;
    API.meshStatus()
      .then(s => { if (alive) setStatus(s); })
      .catch(() => { if (alive) setStatus({ configured: false }); });
    return () => { alive = false; };
  }, [tick]);
  // Right after an install the pod is still pulling and starting: re-read
  // until it answers, so the banner turns green on its own.
  useEffect(() => {
    if (!status || !status.configured || status.reachable !== false || status.source !== "installed") return undefined;
    const h = setTimeout(() => setTick(x => x + 1), 5000);
    return () => clearTimeout(h);
  }, [status]);
  async function install() {
    setBusy(true); setErr(null);
    try {
      const s = await API.meshInstall();
      setStatus(s);
      if (onToast) onToast(t.mesh_install_ok, true);
    } catch (e) { setErr(e.message); }
    finally { setBusy(false); }
  }
  async function uninstall() {
    setBusy(true); setErr(null);
    try {
      const s = await API.meshUninstall(purge);
      setStatus(s); setRemoving(false); setPurge(false);
      if (onToast) onToast(purge ? t.mesh_uninstall_purged : t.mesh_uninstall_ok, true);
    } catch (e) { setErr(e.message); setRemoving(false); }
    finally { setBusy(false); }
  }
  const viaEnv = status && status.source === "env";
  const installed = status && status.source === "installed";
  return (
    <div data-testid="mesh-install-guide">
      <MeshStatusBanners t={t} status={status} />
      <Step n={1} title={t.mesh_j1_h} t={t}>
        <p className="hint" style={{ margin: "0 0 10px" }}>{t.mesh_install_p}</p>
        {viaEnv ? (
          <p className="hint" data-testid="mesh-install-env">{t.mesh_install_env}</p>
        ) : (
          <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
            <Btn variant={installed ? "outline" : "primary"} icon={installed ? "activity" : "download"} disabled={busy || !status}
              data-testid="mesh-install" onClick={install}>
              {busy && !removing ? t.mesh_installing : installed ? t.mesh_reinstall_btn : t.mesh_install_btn}
            </Btn>
            {installed && (
              <Btn variant="ghost" icon="trash" disabled={busy} data-testid="mesh-uninstall" onClick={() => setRemoving(true)}>
                {t.mesh_uninstall_btn}
              </Btn>
            )}
          </div>
        )}
        <Confirm open={removing} busy={busy} title={t.mesh_uninstall_h} body={t.mesh_uninstall_p}
          confirmLabel={purge ? t.mesh_uninstall_purge_btn : t.mesh_uninstall_btn} cancelLabel={t.cancel}
          confirmTestid="mesh-uninstall-confirm" onConfirm={uninstall} onCancel={() => { setRemoving(false); setPurge(false); }}
          requireText={purge ? "velox-mesh" : undefined} requireLabel={purge ? t.mesh_uninstall_type : undefined}>
          <label style={{ display: "flex", gap: 8, alignItems: "flex-start", fontSize: 13.5, margin: "4px 0 10px" }}>
            <input type="checkbox" checked={purge} onChange={e => setPurge(e.target.checked)} data-testid="mesh-uninstall-purge" />
            <span>{t.mesh_uninstall_purge}</span>
          </label>
        </Confirm>
        {err && <Banner tone="danger" title={t.mesh_install_err} style={{ marginTop: 10 }} testid="mesh-install-err">{err}</Banner>}
        <details style={{ marginTop: 14 }}>
          <summary className="hint" style={{ cursor: "pointer" }}>{t.mesh_install_manual}</summary>
          <div style={{ marginTop: 10 }}>
            <CmdBlock t={t} onToast={onToast} text={[
              "# Velox Mesh runs in its own namespace, velox-mesh.",
              "# Database + Secret first: see the header of deploy/mesh/mesh.yaml.",
              "kubectl apply -f deploy/mesh/mesh.yaml",
              "kubectl -n velox-mesh rollout status deploy/velox-mesh",
              "",
              "# Nothing to wire: this console finds it by name.",
            ].join("\n")} />
          </div>
        </details>
        <p className="hint" style={{ marginTop: 8 }}>{t.mesh_j1_note}</p>
      </Step>
    </div>
  );
}

/* ---- the host install guide --------------------------------------------
   Three ways onto a host (ADR-069 amendment 1): the script (recommended),
   the container image, or the per-distribution procedure. Rendered on the
   add-agent page with the policy already chosen, so every command that
   enrols is complete once a token has been minted, and a placeholder before.
   Exactly one block per panel carries `enroll-cmd`: the one that enrols. -- */
const METHODS = ["script", "docker", "manual"];

function InstallGuide({ t, target, onTarget, meshUrl, insecure, token, onToast }) {
  const [method, setMethod] = useState("script");
  const mesh = meshUrl || "MESH_URL";
  const tok = token || "TOKEN";
  const placeholders = (!meshUrl || !token) && <p className="hint" style={{ marginTop: 8 }}>{t.mesh_placeholder_note}</p>;
  const insecureNote = insecure && <p className="hint" style={{ marginTop: 8 }} data-testid="enroll-insecure-note">{t.mesh_enroll_insecure}</p>;
  return (
    <>
      <Tabs items={METHODS.map(k => ({ key: k, label: t[`mesh_method_${k}`] }))}
        value={method} onChange={setMethod} label={t.mesh_method_label} testidPrefix="install-method" />
      {method === "script" && (
        <div className="ag-panel" data-testid="install-panel-script">
          <p className="hint" style={{ margin: "0 0 14px", maxWidth: "75ch" }}>{t.mesh_script_p}</p>
          <Step n={1} title={t.mesh_step_script} t={t}>
            <CmdBlock testid="enroll-cmd" t={t} onToast={onToast} text={scriptCmd(mesh, tok, insecure)} />
            {placeholders}{insecureNote}
            <p className="hint" style={{ marginTop: 8 }}>{t.mesh_script_layout}</p>
            <p className="hint" style={{ marginTop: 4 }}>{t.mesh_script_flags}</p>
          </Step>
          <Step n={2} title={t.mesh_step_verify} t={t}>
            <p className="hint" style={{ margin: "0 0 8px" }}>{t.mesh_step_verify_p}</p>
            <CmdBlock text={"sudo velox-agent doctor\nsystemctl status velox-agent"} t={t} onToast={onToast} />
          </Step>
        </div>
      )}
      {method === "docker" && (
        <div className="ag-panel" data-testid="install-panel-docker">
          <p className="hint" style={{ margin: "0 0 14px", maxWidth: "75ch" }}>{t.mesh_docker_p}</p>
          <Step n={1} title={t.mesh_step_docker} t={t}>
            <CmdBlock testid="enroll-cmd" t={t} onToast={onToast} text={dockerCmd(mesh, tok, insecure)} />
            {placeholders}{insecureNote}
            <p className="hint" style={{ marginTop: 8 }}>{t.mesh_docker_compose}</p>
          </Step>
          <Step n={2} title={t.mesh_step_verify} t={t}>
            <CmdBlock text={"docker logs velox-agent\ndocker exec velox-agent /opt/velox-agent/bin/velox-agent doctor"} t={t} onToast={onToast} />
          </Step>
        </div>
      )}
      {method === "manual" && (
        <ManualGuide t={t} target={target} onTarget={onTarget} mesh={mesh} tok={tok}
          insecure={insecure} onToast={onToast} placeholders={placeholders} insecureNote={insecureNote} />
      )}
    </>
  );
}

/* Fluent Bit from the project's repository, then the agent — on a Linux
   host that is install.sh with the engine already in place, so the layout,
   user and unit are the script's either way — then `doctor`. */
function ManualGuide({ t, target, onTarget, mesh, tok, insecure, onToast, placeholders, insecureNote }) {
  const d = TARGETS.find(x => x.key === target) || TARGETS[0];
  let n = 0;
  return (
    <>
      <Banner tone="info" title={t.mesh_premise_h} style={{ margin: "0 0 14px" }}>
        <p style={{ margin: "0 0 10px" }}>{t.mesh_premise_p}</p>
        <div className="ag-facts">
          <span><b>{t.mesh_min_label}</b>: <span className="mono">{MIN_FLUENT_BIT}</span></span>
          <span className="hint">{t.mesh_min_why}</span>
        </div>
      </Banner>

      <Tabs items={TARGETS.map(x => ({ key: x.key, label: x.label }))}
        value={target} onChange={onTarget} label={t.mesh_target_label} testidPrefix="agents-tab" />

      <div className="ag-panel" data-testid={`agents-panel-${d.key}`}>
        {d.note && <p className="ag-note"><Icon name="info" size={14} /><span>{t[d.note]}</span></p>}

        {d.linkOnly ? (
          <Step n={++n} title={t.mesh_step_install} t={t}>
            <p className="hint" style={{ margin: "0 0 10px" }}>{t.mesh_linkonly_p}</p>
            <DocsLink href={d.docs} t={t} />
          </Step>
        ) : (
          <>
            <Step n={++n} title={t.mesh_step_repo} t={t}>
              <CmdBlock text={d.repo} t={t} onToast={onToast} />
              {!d.windows && (
                <p className="hint" style={{ marginTop: 8 }}>
                  {t.mesh_key_label}: <span className="mono">{FB_KEY_FPR}</span>
                </p>
              )}
            </Step>
            <Step n={++n} title={t.mesh_step_install} t={t}>
              <CmdBlock text={d.install} t={t} onToast={onToast} />
              {d.alt && (
                <div style={{ marginTop: 10 }}>
                  <p className="hint" style={{ marginBottom: 6 }}>{d.alt.label}</p>
                  <CmdBlock text={d.alt.cmd} t={t} onToast={onToast} />
                </div>
              )}
            </Step>
            <Step n={++n} title={t.mesh_step_check} t={t}>
              <p className="hint" style={{ margin: "0 0 8px" }}>
                {t.mesh_step_check_p.replace("{min}", MIN_FLUENT_BIT)}
              </p>
              <CmdBlock text={d.windows ? "fluent-bit.exe --version" : "fluent-bit --version"}
                t={t} onToast={onToast} />
            </Step>
          </>
        )}

        {d.host ? (
          <Step n={++n} title={t.mesh_step_agent} t={t}>
            <p className="hint" style={{ margin: "0 0 8px" }}>{t.mesh_agent_manual_p}</p>
            <CmdBlock testid="enroll-cmd" t={t} onToast={onToast} text={manualAgentCmd(mesh, tok, insecure)} />
            {placeholders}{insecureNote}
          </Step>
        ) : (
          <>
            <Step n={++n} title={t.mesh_step_agent} soon t={t}>
              <CmdBlock text={d.agent} t={t} onToast={onToast} />
            </Step>
            <Step n={++n} title={t.mesh_step_enroll} t={t}>
              <CmdBlock testid="enroll-cmd" t={t} onToast={onToast}
                text={(d.enroll || "sudo velox-agent enroll --mesh MESH_URL --token TOKEN").replace("MESH_URL", mesh).replace("TOKEN", tok) + (insecure ? " --insecure" : "")} />
              {placeholders}{insecureNote}
            </Step>
          </>
        )}
        <Step n={++n} title={t.mesh_step_verify} t={t}>
          <p className="hint" style={{ margin: "0 0 8px" }}>{t.mesh_step_verify_p}</p>
          <CmdBlock text={d.verify || "sudo velox-agent doctor"} t={t} onToast={onToast} />
        </Step>

        <div className="ag-foot">
          <p className="ag-note"><Icon name="info" size={14} />
            <span><b>{t.mesh_airgap_h}.</b> {t.mesh_airgap_p}</span>
          </p>
          <DocsLink href={d.docs || FB_DOWNLOADS} t={t} />
        </div>
      </div>
    </>
  );
}

/* "Add agent" is a page, in steps (not a dialog: an operator does this with
   a terminal open on the host and comes back to it). 1 — the policy: every
   agent runs one, and the token is the policy's; a policy without both
   endpoints cannot enrol hosts, the step says so and does not advance.
   2 — the token, shown once. 3 — the install steps for the host's system
   with the enrol command completed from the policy's Mesh endpoint and the
   token, and a watch on the fleet: the page says when a NEW host reports.
   `?policy=` (a policy card's button) starts on step 1 with it chosen. */
function AddAgentPage({ t, lang, d, overview, settings, initialPolicy, onBack, onToast }) {
  const policies = (overview && overview.policies) || [];
  const [step, setStep] = useState(0);
  const [policy, setPolicy] = useState(initialPolicy || (policies.length === 1 ? policies[0].id : ""));
  const [token, setToken] = useState(null);
  const [minting, setMinting] = useState(false);
  const [target, setTarget] = useState(TARGETS[0].key);
  // The fleet as it was when the page FIRST saw it (the overview may still
  // be loading when the page mounts from a URL): a host that is not in it
  // and reports later is the one being added.
  const [known, setKnown] = useState(null);
  useEffect(() => {
    if (overview && known === null) setKnown(new Set((overview.agents || []).map(a => a.instance_uid)));
  }, [overview, known]);
  useEffect(() => {
    if (!policy && policies.length === 1) setPolicy(policies[0].id);
  }, [policies.length]);
  const p = policies.find(x => x.id === policy) || null;
  const eps = policyEndpoints(p, settings);
  const arrived = known
    ? ((overview && overview.agents) || []).filter(a => !known.has(a.instance_uid) && a.status !== "unenrolled")
    : [];
  async function mint() {
    setMinting(true);
    try {
      const res = await API.meshEnrollmentToken(d.id, policy);
      setToken(res.token);
    } catch (e) { if (onToast) onToast(e.message, false); }
    finally { setMinting(false); }
  }
  const steps = [t.mesh_add_agent_s1, t.mesh_add_agent_s2, t.mesh_add_agent_s3];
  const canNext = step === 0 ? !!(p && eps.complete) : step === 1 ? !!token : true;
  return (
    <div className="ag-panel" data-testid="add-agent-page" data-step={step}>
      {/* Back goes top-LEFT: the top-right corner is the alert tray's
          (alerts.jsx), and a button under a standing popup is unreachable. */}
      <div style={{ marginBottom: 10 }}>
        <Btn variant="ghost" size="sm" icon="chevL" onClick={onBack} data-testid="add-agent-back">{t.mesh_add_agent_back}</Btn>
      </div>
      <div style={{ marginBottom: 14 }}>
        <h3 className="ag-h" style={{ marginBottom: 2 }}>{t.mesh_add_agent}</h3>
        <p className="hint" style={{ margin: 0, maxWidth: "70ch" }}>{t.mesh_add_agent_p}</p>
      </div>

      <div className="stepper" style={{ marginBottom: 16 }}>
        {steps.map((label, i) => (
          <span key={label} style={{ display: "contents" }}>
            <button type="button" className={`step ${i === step ? "active" : ""} ${i < step ? "done" : ""}`}
              style={{ background: "none", border: "none", padding: 0 }} disabled={i > step}
              data-testid={`add-agent-step-${i + 1}`} onClick={() => i < step && setStep(i)}>
              <span className="node">{i < step ? <Icon name="check" size={13} /> : i + 1}</span>
              <span className="txt">{label}</span>
            </button>
            {i < steps.length - 1 && <span className={`line ${i < step ? "done" : ""}`} />}
          </span>
        ))}
      </div>

      <div className="card pad">
        {step === 0 && (
          <div className="view-enter" data-testid="add-agent-policy-card">
            <h4 className="section-title">{t.mesh_add_agent_policy_h}</h4>
            <p className="hint" style={{ margin: "0 0 10px", maxWidth: "70ch" }}>{t.mesh_add_agent_policy_p}</p>
            {policies.length === 0 ? (
              <p className="hint" data-testid="add-agent-no-policies">{t.mesh_add_agent_no_policies}</p>
            ) : (
              <div style={{ display: "flex", gap: 10, alignItems: "center", flexWrap: "wrap" }}>
                <select className="select mono" value={policy} aria-label={t.mesh_fleet_policy} data-testid="add-agent-policy"
                  onChange={e => { setPolicy(e.target.value); setToken(null); }} style={{ maxWidth: 320 }}>
                  <option value="" disabled>{t.mesh_add_agent_pick}</option>
                  {policies.map(x => <option key={x.id} value={x.id}>{x.id}{x.name && x.name !== x.id ? ` — ${x.name}` : ""}</option>)}
                </select>
                {p && <span className="badge mono">{t.mesh_revision.replace("{n}", p.current_revision)}</span>}
                {p && <span className="badge">{t.mesh_pol_integ_n.replace("{n}", (p.integrations || []).length)}</span>}
              </div>
            )}
            {p && (
              <div style={{ marginTop: 10 }}>
                <PolicyEndpointsLine t={t} ids={p} settings={settings} testid="add-agent-endpoints" />
                {!eps.complete && (
                  <Banner tone="warn" title={t.mesh_pol_incomplete} testid="add-agent-incomplete" style={{ marginTop: 8 }}>
                    {t.mesh_add_agent_incomplete}
                  </Banner>
                )}
              </div>
            )}
          </div>
        )}

        {step === 1 && (
          <div className="view-enter" data-testid="add-agent-token-card">
            <h4 className="section-title">{t.mesh_add_agent_token_h}</h4>
            <p className="hint" style={{ margin: "0 0 10px", maxWidth: "70ch" }}>{t.mesh_add_agent_token_p}</p>
            <div style={{ display: "flex", gap: 10, alignItems: "center", flexWrap: "wrap", marginBottom: 10 }}>
              <span className="badge mono">{policy}</span>
              <Btn variant="primary" size="sm" icon="check" disabled={minting} data-testid="add-agent-token" onClick={mint}>
                {token ? t.mesh_token_again : t.mesh_token_get}
              </Btn>
            </div>
            {token && (
              <div>
                <CmdBlock t={t} onToast={onToast} testid="add-agent-token-value" text={token} />
                <p className="hint" style={{ marginTop: 6 }}>{t.mesh_token_once}</p>
              </div>
            )}
          </div>
        )}

        {step === 2 && (
          <div className="view-enter" data-testid="add-agent-install-card">
            <h4 className="section-title">{t.mesh_add_agent_steps_h}</h4>
            <InstallGuide t={t} target={target} onTarget={setTarget} onToast={onToast}
              meshUrl={eps.mesh ? eps.mesh.url : null} insecure={!!(eps.mesh && eps.mesh.insecure)} token={token} />
            <div data-testid="add-agent-watch" data-seen={arrived.length ? "1" : "0"} style={{ marginTop: 16 }}>
              <Banner tone={arrived.length ? "ok" : "info"} title={arrived.length ? t.mesh_add_agent_seen_h : t.mesh_add_agent_watch_h}>
                {arrived.length
                  ? <span data-testid="add-agent-seen">{t.mesh_add_agent_seen_p.replace("{host}", arrived.map(a => a.hostname || a.instance_uid.slice(0, 12)).join(", "))}</span>
                  : <span>{t.mesh_add_agent_watch_p}</span>}
              </Banner>
            </div>
          </div>
        )}
      </div>

      <div className="dialog-actions" style={{ marginTop: 14 }}>
        {step > 0 && <Btn variant="ghost" onClick={() => setStep(step - 1)} data-testid="add-agent-prev">{t.back}</Btn>}
        {step < 2
          ? <Btn variant="primary" icon="arrowR" disabled={!canNext} onClick={() => setStep(step + 1)} data-testid="add-agent-next">{t.next}</Btn>
          : <Btn variant="primary" icon="check" onClick={onBack} data-testid="add-agent-done">{t.mesh_add_agent_done}</Btn>}
      </div>
    </div>
  );
}

/* Two selects over the deployment's configured endpoints. Used to create a
   policy and to rebind one; the latter is a new revision. */
function EndpointPickers({ t, settings, os, mesh, onOs, onMesh, disabled, prefix }) {
  const osEps = (settings && settings.os_endpoints) || [];
  const meshEps = (settings && settings.mesh_endpoints) || [];
  return (
    <div style={{ display: "grid", gap: 10, gridTemplateColumns: "repeat(auto-fit, minmax(240px, 1fr))" }}>
      <Field label={t.mesh_pol_os_endpoint} htmlFor={`${prefix}-os`}>
        <select className="select" id={`${prefix}-os`} value={os} onChange={e => onOs(e.target.value)} disabled={disabled} data-testid={`${prefix}-os-endpoint`}>
          <option value="" disabled>{t.mesh_add_agent_pick}</option>
          {osEps.map(e => <option key={e.id} value={e.id}>{epName(t, e)} · {e.host}</option>)}
        </select>
      </Field>
      <Field label={t.mesh_pol_mesh_endpoint} htmlFor={`${prefix}-mesh`}>
        <select className="select" id={`${prefix}-mesh`} value={mesh} onChange={e => onMesh(e.target.value)} disabled={disabled} data-testid={`${prefix}-mesh-endpoint`}>
          <option value="" disabled>{t.mesh_add_agent_pick}</option>
          {meshEps.map(e => <option key={e.id} value={e.id}>{epName(t, e)} · {e.url}</option>)}
        </select>
      </Field>
    </div>
  );
}

function PolicyEndpointsDialog({ open, onClose, t, d, policy, settings, onDone, onToast }) {
  const [os, setOs] = useState("");
  const [mesh, setMesh] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState(null);
  useEffect(() => {
    if (!open || !policy) return;
    setOs(policy.os_endpoint || ""); setMesh(policy.mesh_endpoint || ""); setErr(null);
  }, [open, policy && policy.id]);
  if (!open || !policy) return null;
  const dirty = os !== (policy.os_endpoint || "") || mesh !== (policy.mesh_endpoint || "");
  async function save() {
    setBusy(true); setErr(null);
    try {
      const r = await API.meshPolicyEndpoints(d.id, policy.id, os, mesh);
      if (onToast) onToast(t.mesh_pol_endpoints_saved.replace("{id}", policy.id).replace("{rev}", r.revision), true);
      if (onDone) onDone();
      if (onClose) onClose();
    } catch (e) { setErr(e.message); }
    finally { setBusy(false); }
  }
  return (
    <Dialog open={open} onClose={busy ? undefined : onClose} width={560} busy={busy} testid="policy-endpoints-dialog"
      title={t.mesh_pol_endpoints_dlg.replace("{id}", policy.id)}>
      <p className="hint" style={{ margin: "0 0 12px" }}>{t.mesh_pol_endpoints_p}</p>
      <EndpointPickers t={t} settings={settings} os={os} mesh={mesh} onOs={setOs} onMesh={setMesh} disabled={busy} prefix="policy-ep" />
      {err && <Banner tone="danger" title={t.mesh_save_err} style={{ marginTop: 10 }}>{err}</Banner>}
      <div className="dialog-actions">
        <Btn variant="ghost" onClick={onClose} disabled={busy}>{t.cancel}</Btn>
        <Btn variant="primary" icon="check" disabled={busy || !dirty || !os || !mesh} data-testid="policy-ep-save" onClick={save}>
          {busy ? t.mesh_saving : t.save}
        </Btn>
      </div>
    </Dialog>
  );
}

/* ---- settings: the endpoint lists ---------------------------------------
   One editor for both kinds. A row is name + address; the id is the name's
   slug, made unique on add. Removing a row the policies still name is
   refused by the server with the policy's name, and the row shows how many
   policies use it so the operator sees that before trying. --------------- */
function EndpointListEditor({ t, kind, items, onChange, usage, suggest, disabled, testid }) {
  const [name, setName] = useState("");
  const [addr, setAddr] = useState("");
  const addrKey = kind === "os" ? "host" : "url";
  const canAdd = name.trim() && addr.trim() && (kind !== "os" || !!osAddressOf(addr));
  function add() {
    if (!canAdd) return;
    let id = slugOf(name) || kind;
    const taken = new Set(items.map(e => e.id));
    let i = 2;
    const base = id;
    while (taken.has(id)) { id = `${base}-${i++}`; }
    onChange([...items, { id, name: name.trim(), [addrKey]: addr.trim() }]);
    setName(""); setAddr("");
  }
  return (
    <div data-testid={testid}>
      {items.length === 0 ? (
        <p className="hint" data-testid={`${testid}-empty`}>{t.mesh_ep_none}</p>
      ) : (
        <table className="fleet" style={{ marginBottom: 10 }}>
          <thead><tr><th>{t.mesh_ep_name}</th><th>{kind === "os" ? t.mesh_os_host : t.mesh_url}</th><th>{t.mesh_tab_policies}</th><th /></tr></thead>
          <tbody>
            {items.map(e => (
              <tr key={e.id} data-testid={`${testid}-row-${e.id}`}>
                <td>
                  {epName(t, e)} <span className="hint mono" style={{ fontSize: 11 }}>{e.id}</span>
                  {e.managed && <div><span className="pill sm" data-testid={`${testid}-row-${e.id}-auto`} title={t.mesh_ep_auto_hint}>{t.mesh_ep_auto}</span></div>}
                </td>
                <td className="mono">
                  {e[addrKey]}
                  {e.insecure && <div style={{ fontFamily: "var(--font-sans)" }}><span className="hint" data-testid={`${testid}-row-${e.id}-insecure`}>{t.mesh_ep_insecure}</span></div>}
                  {kind === "os" && <div style={{ fontFamily: "var(--font-sans)" }}><OsAddressHint t={t} value={e.host} testid={`${testid}-row-${e.id}-resolved`} /></div>}
                </td>
                <td className="hint">{t.mesh_ep_in_use.replace("{n}", (usage && usage[e.id]) || 0)}</td>
                <td style={{ textAlign: "right" }}>
                  {!e.managed && (
                    <Btn variant="ghost" size="sm" icon="trash" disabled={disabled} data-testid={`${testid}-remove-${e.id}`}
                      onClick={() => onChange(items.filter(x => x.id !== e.id))}>{t.mesh_ep_remove}</Btn>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      <div style={{ display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap" }}>
        <input className="input" value={name} onChange={e => setName(e.target.value)} placeholder={t.mesh_ep_name_ph}
          aria-label={t.mesh_ep_name} data-testid={`${testid}-name`} style={{ maxWidth: 200 }} disabled={disabled} />
        <input className="input mono" value={addr} onChange={e => setAddr(e.target.value)}
          placeholder={kind === "os" ? t.mesh_os_host_ph : t.mesh_url_ph}
          aria-label={kind === "os" ? t.mesh_os_host : t.mesh_url} data-testid={`${testid}-addr`} style={{ maxWidth: 320 }} disabled={disabled}
          onKeyDown={e => { if (e.key === "Enter") add(); }} />
        <Btn variant="outline" size="sm" icon="plus" disabled={disabled || !canAdd} data-testid={`${testid}-add`} onClick={add}>{t.mesh_ep_add}</Btn>
        {kind === "os" && <OsAddressHint t={t} value={addr} testid={`${testid}-resolved`} />}
        {suggest && (
          <Btn variant="ghost" size="sm" disabled={disabled} data-testid={`${testid}-suggest`}
            onClick={() => { if (!name.trim()) setName(t.mesh_ep_internal_name); setAddr(suggest); }}>{t.mesh_ep_use_internal}</Btn>
        )}
      </div>
    </div>
  );
}

/* The Mesh daemons: the controller this console talks to, and every Mesh
   address the deployment's hosts use, each with whether the control plane
   reaches it, and how many policies and agents depend on it. Shaped like
   the fleet table on purpose — same question, other side of the wire. */
function DaemonsTable({ t, status, settings, probes, policies, agents, onRefresh, probing }) {
  const meshEps = (settings && settings.mesh_endpoints) || [];
  const byPolicy = {};
  (agents || []).forEach(a => { if (a.policy_id && a.status !== "unenrolled") byPolicy[a.policy_id] = (byPolicy[a.policy_id] || 0) + 1; });
  const count = (id) => {
    const ps = (policies || []).filter(p => p.mesh_endpoint === id);
    return { policies: ps.length, agents: ps.reduce((n, p) => n + (byPolicy[p.id] || 0), 0) };
  };
  const state = (ok, err) => (
    <span className={`pill ${ok ? "green" : "red"} sm`} data-state={ok ? "ok" : "down"}>
      <span className="dot" />{ok ? t.mesh_daemon_reachable : `${t.mesh_daemon_unreachable}${err ? ` · ${err}` : ""}`}
    </span>
  );
  return (
    <div className="card pad" style={{ marginBottom: 18 }} data-testid="mesh-daemons">
      <div className="fleet-head">
        <div>
          <h3 className="section-title" style={{ marginBottom: 2 }}>{t.mesh_daemons_h}</h3>
          <p className="hint" style={{ margin: 0, maxWidth: "70ch" }}>{t.mesh_daemons_p}</p>
        </div>
        <Btn variant="outline" size="sm" icon="activity" disabled={probing} onClick={onRefresh} data-testid="mesh-daemons-refresh">
          {probing ? t.mesh_daemon_probing : t.mesh_daemon_refresh}
        </Btn>
      </div>
      <table className="fleet">
        <thead>
          <tr><th>{t.mesh_daemon_name}</th><th>{t.mesh_daemon_addr}</th><th>{t.mesh_daemon_state}</th><th>{t.mesh_daemon_policies}</th><th>{t.mesh_daemon_agents}</th></tr>
        </thead>
        <tbody>
          {status && status.configured && (
            <tr data-testid="mesh-daemon-controller" data-reachable={status.reachable ? "1" : "0"}>
              <td>{t.mesh_daemon_controller}</td>
              <td className="mono">{status.url || <span className="hint">{t.mesh_daemon_hidden}</span>}</td>
              <td>{state(status.reachable === true, null)}</td>
              <td className="mono">{(policies || []).length}</td>
              <td className="mono">{(agents || []).filter(a => a.status !== "unenrolled").length}</td>
            </tr>
          )}
          {meshEps.map(e => {
            const pr = (probes || []).find(x => x.id === e.id);
            const c = count(e.id);
            return (
              <tr key={e.id} data-testid={`mesh-daemon-${e.id}`} data-reachable={pr ? (pr.reachable ? "1" : "0") : ""}>
                <td>{epName(t, e)} <span className="hint mono" style={{ fontSize: 11 }}>{e.id}</span></td>
                <td className="mono">{e.url}</td>
                <td>{pr ? state(pr.reachable, pr.error) : <span className="hint">{t.mesh_daemon_probing}</span>}</td>
                <td className="mono">{c.policies}</td>
                <td className="mono">{c.agents}</td>
              </tr>
            );
          })}
        </tbody>
      </table>
      {meshEps.length === 0 && <p className="hint" style={{ marginTop: 8 }} data-testid="mesh-daemons-none">{t.mesh_daemons_none}</p>}
    </div>
  );
}

/* ---- Tab 4: the Mesh's state and the deployment's settings ----------------
   Where the controller's state, the guided first run and the endpoint lists
   live: the OpenSearch addresses hosts ship to and the Mesh addresses they
   enrol against (ADR-072 §5, amendment 2). Both lists are REQUIRED for a
   policy to exist; the screen says so. The controller's own installation
   stays the admin's, under Settings. ---------------------------------- */
function SettingsTab({ t, d, status, overview, settings, onSettings, onToast, onTab }) {
  const [osEps, setOsEps] = useState([]);
  const [meshEps, setMeshEps] = useState([]);
  const [saving, setSaving] = useState(false);
  const [err, setErr] = useState(null);
  const [probes, setProbes] = useState(null);
  const [probing, setProbing] = useState(false);
  const [probeTick, setProbeTick] = useState(0);
  useEffect(() => {
    if (!settings) return;
    setOsEps(settings.os_endpoints || []); setMeshEps(settings.mesh_endpoints || []);
  }, [settings]);
  // Probe the configured Mesh addresses on load, on demand and on a slow
  // cadence; the controller's own state comes with `status`.
  useEffect(() => {
    if (!settings || (settings.mesh_endpoints || []).length === 0) { setProbes([]); return undefined; }
    let alive = true;
    setProbing(true);
    API.meshEndpointsStatus(d.id)
      .then(p => { if (alive) setProbes(p || []); })
      .catch(() => { if (alive) setProbes([]); })
      .finally(() => { if (alive) setProbing(false); });
    const h = setInterval(() => setProbeTick(x => x + 1), 30000);
    return () => { alive = false; clearInterval(h); };
  }, [d.id, settings, probeTick]);

  const loaded = !!settings;
  const same = (a, b) => JSON.stringify(a || []) === JSON.stringify(b || []);
  const dirty = loaded && (!same(osEps, settings.os_endpoints) || !same(meshEps, settings.mesh_endpoints));
  const policies = (overview && overview.policies) || [];
  const osUsage = {}; const meshUsage = {};
  policies.forEach(p => {
    if (p.os_endpoint) osUsage[p.os_endpoint] = (osUsage[p.os_endpoint] || 0) + 1;
    if (p.mesh_endpoint) meshUsage[p.mesh_endpoint] = (meshUsage[p.mesh_endpoint] || 0) + 1;
  });
  const incomplete = loaded && ((settings.os_endpoints || []).length === 0 || (settings.mesh_endpoints || []).length === 0);

  async function save() {
    setSaving(true); setErr(null);
    try {
      const next = { os_endpoints: osEps, mesh_endpoints: meshEps };
      await API.meshSaveSettings(d.id, next);
      onSettings({ ...settings, ...next });
      if (onToast) onToast(t.mesh_settings_saved, true);
    } catch (e) { setErr(e.message); }
    finally { setSaving(false); }
  }
  return (
    <div className="ag-panel" data-testid="mesh-panel-settings">
      <MeshStatusBanners t={t} status={status} />
      {overview && <Onboarding t={t} step={overview.onboarding} onTab={onTab} />}
      {overview && (
        <DaemonsTable t={t} status={status} settings={settings} probes={probes} probing={probing}
          policies={policies} agents={overview.agents} onRefresh={() => setProbeTick(x => x + 1)} />
      )}

      <div className="card pad" data-testid="mesh-settings" style={{ marginBottom: 18 }}>
        <h3 className="section-title">{t.mesh_settings_h}</h3>
        <p className="hint" style={{ margin: "0 0 14px", maxWidth: "70ch" }}>{t.mesh_settings_p}</p>
        {incomplete && (
          <Banner tone="warn" title={t.mesh_settings_required_h} testid="mesh-settings-required" style={{ marginBottom: 14 }}>
            {t.mesh_settings_required_p}
          </Banner>
        )}

        <h4 className="section-title">{t.mesh_set_os_h}</h4>
        <p className="hint" style={{ margin: "0 0 10px", maxWidth: "70ch" }}>{t.mesh_set_os_p}</p>
        <EndpointListEditor t={t} kind="os" items={osEps} onChange={setOsEps} usage={osUsage}
          suggest={settings && settings.in_cluster_os_host} disabled={!loaded || saving} testid="mesh-os-endpoints" />

        <h4 className="section-title" style={{ marginTop: 18 }}>{t.mesh_set_mesh_h}</h4>
        <p className="hint" style={{ margin: "0 0 10px", maxWidth: "70ch" }}>{t.mesh_set_mesh_p}</p>
        <EndpointListEditor t={t} kind="mesh" items={meshEps} onChange={setMeshEps} usage={meshUsage}
          disabled={!loaded || saving} testid="mesh-mesh-endpoints" />
        <p className="hint" style={{ margin: "12px 0" }}>{t.mesh_set_ctrl_p}</p>

        {err && <Banner tone="danger" title={t.mesh_settings_err} style={{ marginBottom: 10 }}>{err}</Banner>}
        <Btn variant="primary" size="sm" disabled={!dirty || saving} data-testid="mesh-settings-save" onClick={save}>
          {saving ? t.mesh_saving : t.save}
        </Btn>
      </div>
    </div>
  );
}

/* ---- Tab 1: the fleet at a glance ----------------------------------------
   Status counts, the bar, the configured agents. The Mesh's own state and
   the guided first run live on the settings sub-tab; the overview only
   raises what blocks the fleet: no Mesh, a Mesh that does not answer, or
   endpoints not yet configured. ---------------------------------------- */
function OverviewTab({ t, lang, dep, status, overview, settings, onReload, onToast, onTab }) {
  const [editing, setEditing] = useState(null);
  const [filter, setFilter] = useState(null);
  const [forgetting, setForgetting] = useState(null);
  const [busy, setBusy] = useState(false);
  const agents = (overview && overview.agents) || [];
  const bucket = SUMMARY.find(b => b.key === filter);
  const shown = bucket ? agents.filter(a => bucket.of(a.status)) : agents;
  const forget = async () => {
    if (!forgetting) return;
    setBusy(true);
    try {
      await API.meshForgetAgent(dep.id, forgetting.instance_uid);
      if (onToast) onToast(t.mesh_forgot_toast.replace("{host}", forgetting.hostname || forgetting.instance_uid.slice(0, 12)), true);
      setForgetting(null);
      if (onReload) onReload();
    } catch (e) { if (onToast) onToast(e.message, false); }
    finally { setBusy(false); }
  };
  const needEps = settings && ((settings.os_endpoints || []).length === 0 || (settings.mesh_endpoints || []).length === 0);
  const unenrol = async (uid) => {
    try {
      await API.meshUnenrol(dep.id, uid);
      if (onToast) onToast(t.mesh_unenrolled_toast, true);
      if (onReload) onReload();
    } catch (e) { if (onToast) onToast(e.message, false); }
  };
  return (
    <div className="ag-panel" data-testid="mesh-panel-overview">
      <MeshStatusBanners t={t} status={status} ready={false} />
      {overview && needEps && (
        <Banner tone="warn" title={t.mesh_ov_need_ep_h} testid="mesh-endpoints-missing" style={{ marginBottom: 18 }}
          action={<Btn variant="outline" size="sm" icon="sliders" onClick={() => onTab("settings")} data-testid="mesh-goto-settings">{t.mesh_goto_settings}</Btn>}>
          {t.mesh_ov_need_ep_p}
        </Banner>
      )}

      {overview && (
        <>
          <FleetSummary t={t} agents={agents} policies={overview.policies} filter={filter} onFilter={setFilter} />
          <div className="fleet-head">
            <div>
              <h3 className="ag-h" style={{ marginBottom: 2 }}>{t.mesh_fleet_h}</h3>
              <p className="hint" style={{ margin: 0, maxWidth: "70ch" }}>{t.mesh_agents_p}</p>
            </div>
            <Btn variant="primary" size="sm" icon="plus" onClick={() => onTab("add-agent")} data-testid="mesh-add-agent">
              {t.mesh_add_agent}
            </Btn>
          </div>
          {bucket && (
            <p className="hint" data-testid="mesh-fleet-filter" style={{ margin: "0 0 8px" }}>
              {t.mesh_filter_showing.replace("{n}", shown.length).replace("{total}", agents.length)}{" "}
              <button type="button" className="linkbtn" onClick={() => setFilter(null)} data-testid="mesh-fleet-filter-clear">{t.mesh_filter_clear}</button>
            </p>
          )}
          <FleetTable t={t} lang={lang} agents={shown} onEdit={setEditing} onUnenrol={unenrol} onForget={setForgetting}
            emptyText={bucket ? t.mesh_filter_none : null} />
          <Confirm open={!!forgetting} busy={busy} title={t.mesh_forget_h}
            body={forgetting ? t.mesh_forget_p.replace("{host}", forgetting.hostname || forgetting.instance_uid.slice(0, 12)) : ""}
            confirmLabel={t.mesh_fleet_forget} cancelLabel={t.cancel} confirmTestid="fleet-forget-confirm"
            onConfirm={forget} onCancel={() => setForgetting(null)} />
          <AgentDialog open={!!editing} onClose={() => setEditing(null)} t={t} lang={lang} d={dep}
            agent={editing} policies={overview.policies} settings={settings} onDone={onReload} onToast={onToast} />
        </>
      )}
    </div>
  );
}

/* ---- Tab 3: the deployment's policies ----------------------------------
   A policy is what a group of hosts runs (ADR-073): its integrations, and
   the two endpoints it is bound to (ADR-072 amendment 2). One table — name,
   agents, revision, integrations — with the actions in a menu per row:
   endpoints (rebind, a new revision), settings (the policy's binding,
   rollout and integrations, each configured or removed through the SAME
   dialog the Data tab uses — the one onboarding), add integration, add
   agent (the add-agent page with the policy chosen) and remove (refused by
   the Mesh while a live agent runs it). "New policy" opens a dialog that
   asks for the name and both endpoints. ----------------------------------- */

/* Name + both endpoints. Shared by the tab's "New policy" button. */
function NewPolicyDialog({ open, onClose, t, d, settings, onDone, onToast }) {
  const osEps = (settings && settings.os_endpoints) || [];
  const meshEps = (settings && settings.mesh_endpoints) || [];
  const [name, setName] = useState("");
  const [os, setOs] = useState("");
  const [mesh, setMesh] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState(null);
  useEffect(() => {
    if (!open) return;
    setName(""); setErr(null);
    // One of each configured = chosen; more = the operator picks.
    setOs(osEps.length === 1 ? osEps[0].id : "");
    setMesh(meshEps.length === 1 ? meshEps[0].id : "");
  }, [open]);
  if (!open) return null;
  const id = slugOf(name) || "policy";
  async function create() {
    setBusy(true); setErr(null);
    try {
      await API.meshCreatePolicy(d.id, id, name.trim(), os, mesh);
      if (onToast) onToast(t.mesh_pol_created.replace("{id}", id), true);
      if (onDone) onDone();
      if (onClose) onClose();
    } catch (e) { setErr(e.message); }
    finally { setBusy(false); }
  }
  return (
    <Dialog open={open} onClose={busy ? undefined : onClose} width={600} busy={busy} testid="mesh-policy-new-dialog"
      title={t.mesh_pol_new_h}>
      <p className="hint" style={{ margin: "0 0 14px" }}>{t.mesh_pol_new_dlg_p}</p>
      <div style={{ display: "grid", gap: 12 }}>
        <label style={{ display: "grid", gap: 6 }}>
          <span className="section-title" style={{ margin: 0 }}>{t.mesh_policy_name}</span>
          <div style={{ display: "flex", gap: 10, alignItems: "center", flexWrap: "wrap" }}>
            <input className="input" value={name} onChange={e => setName(e.target.value)} disabled={busy} autoFocus
              placeholder={t.mesh_pol_new_ph} aria-label={t.mesh_policy_name} data-testid="mesh-policy-name" style={{ maxWidth: 320 }}
              onKeyDown={e => { if (e.key === "Enter" && name.trim() && os && mesh && !busy) create(); }} />
            {name.trim() && <span className="hint mono">{id}</span>}
          </div>
        </label>
        <EndpointPickers t={t} settings={settings} os={os} mesh={mesh} onOs={setOs} onMesh={setMesh} disabled={busy} prefix="mesh-policy" />
      </div>
      {err && <Banner tone="danger" title={t.mesh_save_err} style={{ marginTop: 12 }}>{err}</Banner>}
      <div className="dialog-actions">
        <Btn variant="ghost" onClick={onClose} disabled={busy}>{t.cancel}</Btn>
        <Btn variant="primary" icon="plus" disabled={busy || !name.trim() || !os || !mesh} data-testid="mesh-policy-create" onClick={create}>
          {busy ? t.mesh_saving : t.mesh_pol_create}
        </Btn>
      </div>
    </Dialog>
  );
}

/* Pick a package for a policy — from the registry's Spec 2.0 catalogue
   (ADR-074), the one the Data tab shows; the package's generated form then
   configures it with the policy fixed. A version this core cannot run is
   listed, disabled, with the version it needs. */
function PickIntegrationDialog({ open, onClose, t, lang, policy, items, catalogErr, catalogLoaded, onPick }) {
  if (!open || !policy) return null;
  const on = policy.integrations || [];
  const candidates = items.filter(it => !on.includes(it.id));
  const needs = (it) => (((it.versions || [])[0]) || {}).min_mesh_version || "?";
  return (
    <Dialog open={open} onClose={onClose} width={520} testid="policy-add-dialog"
      title={t.mesh_pol_add_integ_dlg.replace("{id}", policy.id)}>
      {catalogErr ? <p className="hint">{t.mesh_pol_catalog_err.replace("{err}", catalogErr)}</p>
        : !catalogLoaded ? <p className="hint">{t.integ_loading}</p>
          : candidates.length === 0 ? <p className="hint">{t.mesh_pol_all_added}</p>
            : (
              <select className="select" aria-label={t.mesh_pol_integ_pick} data-testid={`policy-add-pick-${policy.id}`} defaultValue=""
                onChange={e => { const it = items.find(x => x.id === e.target.value); if (it) onPick(it); }} style={{ width: "100%" }}>
                <option value="" disabled>{t.mesh_pol_integ_pick}</option>
                {candidates.map(it => (
                  <option key={it.id} value={it.id} disabled={!it.offered}>
                    {locText(it.title, lang, it.id)}{it.offered ? "" : ` — ${t.pkg_not_offered.replace("{v}", needs(it))}`}
                  </option>
                ))}
              </select>
            )}
      <div className="dialog-actions">
        <Btn variant="ghost" onClick={onClose}>{t.cancel}</Btn>
      </div>
    </Dialog>
  );
}

/* A policy's settings: what it is bound to, how its rollout stands, and its
   integrations — each configured or removed in place. */
function PolicySettingsDialog({ open, onClose, t, lang, policy, settings, rollout, agents, itemOf, busy,
  onEndpoints, onAddIntegration, onEditIntegration, onRemoveIntegration }) {
  if (!open || !policy) return null;
  const p = policy;
  const r = rollout || {};
  const on = p.integrations || [];
  return (
    <Dialog open={open} onClose={onClose} width={760} testid="policy-settings-dialog"
      title={t.mesh_pol_settings_dlg.replace("{id}", p.id)}>
      <div style={{ display: "flex", gap: 10, alignItems: "center", flexWrap: "wrap", marginBottom: 14 }}>
        <b>{p.name}</b><span className="hint mono">{p.id}</span>
        <span className="badge">{t.mesh_pol_agents.replace("{n}", agents)}</span>
        <span className="badge mono">{t.mesh_revision.replace("{n}", p.current_revision)}</span>
        {r.complete && <span className="badge green">{t.mesh_rollout_done}</span>}
        {r.updating > 0 && <span className="badge yellow">{t.mesh_rollout_wait.replace("{n}", r.updating)}</span>}
        {r.failed > 0 && <span className="badge red">{t.mesh_rollout_failed.replace("{n}", r.failed)}</span>}
      </div>

      <div className="section-title" style={{ marginBottom: 6 }}>{t.mesh_pol_endpoints_h}</div>
      <div style={{ display: "flex", gap: 10, alignItems: "center", flexWrap: "wrap", marginBottom: 16 }}>
        <PolicyEndpointsLine t={t} ids={p} settings={settings} testid={`policy-endpoints-${p.id}`} />
        <Btn variant="ghost" size="sm" icon="sliders" onClick={onEndpoints}>{t.mesh_pol_endpoints_change}</Btn>
      </div>

      <div className="section-title" style={{ marginBottom: 6 }}>{t.mesh_pol_integrations}</div>
      <div data-testid={`policy-integrations-${p.id}`} style={{ display: "grid", gap: 6 }}>
        {on.length === 0 && <span className="hint">{t.mesh_pol_none}</span>}
        {on.map(id => {
          const it = itemOf(id);
          const working = busy[`${p.id}/${id}`];
          return (
            <div key={id} className="mesh-card on" style={{ display: "flex", alignItems: "center", gap: 10, flexWrap: "wrap" }}
              data-testid={`policy-integ-${p.id}-${id}`}>
              <b className="mono">{id}</b>
              <span className="hint" style={{ marginRight: "auto" }}>{locText(it.title, lang, "")}</span>
              <Btn variant="outline" size="sm" icon="sliders" disabled={working} data-testid={`policy-integ-edit-${p.id}-${id}`}
                onClick={() => onEditIntegration(id)}>{t.mesh_pol_integ_edit}</Btn>
              <Btn variant="ghost" size="sm" icon="trash" disabled={working} data-testid={`policy-integ-remove-${p.id}-${id}`}
                onClick={() => onRemoveIntegration(id)}>{t.mesh_pol_integ_remove}</Btn>
            </div>
          );
        })}
      </div>
      <div className="dialog-actions">
        <Btn variant="outline" icon="plus" onClick={onAddIntegration} data-testid={`policy-settings-add-${p.id}`}>{t.mesh_pol_add_integ}</Btn>
        <Btn variant="primary" onClick={onClose} data-testid="policy-settings-close">{t.close}</Btn>
      </div>
    </Dialog>
  );
}

function PoliciesTab({ t, lang, d, overview, settings, onReload, onToast, onAddAgent, onTab }) {
  const [newOpen, setNewOpen] = useState(false);
  const [catalog, setCatalog] = useState(null);
  const [catalogErr, setCatalogErr] = useState(null);
  const [picking, setPicking] = useState(null);      // policy id: the package picker
  const [dialog, setDialog] = useState(null);        // the 1.x onboarding dialog (legacy activations)
  const [pkgDialog, setPkgDialog] = useState(null);  // a Spec 2.0 package's form (ADR-074)
  const [pkgs, setPkgs] = useState(null);
  const [epDialog, setEpDialog] = useState(null);    // policy id: rebind
  const [settingsOf, setSettingsOf] = useState(null); // policy id: settings
  const [removing, setRemoving] = useState(null);    // policy id: confirm
  const [removeBusy, setRemoveBusy] = useState(false);
  const [busy, setBusy] = useState({});
  const policies = (overview && overview.policies) || [];
  const byId = (id) => policies.find(p => p.id === id) || null;
  const agentsByPolicy = {};
  ((overview && overview.agents) || []).forEach(a => { if (a.policy_id && a.status !== "unenrolled") agentsByPolicy[a.policy_id] = (agentsByPolicy[a.policy_id] || 0) + 1; });
  const osEps = (settings && settings.os_endpoints) || [];
  const meshEps = (settings && settings.mesh_endpoints) || [];
  const haveEps = osEps.length > 0 && meshEps.length > 0;

  // The registry's Spec 2.0 catalogue is what "add integration" offers
  // (ADR-074). The 1.x catalogue is read only to name and edit activations a
  // policy carried before the switch.
  useEffect(() => {
    let alive = true;
    API.catalogV2(d.id, false)
      .then(v => { if (alive) { setPkgs(v); setCatalogErr(null); } })
      .catch(e => { if (alive) setCatalogErr(e.message); });
    API.catalog(d.id)
      .then(v => { if (alive) setCatalog(v); })
      .catch(() => {});
    return () => { alive = false; };
  }, [d.id]);
  const packages = (pkgs && pkgs.packages) || [];
  const legacy = (catalog && catalog.integrations) || [];
  const itemOf = (id) => packages.find(x => x.id === id) || legacy.find(x => x.id === id) || { id, title: null };
  const isPackage = (body, id) => (body.packages || []).some(a => a.package_id === id);

  const openEdit = async (pid, id) => {
    try {
      const doc = await API.meshPolicy(d.id, pid);
      const body = doc.body || {};
      if (isPackage(body, id)) {
        setPkgDialog({ item: itemOf(id), policy: pid, editing: true });
        return;
      }
      const act = (body.activations || []).find(a => a.package_id === id);
      setDialog({ item: itemOf(id), policy: pid, editing: true, initial: act ? { inputs: act.inputs || {} } : null });
    } catch (e) { if (onToast) onToast(e.message, false); }
  };
  const removeInteg = async (pid, id) => {
    setBusy(b => ({ ...b, [`${pid}/${id}`]: true }));
    try {
      const doc = await API.meshPolicy(d.id, pid);
      const body = doc.body || {};
      if (isPackage(body, id)) {
        // A Spec 2.0 activation leaves by saving the policy without it — a
        // new revision, like every change to what a policy collects.
        await API.meshSavePolicy({
          id: pid, name: body.name || pid, deployment: d.id, integrations: [],
          packages: (body.packages || []).filter(a => a.package_id !== id), secrets: {}, endpoints: body.endpoints || {},
        });
      } else {
        await API.removeIntegration(d.id, id, pid);
      }
      if (onToast) onToast(t.mesh_pol_integ_removed.replace("{id}", id).replace("{policy}", pid), true);
      if (onReload) onReload();
    } catch (e) { if (onToast) onToast(e.message, false); }
    finally { setBusy(b => ({ ...b, [`${pid}/${id}`]: false })); }
  };
  const removePolicy = async () => {
    setRemoveBusy(true);
    try {
      await API.meshDeletePolicy(d.id, removing);
      if (onToast) onToast(t.mesh_pol_removed.replace("{id}", removing), true);
      setRemoving(null);
      if (onReload) onReload();
    } catch (e) { if (onToast) onToast(e.message, false); }
    finally { setRemoveBusy(false); }
  };

  if (!overview) {
    return (
      <div className="ag-panel" data-testid="mesh-panel-policies">
        <Banner tone="warn" title={t.mesh_pol_save_h} testid="mesh-policy-save-blocked">
          {t.mesh_pol_save_p}
        </Banner>
      </div>
    );
  }

  return (
    <div className="ag-panel" data-testid="mesh-panel-policies">
      <AddIntegrationDialog open={!!dialog} onClose={() => setDialog(null)} lang={lang} d={d}
        item={dialog && dialog.item} policies={policies} meshReady
        fixedTarget={dialog && dialog.policy} editing={!!(dialog && dialog.editing)} initial={dialog && dialog.initial}
        agentsByPolicy={agentsByPolicy} onToast={onToast}
        onDone={() => { if (onReload) onReload(); }} />
      <AddPackageDialog open={!!pkgDialog} onClose={() => setPkgDialog(null)} lang={lang} d={d}
        item={pkgDialog && pkgDialog.item} policies={policies} agentsByPolicy={agentsByPolicy}
        fixedPolicy={pkgDialog && pkgDialog.policy} editing={!!(pkgDialog && pkgDialog.editing)}
        onToast={onToast} onDone={() => { if (onReload) onReload(); }} />
      <PolicyEndpointsDialog open={!!epDialog} onClose={() => setEpDialog(null)} t={t} d={d}
        policy={byId(epDialog)} settings={settings} onDone={onReload} onToast={onToast} />
      <NewPolicyDialog open={newOpen} onClose={() => setNewOpen(false)} t={t} d={d} settings={settings}
        onDone={onReload} onToast={onToast} />
      <PickIntegrationDialog open={!!picking} onClose={() => setPicking(null)} t={t} lang={lang} policy={byId(picking)}
        items={packages} catalogErr={catalogErr} catalogLoaded={!!pkgs}
        onPick={it => { const pid = picking; setPicking(null); setPkgDialog({ item: it, policy: pid, editing: false }); }} />
      <PolicySettingsDialog open={!!settingsOf && !dialog && !pkgDialog && !epDialog && !picking} onClose={() => setSettingsOf(null)}
        t={t} lang={lang} policy={byId(settingsOf)} settings={settings}
        rollout={(overview.rollout || {})[settingsOf]} agents={agentsByPolicy[settingsOf] || 0}
        itemOf={itemOf} busy={busy}
        onEndpoints={() => setEpDialog(settingsOf)} onAddIntegration={() => setPicking(settingsOf)}
        onEditIntegration={id => openEdit(settingsOf, id)} onRemoveIntegration={id => removeInteg(settingsOf, id)} />
      <Confirm open={!!removing} title={removing ? t.mesh_pol_remove_h.replace("{id}", removing) : ""}
        body={t.mesh_pol_remove_p} confirmLabel={t.mesh_pol_act_remove} cancelLabel={t.cancel} busy={removeBusy}
        confirmTestid="policy-remove-confirm" onCancel={() => setRemoving(null)} onConfirm={removePolicy} />

      <div className="fleet-head" style={{ marginBottom: 14 }}>
        <div>
          <h3 className="ag-h" style={{ marginBottom: 2 }}>{t.mesh_pol_list_h}</h3>
          <p className="hint" style={{ margin: 0, maxWidth: "70ch" }}>{t.mesh_pol_list_p}</p>
        </div>
        <Btn variant="primary" size="sm" icon="plus" disabled={!haveEps} onClick={() => setNewOpen(true)} data-testid="mesh-policy-new-open">
          {t.mesh_pol_new_h}
        </Btn>
      </div>

      {settings && !haveEps && (
        <Banner tone="warn" title={t.mesh_pol_need_ep_h} testid="mesh-policy-need-endpoints" style={{ marginBottom: 14 }}
          action={<Btn variant="outline" size="sm" icon="sliders" onClick={() => onTab("settings")}>{t.mesh_goto_settings}</Btn>}>
          {t.mesh_pol_need_ep_p}
        </Banner>
      )}

      {policies.length === 0 ? (
        <p className="hint" data-testid="mesh-policies-empty">{t.mesh_pol_empty}</p>
      ) : (
        <table className="fleet" data-testid="mesh-policies-table">
          <thead>
            <tr>
              <th>{t.mesh_pol_col_name}</th><th>{t.mesh_pol_col_agents}</th>
              <th>{t.mesh_pol_col_rev}</th><th>{t.mesh_pol_col_integ}</th>
              <th style={{ textAlign: "right" }}>{t.mesh_fleet_actions}</th>
            </tr>
          </thead>
          <tbody>
            {policies.map(p => {
              const r = (overview.rollout || {})[p.id] || {};
              const on = p.integrations || [];
              const eps = policyEndpoints(p, settings);
              const live = agentsByPolicy[p.id] || 0;
              return (
                <tr key={p.id} data-testid={`policy-${p.id}`} data-complete={eps.complete ? "1" : "0"}>
                  <td>
                    <b>{p.name || p.id}</b> <span className="hint mono" style={{ fontSize: 11.5 }}>{p.id}</span>
                    <div style={{ display: "flex", gap: 6, flexWrap: "wrap", marginTop: 4 }}>
                      {settings && !eps.complete && <span className="badge red" data-testid={`policy-endpoints-missing-${p.id}`}>{t.mesh_pol_incomplete}</span>}
                      {r.updating > 0 && <span className="badge yellow">{t.mesh_rollout_wait.replace("{n}", r.updating)}</span>}
                      {r.failed > 0 && <span className="badge red">{t.mesh_rollout_failed.replace("{n}", r.failed)}</span>}
                    </div>
                  </td>
                  <td className="tnum" data-testid={`policy-agents-${p.id}`}>{live}</td>
                  <td className="mono">{p.current_revision}</td>
                  <td data-testid={`policy-integ-list-${p.id}`}>
                    {on.length === 0 ? <span className="hint">{t.mesh_pol_none}</span> : (
                      <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
                        {on.map(id => <span key={id} className="badge green">{locText(itemOf(id).title, lang, id)}</span>)}
                      </div>
                    )}
                  </td>
                  <td style={{ textAlign: "right" }}>
                    <Menu label={t.mesh_fleet_actions} testid={`policy-menu-${p.id}`} items={[
                      { label: t.mesh_pol_act_endpoints, icon: "plug", testid: `policy-endpoints-edit-${p.id}`, onClick: () => setEpDialog(p.id) },
                      { label: t.mesh_pol_act_settings, icon: "sliders", testid: `policy-settings-${p.id}`, onClick: () => setSettingsOf(p.id) },
                      { label: t.mesh_pol_add_integ, icon: "plus", testid: `policy-add-${p.id}`, onClick: () => setPicking(p.id) },
                      { label: t.mesh_pol_add_agent, icon: "server", testid: `policy-add-agent-${p.id}`, disabled: !eps.complete, onClick: () => onAddAgent(p.id) },
                      { sep: true },
                      { label: live ? t.mesh_pol_remove_busy.replace("{n}", live) : t.mesh_pol_act_remove, icon: "trash", danger: true,
                        disabled: live > 0, testid: `policy-remove-${p.id}`, onClick: () => setRemoving(p.id) },
                    ]} />
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      )}
    </div>
  );
}

// The sub-screens: three tabs and the add-agent page (no tab of its own;
// reached from the overview, a policy card or the first-run guide).
const TABS = ["overview", "policies", "settings", "add-agent"];

/* The `mesh` tab of one deployment: `/deployments/:id/mesh/:sub`. */
function MeshTab({ d, lang, sub, onSub, onToast, initialPolicy }) {
  const t = STR[lang] || STR.pt;
  const [status, setStatus] = useState(null);
  const [overview, setOverview] = useState(null);
  const [settings, setSettings] = useState(null);
  const [tick, setTick] = useState(0);
  const active = TABS.includes(sub) ? sub : "overview";

  useEffect(() => {
    let alive = true;
    API.meshStatus()
      .then(s => { if (alive) setStatus(s); })
      .catch(() => { if (alive) setStatus({ configured: false }); });
    API.meshSettings(d.id)
      .then(s => { if (alive) setSettings(s || {}); })
      .catch(() => { if (alive) setSettings({}); });
    return () => { alive = false; };
  }, [d.id]);

  // THIS deployment's fleet (ADR-072), refreshed on a cadence a little under
  // the agents' own poll so "last seen" and a rollout move without the
  // operator reloading. No Mesh simply leaves `overview` null and the
  // screens say so.
  useEffect(() => {
    let alive = true;
    const load = () => API.meshOverview(d.id)
      .then(o => { if (alive) setOverview(o); })
      .catch(() => { if (alive) setOverview(null); });
    load();
    const h = setInterval(load, 10000);
    return () => { alive = false; clearInterval(h); };
  }, [d.id, tick]);

  const reload = () => setTick(x => x + 1);
  // The add-agent page, with the policy on the URL so a card's button and a
  // bookmark both land on step 1 with it chosen.
  const openAddAgent = (policyId) => onSub("add-agent", policyId ? { policy: policyId } : undefined);

  if (active === "add-agent") {
    return (
      <div className="view-enter" data-testid="mesh-tab">
        <AddAgentPage t={t} lang={lang} d={d} overview={overview} settings={settings}
          initialPolicy={initialPolicy || null} onBack={() => onSub("overview")} onToast={onToast} />
      </div>
    );
  }

  return (
    <div className="view-enter" data-testid="mesh-tab">
      <p className="lead" style={{ marginBottom: 14 }}>{t.mesh_dep_lead}</p>
      <Tabs
        items={[
          { key: "overview", label: t.mesh_tab_overview, icon: "grid" },
          { key: "policies", label: t.mesh_tab_policies, icon: "layers" },
          { key: "settings", label: t.mesh_tab_settings, icon: "sliders" },
        ]}
        value={active} onChange={onSub} label={t.tab_mesh} testidPrefix="mesh-tab" />

      {active === "overview" && (
        <OverviewTab t={t} lang={lang} dep={d} status={status} overview={overview} settings={settings}
          onReload={reload} onToast={onToast} onTab={onSub} />
      )}
      {active === "policies" && (
        <PoliciesTab t={t} lang={lang} d={d} overview={overview} settings={settings}
          onReload={reload} onToast={onToast} onAddAgent={openAddAgent} onTab={onSub} />
      )}
      {active === "settings" && (
        <SettingsTab t={t} d={d} status={status} overview={overview} settings={settings} onSettings={setSettings}
          onToast={onToast} onTab={onSub} />
      )}
    </div>
  );
}

export { MeshTab, MeshInstallGuide, MIN_FLUENT_BIT, TABS as MESH_TABS };
