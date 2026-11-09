// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
/* ============================================================
   views_mesh.jsx — Velox Mesh: the fleet journey.

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

   What is NOT here: saved policies, enrolled agents, live fleet state. Those
   need the Mesh's API (phase 1). This screen composes a policy and shows
   exactly what it would carry; nothing is persisted and nothing is mocked.
   ============================================================ */
import { useCallback, useEffect, useState } from "react";
import { STR } from "./i18n.jsx";
import { API } from "./api.jsx";
import { Banner, Btn, Icon, PageHeader, Tabs, copyText } from "./ui.jsx";

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

function FleetTable({ t, agents }) {
  if (!agents || agents.length === 0) return <p className="hint">{t.mesh_fleet_empty}</p>;
  return (
    <table className="fleet" data-testid="mesh-fleet">
      <thead>
        <tr>
          <th>{t.mesh_fleet_host}</th><th>{t.mesh_fleet_status}</th>
          <th>{t.mesh_fleet_policy}</th><th>{t.mesh_fleet_rev}</th>
          <th>{t.mesh_fleet_seen}</th>
        </tr>
      </thead>
      <tbody>
        {agents.map(a => (
          <tr key={a.instance_uid} data-testid={`fleet-row-${a.instance_uid.slice(0, 8)}`}
            data-status={a.status} data-up={a.up ? "1" : "0"}>
            <td className="mono">{a.hostname || a.instance_uid.slice(0, 12)}</td>
            <td>
              <AgentPill status={a.status} t={t} />
              <span className="hint" style={{ marginLeft: 8 }}>{a.up ? t.mesh_up : t.mesh_down}</span>
              {a.config_error && (
                <div className="mono fleet-err" title={a.config_error}>{a.config_error}</div>
              )}
            </td>
            <td className="mono">{a.policy_id || "—"}</td>
            <td className="mono">
              {a.applied_revision != null ? a.applied_revision : "—"}
              {a.current_revision != null && a.applied_revision !== a.current_revision
                && <span className="hint"> → {a.current_revision}</span>}
            </td>
            <td className="hint">{seenAgo(a.last_seen, t)}</td>
          </tr>
        ))}
      </tbody>
    </table>
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
function OverviewTab({ t, status, overview, onToast, onTab }) {
  const notConfigured = !status || !status.configured;
  const unreachable = status && status.configured && status.reachable === false;

  return (
    <div className="ag-panel" data-testid="mesh-panel-overview">
      {notConfigured && (
        <Banner tone="warn" title={t.mesh_absent_h} testid="mesh-absent" style={{ marginBottom: 18 }}>
          {t.mesh_absent_p}
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

      {overview && (
        <div style={{ marginBottom: 22 }}>
          <Onboarding t={t} step={overview.onboarding} onTab={onTab} />
        </div>
      )}

      <h3 className="ag-h">{t.mesh_journey_h}</h3>
      <p className="hint" style={{ margin: "0 0 18px", maxWidth: "70ch" }}>{t.mesh_journey_p}</p>

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

      <Step n={2} title={t.mesh_j2_h} t={t}>
        <p className="hint" style={{ margin: "0 0 10px" }}>{t.mesh_j2_p}</p>
        <Btn variant="ghost" icon="server" onClick={() => onTab("agents")} data-testid="mesh-goto-agents">
          {t.mesh_tab_agents}
        </Btn>
      </Step>

      <Step n={3} title={t.mesh_j3_h} t={t}>
        <p className="hint" style={{ margin: "0 0 10px" }}>{t.mesh_j3_p}</p>
        <Btn variant="ghost" icon="layers" onClick={() => onTab("policies")} data-testid="mesh-goto-policies">
          {t.mesh_tab_policies}
        </Btn>
      </Step>

      <Step n={4} title={t.mesh_j4_h} t={t}>
        <p className="hint" style={{ margin: 0 }}>{t.mesh_j4_p}</p>
      </Step>
    </div>
  );
}

/* ---- Tab 2: get collection onto a host ------------------------------- */
function AgentsTab({ t, overview, onToast }) {
  const [target, setTarget] = useState(TARGETS[0].key);
  const d = TARGETS.find(x => x.key === target) || TARGETS[0];
  let n = 0;

  return (
    <>
      {overview && (
        <div className="ag-panel" style={{ borderTop: "1px solid var(--border)", borderRadius: "var(--radius)", marginBottom: 18 }}>
          <h3 className="ag-h" style={{ marginBottom: 12 }}>{t.mesh_fleet_h}</h3>
          <FleetTable t={t} agents={overview.agents} />
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
          <CmdBlock text={d.enroll || "sudo velox-agent enroll --mesh MESH_URL --token TOKEN"}
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

/* ---- Tab 3: compose a policy ----------------------------------------
   An activation is two halves (velox-policy): the stanza the agent runs and
   the artifacts the target deployment must carry. Both are shown, because
   an operator picking an integration is deciding about both. ---------- */
function ArtifactChips({ a, t }) {
  if (!a) return <span className="hint">{t.mesh_art_loading}</span>;
  const chips = [
    { on: true, label: t.mesh_art_agent },
    { on: !!a.ingest_pipeline, label: t.mesh_art_pipeline },
    { on: !!a.index_template, label: t.mesh_art_template },
    { on: (a.index_patterns || []).length > 0, label: t.mesh_art_index_pattern },
    { on: (a.dashboards || []).length > 0, label: t.mesh_art_dashboards, n: (a.dashboards || []).length },
    { on: !!a.otel_config, label: t.mesh_art_otel },
  ];
  return (
    <div className="mesh-chips">
      {chips.map(c => (
        <span key={c.label} className={`mesh-chip${c.on ? " on" : ""}`}>
          <Icon name={c.on ? "check" : "x"} size={11} />
          {c.label}{c.on && c.n > 1 ? ` · ${c.n}` : ""}
        </span>
      ))}
    </div>
  );
}

function PoliciesTab({ t, deployments, overview, onReload, onToast }) {
  const [target, setTarget] = useState("");
  const [name, setName] = useState("Web tier");
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(null);
  const [token, setToken] = useState(null);
  const [saveErr, setSaveErr] = useState(null);
  const [catalog, setCatalog] = useState(null);
  const [picked, setPicked] = useState([]);
  const [artifacts, setArtifacts] = useState({});
  const [err, setErr] = useState(null);

  useEffect(() => {
    let alive = true;
    API.catalog(null)
      .then(c => { if (alive) setCatalog(c); })
      .catch(e => { if (alive) setErr(e.message); });
    return () => { alive = false; };
  }, []);

  useEffect(() => {
    if (!target && deployments && deployments.length) setTarget(deployments[0].id || deployments[0].name);
  }, [deployments, target]);

  const toggle = useCallback((id) => {
    setPicked(p => (p.includes(id) ? p.filter(x => x !== id) : [...p, id]));
    setArtifacts(a => {
      if (a[id] !== undefined) return a;
      API.integrationArtifacts(id)
        .then(r => setArtifacts(x => ({ ...x, [id]: r })))
        .catch(() => setArtifacts(x => ({ ...x, [id]: null })));
      return { ...a, [id]: undefined };
    });
  }, []);

  const items = (catalog && catalog.integrations) || [];

  return (
    <div className="ag-panel" data-testid="mesh-panel-policies">
      <Banner tone="info" title={t.mesh_policy_h} style={{ marginBottom: 18 }}>
        {t.mesh_policy_p}
      </Banner>

      <Step n={1} title={t.mesh_pol_target_h} t={t}>
        <p className="hint" style={{ margin: "0 0 8px" }}>{t.mesh_pol_target_p}</p>
        {deployments && deployments.length ? (
          <select className="select" value={target} onChange={e => setTarget(e.target.value)}
            data-testid="mesh-policy-target" style={{ maxWidth: 340 }}>
            {deployments.map(d => {
              const id = d.id || d.name;
              return <option key={id} value={id}>{id}</option>;
            })}
          </select>
        ) : (
          <p className="hint">{t.mesh_pol_no_deployment}</p>
        )}
      </Step>

      <Step n={2} title={t.mesh_pol_integrations_h} t={t}>
        <p className="hint" style={{ margin: "0 0 12px" }}>{t.mesh_pol_integrations_p}</p>
        {err && <Banner tone="danger" title={t.mesh_pol_catalog_err}>{err}</Banner>}
        {!catalog && !err && <p className="hint">{t.mesh_art_loading}</p>}
        <div className="mesh-grid">
          {items.map(it => {
            const on = picked.includes(it.id);
            return (
              <button type="button" key={it.id}
                className={`mesh-card${on ? " on" : ""}`}
                aria-pressed={on}
                data-testid={`mesh-integration-${it.id}`}
                onClick={() => toggle(it.id)}>
                <div className="mesh-card-head">
                  <span className="mono">{it.id}</span>
                  <Icon name={on ? "check" : "plus"} size={14} />
                </div>
                {on && <ArtifactChips a={artifacts[it.id]} t={t} />}
              </button>
            );
          })}
        </div>
      </Step>

      <Step n={3} title={t.mesh_pol_result_h} t={t}>
        {picked.length === 0 ? (
          <p className="hint">{t.mesh_pol_result_empty}</p>
        ) : (
          <div data-testid="mesh-policy-summary">
            <p className="hint" style={{ margin: "0 0 10px" }}>
              {t.mesh_pol_result_p.replace("{n}", picked.length).replace("{target}", target || "—")}
            </p>
            <div className="kvrow"><span className="k">{t.mesh_pol_edge}</span>
              <span className="v sans">{picked.join(", ")}</span></div>
            <div className="kvrow"><span className="k">{t.mesh_pol_backend}</span>
              <span className="v sans">
                {picked.map(id => {
                  const a = artifacts[id];
                  if (!a) return null;
                  const parts = [];
                  if (a.ingest_pipeline) parts.push(a.ingest_pipeline);
                  if (a.index_template) parts.push(a.index_template);
                  (a.index_patterns || []).forEach(p => parts.push(p));
                  (a.dashboards || []).forEach(p => parts.push(p));
                  return parts.length ? `${id}: ${parts.length}` : null;
                }).filter(Boolean).join(" · ") || "—"}
              </span></div>
          </div>
        )}
      </Step>

      {/* Saving needs a controller. With one, this is the step that makes
          the policy real: the core compiles it against verified packages and
          the Mesh stores it as a new revision. */}
      {overview ? (
        <>
          <Step n={4} title={t.mesh_save} t={t}>
            <div style={{ display: "flex", gap: 10, alignItems: "center", flexWrap: "wrap" }}>
              <input className="input" value={name} onChange={e => setName(e.target.value)}
                aria-label={t.mesh_policy_name} data-testid="mesh-policy-name" style={{ maxWidth: 260 }} />
              <Btn variant="primary" disabled={saving || picked.length === 0 || !target}
                data-testid="mesh-policy-save"
                onClick={async () => {
                  setSaving(true); setSaveErr(null);
                  try {
                    const id = name.trim().toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "") || "policy";
                    const r = await API.meshSavePolicy({
                      id, name: name.trim() || id, deployment: target, integrations: picked,
                    });
                    setSaved(r.revision);
                    if (onToast) onToast(t.mesh_saved.replace("{rev}", r.revision), true);
                    if (onReload) onReload();
                  } catch (e) {
                    setSaveErr(e.message);
                    if (onToast) onToast(t.mesh_save_err, false);
                  } finally { setSaving(false); }
                }}>
                {saving ? t.mesh_saving : t.mesh_save}
              </Btn>
              {saved != null && (
                <span className="badge green" data-testid="mesh-policy-saved">
                  {t.mesh_revision.replace("{n}", saved)}
                </span>
              )}
            </div>
            {saveErr && <Banner tone="danger" title={t.mesh_save_err} style={{ marginTop: 10 }}>{saveErr}</Banner>}
          </Step>

          <Step n={5} title={t.mesh_token_h} t={t}>
            {saved == null ? (
              <p className="hint">{t.mesh_need_policy}</p>
            ) : (
              <>
                <Btn variant="ghost" icon="plus" data-testid="mesh-token-get"
                  onClick={async () => {
                    const id = name.trim().toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "") || "policy";
                    try {
                      const r = await API.meshEnrollmentToken(id);
                      setToken(r.token);
                    } catch (e) { if (onToast) onToast(e.message, false); }
                  }}>{t.mesh_token_get}</Btn>
                {token && (
                  <div style={{ marginTop: 10 }}>
                    <CmdBlock t={t} onToast={onToast}
                      text={`sudo velox-agent enroll --mesh MESH_URL --token ${token}`} />
                    <p className="hint" style={{ marginTop: 6 }}>{t.mesh_token_once}</p>
                  </div>
                )}
              </>
            )}
          </Step>

          {overview.policies && overview.policies.length > 0 && (
            <Step n={6} title={t.mesh_rollout_h} t={t}>
              <table className="fleet" data-testid="mesh-rollout">
                <tbody>
                  {overview.policies.map(p => {
                    const r = (overview.rollout || {})[p.id] || {};
                    return (
                      <tr key={p.id} data-testid={`rollout-${p.id}`} data-complete={r.complete ? "1" : "0"}>
                        <td className="mono">{p.id}</td>
                        <td>{t.mesh_rollout_p
                          .replace("{current}", r.current || 0)
                          .replace("{total}", r.total || 0)
                          .replace("{rev}", p.current_revision)}</td>
                        <td>
                          {r.complete && <span className="badge green">{t.mesh_rollout_done}</span>}
                          {r.updating > 0 && <span className="badge yellow">{t.mesh_rollout_wait.replace("{n}", r.updating)}</span>}
                          {r.failed > 0 && <span className="badge red">{t.mesh_rollout_failed.replace("{n}", r.failed)}</span>}
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </Step>
          )}
        </>
      ) : (
        <Banner tone="warn" title={t.mesh_pol_save_h} testid="mesh-policy-save-blocked" style={{ marginTop: 18 }}>
          {t.mesh_pol_save_p}
        </Banner>
      )}
    </div>
  );
}

const TABS = ["overview", "agents", "policies"];

function MeshView({ lang, tab, onTab, deployments, onToast }) {
  const t = STR[lang] || STR.pt;
  const [status, setStatus] = useState(null);
  const [overview, setOverview] = useState(null);
  const [tick, setTick] = useState(0);
  const active = TABS.includes(tab) ? tab : "overview";

  useEffect(() => {
    let alive = true;
    API.meshStatus()
      .then(s => { if (alive) setStatus(s); })
      // A tenant has no business with the Mesh URL (AdminOnly): treat a
      // refusal as "not configured" rather than showing an error.
      .catch(() => { if (alive) setStatus({ configured: false }); });
    return () => { alive = false; };
  }, []);

  // The fleet, refreshed on a cadence a little under the agents' own poll so
  // "last seen" and a rollout move without the operator reloading. No Mesh
  // (or no permission) simply leaves `overview` null and the screens fall
  // back to explaining how to get one.
  useEffect(() => {
    let alive = true;
    const load = () => API.meshOverview()
      .then(o => { if (alive) setOverview(o); })
      .catch(() => { if (alive) setOverview(null); });
    load();
    const h = setInterval(load, 10000);
    return () => { alive = false; clearInterval(h); };
  }, [tick]);

  return (
    <>
      <PageHeader title={t.nav_mesh} lead={t.mesh_lead} />
      <Tabs
        items={[
          { key: "overview", label: t.mesh_tab_overview, icon: "grid" },
          { key: "agents", label: t.mesh_tab_agents, icon: "server" },
          { key: "policies", label: t.mesh_tab_policies, icon: "layers" },
        ]}
        value={active} onChange={onTab} label={t.nav_mesh} testidPrefix="mesh-tab" />

      {active === "overview" && <OverviewTab t={t} status={status} overview={overview} onToast={onToast} onTab={onTab} />}
      {active === "agents" && <AgentsTab t={t} overview={overview} onToast={onToast} />}
      {active === "policies" && (
        <PoliciesTab t={t} deployments={deployments} overview={overview}
          onReload={() => setTick(x => x + 1)} onToast={onToast} />
      )}
    </>
  );
}

export { MeshView, MIN_FLUENT_BIT, TABS };
