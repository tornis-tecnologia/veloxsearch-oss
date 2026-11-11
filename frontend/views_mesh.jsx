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

     install the controller  →  install an agent on a host
                             →  create a collection policy
                             →  add integrations to the policy

   The last step is the one that has to mean something. Turning on "nginx"
   must bring everything that makes nginx logs *useful* — the collection
   stanza at the edge, the ingest pipeline that parses, the index template
   that maps, the index pattern that makes it visible and the dashboard that
   shows it. All five come out of the same signed package (ADR-039), so this
   screen reads them off `POST /api/integration_artifacts` rather than
   describing them from a table someone has to remember to update: a package
   that ships no pipeline is shown with no pipeline.

   Three tabs, matching the three questions an operator has:
     overview  — is there a Mesh? (not configured / unreachable / ready)
     agents    — how do I get collection onto a host?
     policies  — what will the agent actually do?

   The install tab mirrors the targets the Fluent Bit project itself
   documents (docs.fluentbit.io/manual/installation/downloads), including
   the ones we have no commands for (Buildroot, Yocto, macOS) — those link
   out rather than inventing a procedure. We ship no engine (ADR-069).

   Without a Mesh this tab composes a policy and shows exactly what it would
   carry; nothing is persisted and nothing is mocked. With one, the fleet is
   the product: `POST /api/mesh_overview` for THIS deployment, polled.
   ============================================================ */
import { useEffect, useState } from "react";
import { STR } from "./i18n.jsx";
import { API } from "./api.jsx";
import { Banner, Btn, Dialog, Field, Icon, Tabs, copyText } from "./ui.jsx";
import { AddIntegrationDialog, locText } from "./views_integration_add.jsx";

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

const APT_AGENT = "sudo apt-get install ./velox-agent_0.1.0_amd64.deb";
const RPM_AGENT = "sudo dnf install ./velox-agent-0.1.0.x86_64.rpm";

const TARGETS = [
  {
    key: "amazon", label: "Amazon Linux",
    docs: `${DOCS}/linux/amazon-linux`,
    repo: yumRepo("amazonlinux", "2023"), install: "sudo dnf install fluent-bit", agent: RPM_AGENT,
  },
  {
    key: "buildroot", label: "Buildroot", linkOnly: true,
    docs: `${DOCS}/linux/buildroot-embedded-linux`,
    note: "mesh_note_embedded", agent: "# build velox-agent for the target and install it with the image",
  },
  {
    key: "debian", label: "Debian",
    docs: `${DOCS}/linux/debian`, note: "mesh_note_no_pkg",
    repo: aptRepo("debian"), install: "sudo apt-get install fluent-bit", agent: APT_AGENT,
  },
  {
    key: "raspbian", label: "Raspbian · Raspberry Pi",
    docs: `${DOCS}/linux/raspbian-raspberry-pi`, note: "mesh_note_aptkey",
    repo: ["sudo sh -c 'curl https://packages.fluentbit.io/fluentbit.key | sudo apt-key add - '",
      'echo "deb https://packages.fluentbit.io/raspbian/bookworm bookworm main" | sudo tee /etc/apt/sources.list.d/fluent-bit.list',
      "sudo apt-get update"].join("\n"),
    install: "sudo apt-get install fluent-bit",
    agent: "sudo apt-get install ./velox-agent_0.1.0_arm64.deb",
  },
  {
    key: "redhat", label: "Red Hat · CentOS",
    docs: `${DOCS}/linux/redhat-centos`, note: "mesh_note_rhel9",
    repo: yumRepo("centos", "$releasever"), install: "sudo dnf install fluent-bit",
    alt: { label: "EPEL (RHEL 9)", cmd: "sudo dnf install epel-release\nsudo dnf install fluent-bit" },
    agent: RPM_AGENT,
  },
  {
    key: "rocky", label: "Rocky · Alma",
    docs: `${DOCS}/linux/alma-rocky`,
    repo: yumRepo("almalinux", "$releasever"), install: "sudo dnf install fluent-bit", agent: RPM_AGENT,
  },
  {
    key: "ubuntu", label: "Ubuntu",
    docs: `${DOCS}/linux/ubuntu`, note: "mesh_note_no_pkg",
    repo: aptRepo("ubuntu"), install: "sudo apt-get install fluent-bit", agent: APT_AGENT,
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

function FleetSummary({ t, agents, policies }) {
  const rows = agents || [];
  const counts = SUMMARY.map(b => ({ ...b, n: rows.filter(a => b.of(a.status)).length }));
  const total = rows.length;
  return (
    <div className="card pad" data-testid="mesh-summary" style={{ marginBottom: 18 }}>
      <div className="fleet-sum">
        <span className="total" data-testid="mesh-summary-total">
          {t.mesh_sum_total.replace("{n}", total).replace("{p}", (policies || []).length)}
        </span>
        {counts.map(b => (
          <span key={b.key} className={`chip ${b.tone}`} data-testid={`mesh-sum-${b.key}`} data-n={b.n}>
            <span className="dot" />{b.key === "unhealthy" ? t.mesh_sum_unhealthy : t[`mesh_st_${b.key}`]} <b>{b.n}</b>
          </span>
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

/* The deployment's fleet. Read-only on the overview; on the Agents tab each
   row opens the agent's editor (where the policy is picked) or unenrols.
   The policy is a word here, never a picker: moving a host is a decision
   with dependencies, and the editor shows them before "Deploy". */
function FleetTable({ t, agents, onEdit, onUnenrol, testid = "mesh-fleet" }) {
  if (!agents || agents.length === 0) return <p className="hint" data-testid={`${testid}-empty`}>{t.mesh_fleet_empty}</p>;
  const actions = !!(onEdit || onUnenrol);
  return (
    <table className="fleet" data-testid={testid}>
      <thead>
        <tr>
          <th>{t.mesh_fleet_host}</th><th>{t.mesh_fleet_status}</th>
          <th>{t.mesh_fleet_policy}</th><th>{t.mesh_fleet_rev}</th>
          <th>{t.mesh_fleet_seen}</th>
          {actions && <th>{t.mesh_fleet_actions}</th>}
        </tr>
      </thead>
      <tbody>
        {agents.map(a => {
          const short = a.instance_uid.slice(0, 8);
          const live = a.status !== "unenrolled";
          return (
            <tr key={a.instance_uid} data-testid={`fleet-row-${short}`}
              data-status={a.status} data-up={a.up ? "1" : "0"}>
              <td className="mono">{a.hostname || a.instance_uid.slice(0, 12)}</td>
              <td>
                <AgentPill status={a.status} t={t} />
                <span className="hint" style={{ marginLeft: 8 }}>{a.up ? t.mesh_up : t.mesh_down}</span>
                {a.config_error && (
                  <div className="mono fleet-err" title={a.config_error}>{a.config_error}</div>
                )}
              </td>
              <td className="mono" data-testid={`fleet-policy-${short}`}>{a.policy_id || "—"}</td>
              <td className="mono">
                {a.applied_revision != null ? a.applied_revision : "—"}
                {a.current_revision != null && a.applied_revision !== a.current_revision
                  && <span className="hint"> → {a.current_revision}</span>}
              </td>
              <td className="hint">{seenAgo(a.last_seen, t)}</td>
              {actions && (
                <td>
                  <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
                    {onEdit && live && (
                      <Btn variant="outline" size="sm" icon="sliders" data-testid={`fleet-edit-${short}`}
                        onClick={() => onEdit(a)}>{t.mesh_fleet_edit}</Btn>
                    )}
                    {onUnenrol && live && (
                      <Btn variant="ghost" size="sm" icon="trash" data-testid={`fleet-unenrol-${short}`}
                        onClick={() => onUnenrol(a.instance_uid)}>{t.mesh_fleet_unenrol}</Btn>
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
   one place it is picked), and what that policy brings — integrations,
   inputs, values — read off the stored policy document so the operator
   sees the dependencies BEFORE deploying. "Deploy" records the assignment;
   nothing is pushed, the agent applies on its next check-in (ADR-068). */
function AgentDialog({ open, onClose, t, lang, d, agent, policies, onDone, onToast }) {
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
  const acts = ((doc && doc.body) || {}).activations || [];
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
        <Btn variant="primary" icon="check" disabled={busy || !dirty || !policy} data-testid="agent-deploy" onClick={deploy}>
          {busy ? t.mesh_agent_deploying : t.mesh_agent_deploy}
        </Btn>
      </div>
    </Dialog>
  );
}

/* The guided first run. The order is a product rule, not a layout: a policy
   with no controller to hold it, or an agent with no policy to run, is a
   dead end. The Mesh decides which step is live (`fleet::onboarding`); this
   only draws it. */
const ONB_STEPS = ["install_mesh", "create_policy", "enrol_agent", "await_first_apply"];

function Onboarding({ t, step, onTab }) {
  if (step === "done") {
    return <Banner tone="ok" title={t.mesh_onb_h} testid="mesh-onboarding-done">{t.mesh_onb_done}</Banner>;
  }
  const at = ONB_STEPS.indexOf(step);
  const labels = [t.mesh_j1_h, t.mesh_j3_h, t.mesh_j2_h, t.mesh_j4_h];
  const goes = [null, "policies", "agents", "policies"];
  return (
    <div className="onb" data-testid="mesh-onboarding" data-step={step}>
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
                  data-testid={`onb-go-${goes[i]}`}>{t[`mesh_tab_${goes[i]}`]}</Btn>
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
function CmdBlock({ text, t, onToast }) {
  const [state, setState] = useState("idle");
  async function copy() {
    const ok = await copyText(text);
    setState(ok ? "copied" : "failed");
    if (onToast) onToast(ok ? t.copied : t.copy_failed, ok);
    setTimeout(() => setState("idle"), 1500);
  }
  return (
    <div className="cmdblock">
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

/* ---- Tab 1: is there a Mesh, and what is the journey? ---------------- */
/* The three states of the controller, as banners. Shared by the deployment
   tab (which can only point at Settings) and the Settings block (which
   carries the install commands). A tenant's status has no URL and no error:
   the core strips them (ADR-072 §7), so the texts fall back to the fact. */
function MeshStatusBanners({ t, status }) {
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
      {status && status.reachable === true && (
        <Banner tone="ok" title={t.mesh_ready_h} testid="mesh-ready" style={{ marginBottom: 18 }}>
          {t.mesh_ready_p.replace("{url}", status.url || "—")}
        </Banner>
      )}
    </>
  );
}

/* Installing the controller is an INSTALLATION step (one Mesh serves every
   deployment, ADR-072 §1), so this sits under Settings for the admin. */
function MeshInstallGuide({ lang, onToast }) {
  const t = STR[lang] || STR.pt;
  const [status, setStatus] = useState(null);
  useEffect(() => {
    let alive = true;
    API.meshStatus()
      .then(s => { if (alive) setStatus(s); })
      .catch(() => { if (alive) setStatus({ configured: false }); });
    return () => { alive = false; };
  }, []);
  return (
    <div data-testid="mesh-install-guide">
      <MeshStatusBanners t={t} status={status} />
      <Step n={1} title={t.mesh_j1_h} t={t}>
        <p className="hint" style={{ margin: "0 0 10px" }}>{t.mesh_j1_p}</p>
        <CmdBlock t={t} onToast={onToast} text={[
          "# Velox Mesh runs in the cluster, beside the control plane.",
          "kubectl apply -f https://get.veloxsearch.ai/mesh.yaml",
          "",
          "# Then point this console at it and restart the control plane:",
          "kubectl -n veloxsearch-system set env deploy/veloxsearch \\",
          "  VELOX_MESH_URL=http://velox-mesh.veloxsearch-system.svc:4320",
        ].join("\n")} />
        <p className="hint" style={{ marginTop: 8 }}>{t.mesh_j1_note}</p>
      </Step>
    </div>
  );
}

/* ---- Tab 4: the deployment's Mesh settings (ADR-072 §5) ---------------
   Where a host OUTSIDE the cluster reaches this deployment's OpenSearch
   and the Mesh itself. One value each per deployment, read at compile
   time (os_host) and into the enrolment commands (mesh_url), so they are
   a screen here and not fields on every policy or token. The controller's
   own installation stays the admin's, under Settings. -------------------- */
function SettingsTab({ t, d, status, settings, onSettings, onToast }) {
  const [osHost, setOsHost] = useState("");
  const [meshUrl, setMeshUrl] = useState("");
  const [saving, setSaving] = useState(false);
  const [err, setErr] = useState(null);
  useEffect(() => {
    if (!settings) return;
    setOsHost(settings.os_host || ""); setMeshUrl(settings.mesh_url || "");
  }, [settings]);
  const loaded = !!settings;
  const dirty = loaded && ((osHost.trim() || null) !== (settings.os_host || null)
    || (meshUrl.trim() || null) !== (settings.mesh_url || null));
  async function save() {
    setSaving(true); setErr(null);
    try {
      const next = { os_host: osHost.trim() || null, mesh_url: meshUrl.trim() || null };
      await API.meshSaveSettings(d.id, next);
      onSettings(next);
      if (onToast) onToast(t.mesh_settings_saved, true);
    } catch (e) { setErr(e.message); }
    finally { setSaving(false); }
  }
  return (
    <div className="ag-panel" data-testid="mesh-panel-settings">
      <MeshStatusBanners t={t} status={status} />
      <div className="card pad" data-testid="mesh-settings" style={{ marginBottom: 18 }}>
        <h3 className="section-title">{t.mesh_settings_h}</h3>
        <p className="hint" style={{ margin: "0 0 14px", maxWidth: "70ch" }}>{t.mesh_settings_p}</p>

        <h4 className="section-title">{t.mesh_set_os_h}</h4>
        <Field label={t.mesh_os_host} htmlFor="mesh-os-host" hint={t.mesh_os_host_hint}>
          <input className="input mono" id="mesh-os-host" value={osHost} onChange={e => setOsHost(e.target.value)}
            placeholder={t.mesh_os_host_ph} data-testid="mesh-policy-os-host" style={{ maxWidth: 360 }} disabled={!loaded || saving} />
        </Field>

        <h4 className="section-title" style={{ marginTop: 14 }}>{t.mesh_set_mesh_h}</h4>
        <Field label={t.mesh_url} htmlFor="mesh-url" hint={t.mesh_url_hint}>
          <input className="input mono" id="mesh-url" value={meshUrl} onChange={e => setMeshUrl(e.target.value)}
            placeholder={t.mesh_url_ph} data-testid="mesh-url" style={{ maxWidth: 360 }} disabled={!loaded || saving} />
        </Field>
        <p className="hint" style={{ margin: "0 0 12px" }}>{t.mesh_set_ctrl_p}</p>

        {err && <Banner tone="danger" title={t.mesh_settings_err} style={{ marginBottom: 10 }}>{err}</Banner>}
        <Btn variant="primary" size="sm" disabled={!dirty || saving} data-testid="mesh-settings-save" onClick={save}>
          {saving ? t.mesh_saving : t.save}
        </Btn>
      </div>
    </div>
  );
}

/* ---- Tab 1: the state of the Mesh and the fleet it runs ---------------- */
function OverviewTab({ t, d, status, overview, onTab }) {
  const agents = (overview && overview.agents) || [];
  return (
    <div className="ag-panel" data-testid="mesh-panel-overview">
      <MeshStatusBanners t={t} status={status} />

      {overview && (
        <>
          <FleetSummary t={t} agents={agents} policies={overview.policies} />
          <div style={{ marginBottom: 22 }}>
            <Onboarding t={t} step={overview.onboarding} onTab={onTab} />
            {overview.seats != null && (
              <p className="hint" style={{ margin: "10px 0 0" }} data-testid="mesh-seats">
                {t.mesh_seats.replace("{used}", overview.enrolled_total ?? 0).replace("{seats}", overview.seats)}
              </p>
            )}
          </div>

          <div className="card pad" style={{ marginBottom: 22 }} data-testid="mesh-overview-agents">
            <div className="fleet-head">
              <div>
                <h3 className="section-title" style={{ marginBottom: 2 }}>{t.mesh_ov_agents_h}</h3>
                <p className="hint" style={{ margin: 0, maxWidth: "70ch" }}>{t.mesh_ov_agents_p}</p>
              </div>
              <Btn variant="outline" size="sm" icon="server" onClick={() => onTab("agents")} data-testid="mesh-goto-agents">
                {t.mesh_tab_agents}
              </Btn>
            </div>
            {agents.length === 0
              ? <p className="hint" data-testid="mesh-ov-none">{t.mesh_ov_none}</p>
              : <FleetTable t={t} agents={agents} testid="mesh-overview-fleet" />}
          </div>
        </>
      )}

      {(!overview || agents.length === 0) && (
        <>
          <h3 className="ag-h">{t.mesh_journey_h}</h3>
          <p className="hint" style={{ margin: "0 0 18px", maxWidth: "70ch" }}>{t.mesh_journey_p}</p>
          <Step n={1} title={t.mesh_j2_h} t={t}>
            <p className="hint" style={{ margin: "0 0 10px" }}>{t.mesh_j2_p}</p>
            {!overview && (
              <Btn variant="ghost" icon="server" onClick={() => onTab("agents")} data-testid="mesh-goto-agents">
                {t.mesh_tab_agents}
              </Btn>
            )}
          </Step>
          <Step n={2} title={t.mesh_j3_h} t={t}>
            <p className="hint" style={{ margin: "0 0 10px" }}>{t.mesh_j3_p}</p>
            <Btn variant="ghost" icon="layers" onClick={() => onTab("policies")} data-testid="mesh-goto-policies">
              {t.mesh_tab_policies}
            </Btn>
          </Step>
          <Step n={3} title={t.mesh_j4_h} t={t}>
            <p className="hint" style={{ margin: 0 }}>{t.mesh_j4_p}</p>
          </Step>
        </>
      )}
    </div>
  );
}

/* The `--mesh` an agent enrols against: the deployment's published Mesh
   address when the operator set one, else a placeholder they must replace. */
function meshArg(settings) {
  return (settings && settings.mesh_url) || "MESH_URL";
}

/* ---- Tab 2: the fleet, and how to get collection onto a host ----------- */
function AgentsTab({ t, lang, dep, overview, settings, onReload, onToast }) {
  const [target, setTarget] = useState(TARGETS[0].key);
  const [editing, setEditing] = useState(null);
  const d = TARGETS.find(x => x.key === target) || TARGETS[0];
  const mesh = meshArg(settings);
  let n = 0;

  const unenrol = async (uid) => {
    try {
      await API.meshUnenrol(dep.id, uid);
      if (onToast) onToast(t.mesh_unenrolled_toast, true);
      if (onReload) onReload();
    } catch (e) { if (onToast) onToast(e.message, false); }
  };

  return (
    <>
      {overview && (
        <div className="ag-panel" style={{ borderTop: "1px solid var(--border)", borderRadius: "var(--radius)", marginBottom: 18 }}>
          <FleetSummary t={t} agents={overview.agents} policies={overview.policies} />
          <h3 className="ag-h" style={{ marginBottom: 12 }}>{t.mesh_fleet_h}</h3>
          <FleetTable t={t} agents={overview.agents} onEdit={setEditing} onUnenrol={unenrol} />
          <AgentDialog open={!!editing} onClose={() => setEditing(null)} t={t} lang={lang} d={dep}
            agent={editing} policies={overview.policies} onDone={onReload} onToast={onToast} />
        </div>
      )}

      <h3 className="ag-h" style={{ margin: "0 0 10px" }}>{t.mesh_add_host_h}</h3>
      <Banner tone="info" title={t.mesh_premise_h} style={{ margin: "0 0 14px" }}>
        <p style={{ margin: "0 0 10px" }}>{t.mesh_premise_p}</p>
        <div className="ag-facts">
          <span><b>{t.mesh_min_label}</b>: <span className="mono">{MIN_FLUENT_BIT}</span></span>
          <span className="hint">{t.mesh_min_why}</span>
        </div>
      </Banner>

      <Tabs items={TARGETS.map(x => ({ key: x.key, label: x.label }))}
        value={target} onChange={setTarget} label={t.mesh_tab_agents} testidPrefix="agents-tab" />

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

        <Step n={++n} title={t.mesh_step_agent} soon t={t}>
          <CmdBlock text={d.agent} t={t} onToast={onToast} />
        </Step>
        <Step n={++n} title={t.mesh_step_enroll} soon t={t}>
          <CmdBlock text={(d.enroll || "sudo velox-agent enroll --mesh MESH_URL --token TOKEN").replace("MESH_URL", mesh)}
            t={t} onToast={onToast} />
          <p className="hint" style={{ marginTop: 8 }}>{t.mesh_placeholder_note}</p>
        </Step>
        <Step n={++n} title={t.mesh_step_verify} soon t={t}>
          <p className="hint" style={{ margin: "0 0 8px" }}>{t.mesh_step_verify_p}</p>
          <CmdBlock text={d.verify || "velox-agent doctor"} t={t} onToast={onToast} />
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

/* ---- Tab 3: the deployment's policies ----------------------------------
   A policy is what a group of hosts runs (ADR-073). It is created here with
   a name; its integrations are added, configured and removed right here
   too — through the SAME dialog the Data tab uses (the one onboarding), with
   the policy as the fixed target — so this tab never grows a second
   composer. It also mints tokens and shows rollouts. ------------------- */
function PoliciesTab({ t, lang, d, overview, settings, onReload, onToast }) {
  const [name, setName] = useState("");
  const [creating, setCreating] = useState(false);
  const [created, setCreated] = useState(null);
  const [createErr, setCreateErr] = useState(null);
  const [tokens, setTokens] = useState({});
  const [catalog, setCatalog] = useState(null);
  const [catalogErr, setCatalogErr] = useState(null);
  const [picking, setPicking] = useState(null);
  const [dialog, setDialog] = useState(null);
  const [busy, setBusy] = useState({});
  const policies = (overview && overview.policies) || [];
  const agentsByPolicy = {};
  ((overview && overview.agents) || []).forEach(a => { if (a.policy_id && a.status !== "unenrolled") agentsByPolicy[a.policy_id] = (agentsByPolicy[a.policy_id] || 0) + 1; });
  const slug = (n) => n.trim().toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "").slice(0, 40) || "policy";
  const mesh = meshArg(settings);

  // The catalog, so "add integration" can offer what the registry has. The
  // dialog then reads the chosen package's inputs itself.
  useEffect(() => {
    let alive = true;
    API.catalog(d.id)
      .then(v => { if (alive) { setCatalog(v); setCatalogErr(null); } })
      .catch(e => { if (alive) setCatalogErr(e.message); });
    return () => { alive = false; };
  }, [d.id]);
  const items = (catalog && catalog.integrations) || [];
  const itemOf = (id) => items.find(x => x.id === id) || { id, title: null };

  const openEdit = async (p, id) => {
    try {
      const doc = await API.meshPolicy(d.id, p.id);
      const act = ((doc.body || {}).activations || []).find(a => a.package_id === id);
      setDialog({ item: itemOf(id), policy: p.id, editing: true, initial: act ? { inputs: act.inputs || {} } : null });
    } catch (e) { if (onToast) onToast(e.message, false); }
  };
  const remove = async (p, id) => {
    setBusy(b => ({ ...b, [`${p.id}/${id}`]: true }));
    try {
      await API.removeIntegration(d.id, id, p.id);
      if (onToast) onToast(t.mesh_pol_integ_removed.replace("{id}", id).replace("{policy}", p.id), true);
      if (onReload) onReload();
    } catch (e) { if (onToast) onToast(e.message, false); }
    finally { setBusy(b => ({ ...b, [`${p.id}/${id}`]: false })); }
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
        onDone={() => { setPicking(null); if (onReload) onReload(); }} />

      <h3 className="ag-h">{t.mesh_pol_list_h}</h3>
      <p className="hint" style={{ margin: "0 0 14px", maxWidth: "70ch" }}>{t.mesh_pol_list_p}</p>

      <div className="card pad" style={{ marginBottom: 18 }} data-testid="mesh-policy-new">
        <h3 className="section-title">{t.mesh_pol_new_h}</h3>
        <div style={{ display: "flex", gap: 10, alignItems: "center", flexWrap: "wrap" }}>
          <input className="input" value={name} onChange={e => setName(e.target.value)}
            placeholder={t.mesh_pol_new_ph} aria-label={t.mesh_policy_name} data-testid="mesh-policy-name" style={{ maxWidth: 280 }} />
          {name.trim() && <span className="hint mono">{slug(name)}</span>}
          <Btn variant="primary" icon="plus" disabled={creating || !name.trim()} data-testid="mesh-policy-create"
            onClick={async () => {
              setCreating(true); setCreateErr(null);
              try {
                const id = slug(name);
                await API.meshCreatePolicy(d.id, id, name.trim());
                setCreated(id); setName("");
                if (onToast) onToast(t.mesh_pol_created.replace("{id}", id), true);
                if (onReload) onReload();
              } catch (e) {
                setCreateErr(e.message);
              } finally { setCreating(false); }
            }}>{creating ? t.mesh_saving : t.mesh_pol_create}</Btn>
        </div>
        {createErr && <Banner tone="danger" title={t.mesh_save_err} style={{ marginTop: 10 }}>{createErr}</Banner>}
        {created && (
          <p className="hint" style={{ marginTop: 8 }} data-testid="mesh-policy-saved">
            {t.mesh_pol_created.replace("{id}", created)}
          </p>
        )}
      </div>

      {policies.length === 0 ? (
        <p className="hint" data-testid="mesh-policies-empty">{t.mesh_pol_empty}</p>
      ) : (
        <div style={{ display: "grid", gap: 10 }}>
          {policies.map(p => {
            const r = (overview.rollout || {})[p.id] || {};
            const tok = tokens[p.id];
            const on = p.integrations || [];
            const candidates = items.filter(it => !on.includes(it.id));
            return (
              <div className="card pad" key={p.id} data-testid={`policy-${p.id}`}>
                <div style={{ display: "flex", gap: 10, alignItems: "center", flexWrap: "wrap" }}>
                  <b className="mono">{p.id}</b>
                  <span className="hint">{p.name}</span>
                  <span className="badge">{t.mesh_pol_agents.replace("{n}", agentsByPolicy[p.id] || 0)}</span>
                  <span className="badge mono">{t.mesh_revision.replace("{n}", p.current_revision)}</span>
                  {r.complete && <span className="badge green">{t.mesh_rollout_done}</span>}
                  {r.updating > 0 && <span className="badge yellow">{t.mesh_rollout_wait.replace("{n}", r.updating)}</span>}
                  {r.failed > 0 && <span className="badge red">{t.mesh_rollout_failed.replace("{n}", r.failed)}</span>}
                </div>

                <div style={{ marginTop: 12 }}>
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
                            onClick={() => openEdit(p, id)}>{t.mesh_pol_integ_edit}</Btn>
                          <Btn variant="ghost" size="sm" icon="trash" disabled={working} data-testid={`policy-integ-remove-${p.id}-${id}`}
                            onClick={() => remove(p, id)}>{t.mesh_pol_integ_remove}</Btn>
                        </div>
                      );
                    })}
                  </div>
                </div>

                <div style={{ display: "flex", gap: 10, marginTop: 12, flexWrap: "wrap", alignItems: "center" }}>
                  <Btn variant="outline" size="sm" icon="plus" data-testid={`policy-add-${p.id}`}
                    onClick={() => setPicking(picking === p.id ? null : p.id)}>{t.mesh_pol_add_integ}</Btn>
                  {picking === p.id && (
                    catalogErr ? <span className="hint">{t.mesh_pol_catalog_err.replace("{err}", catalogErr)}</span>
                      : !catalog ? <span className="hint">{t.integ_loading}</span>
                        : candidates.length === 0 ? <span className="hint">{t.mesh_pol_all_added}</span>
                          : (
                            <select className="select" aria-label={t.mesh_pol_integ_pick} data-testid={`policy-add-pick-${p.id}`} defaultValue=""
                              onChange={e => { const it = items.find(x => x.id === e.target.value); if (it) setDialog({ item: it, policy: p.id, editing: false, initial: null }); }}>
                              <option value="" disabled>{t.mesh_pol_integ_pick}</option>
                              {candidates.map(it => <option key={it.id} value={it.id}>{locText(it.title, lang, it.id)}</option>)}
                            </select>
                          )
                  )}
                  <Btn variant="ghost" size="sm" icon="check" data-testid={`policy-token-${p.id}`}
                    onClick={async () => {
                      try {
                        const res = await API.meshEnrollmentToken(d.id, p.id);
                        setTokens(x => ({ ...x, [p.id]: res.token }));
                      } catch (e) { if (onToast) onToast(e.message, false); }
                    }}>{t.mesh_token_get}</Btn>
                </div>
                {tok && (
                  <div style={{ marginTop: 10 }}>
                    <CmdBlock t={t} onToast={onToast} text={`sudo velox-agent enroll --mesh ${mesh} --token ${tok}`} />
                    <p className="hint" style={{ marginTop: 6 }}>{t.mesh_token_once}</p>
                  </div>
                )}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}

const TABS = ["overview", "agents", "policies", "settings"];

/* The `mesh` tab of one deployment: `/deployments/:id/mesh/:sub`. */
function MeshTab({ d, lang, sub, onSub, onToast }) {
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

  return (
    <div className="view-enter" data-testid="mesh-tab">
      <p className="lead" style={{ marginBottom: 14 }}>{t.mesh_dep_lead}</p>
      <Tabs
        items={[
          { key: "overview", label: t.mesh_tab_overview, icon: "grid" },
          { key: "agents", label: t.mesh_tab_agents, icon: "server" },
          { key: "policies", label: t.mesh_tab_policies, icon: "layers" },
          { key: "settings", label: t.mesh_tab_settings, icon: "sliders" },
        ]}
        value={active} onChange={onSub} label={t.tab_mesh} testidPrefix="mesh-tab" />

      {active === "overview" && (
        <OverviewTab t={t} d={d} status={status} overview={overview} onTab={onSub} />
      )}
      {active === "agents" && (
        <AgentsTab t={t} lang={lang} dep={d} overview={overview} settings={settings} onReload={reload} onToast={onToast} />
      )}
      {active === "policies" && (
        <PoliciesTab t={t} lang={lang} d={d} overview={overview} settings={settings}
          onReload={reload} onToast={onToast} />
      )}
      {active === "settings" && (
        <SettingsTab t={t} d={d} status={status} settings={settings} onSettings={setSettings} onToast={onToast} />
      )}
    </div>
  );
}

export { MeshTab, MeshInstallGuide, MIN_FLUENT_BIT, TABS as MESH_TABS };
