// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
/* ============================================================
   views_agents.jsx — "Agentes": how to put collection on a host that is
   not a node of this cluster.

   This screen exists because of a product decision, not a UI idea
   (ADR-069): **VeloxSearch does not distribute the collection engines.**
   Upstream Fluent Bit publishes no standalone static Linux binary — the
   install path is the project's own apt/yum repositories, or a distribution
   package where one exists — so bundling it would make us the builder of a C
   codebase and the owner of its CVE response. Instead the operator installs
   it and the Velox Agent supervises what it finds, refusing anything below
   the minimum version.

   The consequence is that the install becomes a documented procedure, and a
   procedure nobody can find is a procedure nobody follows. So it lives here,
   one tab per distribution, with the upstream commands verbatim and
   copy-paste — never paraphrased, because a paraphrased shell command is a
   support ticket.

   Three honesty rules this file keeps:
     - Commands are DATA, not translated strings. A translated `apt-get` is a
       bug. Only titles, notes and prose go through i18n.jsx.
     - Where a distribution does NOT package Fluent Bit (Debian, Ubuntu) the
       tab says so, instead of letting the operator discover that
       `apt install fluent-bit` finds nothing.
     - The agent package is not published yet. The steps that need it are
       marked, rather than printing a command that cannot work.

   The minimum version is ADR-069's; `MIN_FLUENT_BIT` here and
   `velox_agent::preflight::MIN_FLUENT_BIT` in the agent both cite it.
   ============================================================ */
import { useState } from "react";
import { STR } from "./i18n.jsx";
import { Banner, Icon, PageHeader, Tabs, copyText } from "./ui.jsx";

/* ADR-069 decision 3. Keep in step with the agent's constant. */
const MIN_FLUENT_BIT = "3.2";
const FB_DOCS = "https://docs.fluentbit.io/manual/installation/downloads/linux";
const FB_WINDOWS_DOCS = "https://docs.fluentbit.io/manual/installation/downloads/windows";
/* The current repository signing key, shown so an operator can compare it
   with what `gpg` reports after the import. */
const FB_KEY_FPR = "C3C0 A285 34B9 293E AF51 FABD 9F9D DC08 3888 C1CD";

/* ---- The per-distribution procedure -----------------------------------
   `repo` and `install` are verbatim from docs.fluentbit.io. `note` names an
   i18n key when the tab needs a caveat. `distroPackaged` drops the
   repository step entirely (Fedora). ---------------------------------- */
const DEB_KEY =
  "sudo sh -c 'curl https://packages.fluentbit.io/fluentbit.key | gpg --dearmor > /usr/share/keyrings/fluentbit-keyring.gpg'";
const DEB_CODENAME =
  "codename=$(grep -oP '(?<=VERSION_CODENAME=).*' /etc/os-release 2>/dev/null || lsb_release -cs 2>/dev/null)";

function yumRepo(flavour, releasever) {
  return [
    "sudo tee /etc/yum.repos.d/fluent-bit.repo <<'EOF'",
    "[fluent-bit]",
    "name=Fluent Bit",
    `baseurl=https://packages.fluentbit.io/${flavour}/${releasever}/`,
    "gpgcheck=1",
    "gpgkey=https://packages.fluentbit.io/fluentbit.key",
    "repo_gpgcheck=1",
    "enabled=1",
    "EOF",
  ].join("\n");
}

const DISTROS = [
  {
    key: "debian",
    label: "Debian",
    note: "agents_note_no_pkg",
    repo: [DEB_KEY, DEB_CODENAME,
      'echo "deb [signed-by=/usr/share/keyrings/fluentbit-keyring.gpg] https://packages.fluentbit.io/debian/$codename $codename main" | sudo tee /etc/apt/sources.list.d/fluent-bit.list',
      "sudo apt-get update"].join("\n"),
    install: "sudo apt-get install fluent-bit",
    agent: "sudo apt-get install ./velox-agent_0.1.0_amd64.deb",
  },
  {
    key: "ubuntu",
    label: "Ubuntu",
    note: "agents_note_no_pkg",
    repo: [DEB_KEY, DEB_CODENAME,
      'echo "deb [signed-by=/usr/share/keyrings/fluentbit-keyring.gpg] https://packages.fluentbit.io/ubuntu/$codename $codename main" | sudo tee /etc/apt/sources.list.d/fluent-bit.list',
      "sudo apt-get update"].join("\n"),
    install: "sudo apt-get install fluent-bit",
    agent: "sudo apt-get install ./velox-agent_0.1.0_amd64.deb",
  },
  {
    key: "rhel",
    label: "RHEL · CentOS Stream",
    note: "agents_note_rhel9",
    repo: yumRepo("centos", "$releasever"),
    install: "sudo dnf install fluent-bit",
    alt: { label: "EPEL (RHEL 9)", cmd: "sudo dnf install epel-release\nsudo dnf install fluent-bit" },
    agent: "sudo dnf install ./velox-agent-0.1.0.x86_64.rpm",
  },
  {
    key: "rocky",
    label: "Rocky · Alma",
    repo: yumRepo("almalinux", "$releasever"),
    install: "sudo dnf install fluent-bit",
    agent: "sudo dnf install ./velox-agent-0.1.0.x86_64.rpm",
  },
  {
    key: "amazon",
    label: "Amazon Linux",
    repo: yumRepo("amazonlinux", "2023"),
    install: "sudo dnf install fluent-bit",
    agent: "sudo dnf install ./velox-agent-0.1.0.x86_64.rpm",
  },
  {
    key: "fedora",
    label: "Fedora",
    note: "agents_note_distro_pkg",
    distroPackaged: true,
    install: "sudo dnf install fluent-bit",
    agent: "sudo dnf install ./velox-agent-0.1.0.x86_64.rpm",
  },
  {
    key: "raspbian",
    label: "Raspberry Pi OS",
    note: "agents_note_aptkey",
    repo: ["sudo sh -c 'curl https://packages.fluentbit.io/fluentbit.key | sudo apt-key add - '",
      'echo "deb https://packages.fluentbit.io/raspbian/bookworm bookworm main" | sudo tee /etc/apt/sources.list.d/fluent-bit.list',
      "sudo apt-get update"].join("\n"),
    install: "sudo apt-get install fluent-bit",
    agent: "sudo apt-get install ./velox-agent_0.1.0_arm64.deb",
  },
  {
    key: "windows",
    label: "Windows",
    note: "agents_note_windows",
    docs: FB_WINDOWS_DOCS,
    windows: true,
    repo: ["# Get the installer and its checksum from the official downloads page,",
      "# then compare the hash before running it:",
      "Get-FileHash .\\fluent-bit-<version>-win64.msi"].join("\n"),
    install: "msiexec /i fluent-bit-<version>-win64.msi /quiet",
    agent: "msiexec /i velox-agent-0.1.0-win64.msi /quiet",
    enroll: "velox-agent.exe enroll --mesh MESH_URL --token TOKEN",
    verify: "velox-agent.exe doctor",
  },
];

/* A multi-line command with its own copy button. `Copyable` in ui.jsx is for
   single-line values (it ellipsises and never wraps); a repository stanza
   needs the lines preserved, so this pairs `.codeblock` with the same
   `copyText` helper and the same copied/failed feedback. */
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

/* One numbered step. `soon` marks what needs the unpublished agent package. */
function Step({ n, title, soon, t, children }) {
  return (
    <section className="ag-step">
      <div className="ag-step-head">
        <span className="ag-step-n">{n}</span>
        <h3>{title}</h3>
        {soon && <span className="badge yellow">{t.agents_soon}</span>}
      </div>
      <div className="ag-step-body">{children}</div>
    </section>
  );
}

function AgentsView({ lang, onToast }) {
  const t = STR[lang] || STR.pt;
  const [tab, setTab] = useState(DISTROS[0].key);
  const d = DISTROS.find(x => x.key === tab) || DISTROS[0];
  let n = 0;

  return (
    <>
      <PageHeader title={t.nav_agents} lead={t.agents_lead} />

      {/* The premise, stated before the commands: whoever reads this screen is
          about to install something we deliberately do not ship. */}
      <Banner tone="info" title={t.agents_premise_h} style={{ marginBottom: 16 }}>
        <p style={{ margin: "0 0 10px" }}>{t.agents_premise_p}</p>
        <div className="ag-facts">
          <span><b>{t.agents_min_label}</b>: <span className="mono">{MIN_FLUENT_BIT}</span></span>
          <span className="hint">{t.agents_min_why}</span>
        </div>
      </Banner>

      <Banner tone="warn" title={t.agents_notready_h} style={{ marginBottom: 20 }}>
        {t.agents_notready_p}
      </Banner>

      <Tabs items={DISTROS.map(x => ({ key: x.key, label: x.label }))}
        value={tab} onChange={setTab} label={t.nav_agents} testidPrefix="agents-tab" />

      <div className="ag-panel" data-testid={`agents-panel-${d.key}`}>
        {d.note && <p className="ag-note"><Icon name="info" size={14} />{t[d.note]}</p>}

        {!d.distroPackaged && (
          <Step n={++n} title={t.agents_step_repo} t={t}>
            <CmdBlock text={d.repo} t={t} onToast={onToast} />
            {!d.windows && (
              <p className="hint" style={{ marginTop: 8 }}>
                {t.agents_key_label}: <span className="mono">{FB_KEY_FPR}</span>
              </p>
            )}
          </Step>
        )}

        <Step n={++n} title={t.agents_step_install} t={t}>
          <CmdBlock text={d.install} t={t} onToast={onToast} />
          {d.alt && (
            <div style={{ marginTop: 10 }}>
              <p className="hint" style={{ marginBottom: 6 }}>{d.alt.label}</p>
              <CmdBlock text={d.alt.cmd} t={t} onToast={onToast} />
            </div>
          )}
        </Step>

        <Step n={++n} title={t.agents_step_check} t={t}>
          <p className="hint" style={{ margin: "0 0 8px" }}>
            {t.agents_step_check_p.replace("{min}", MIN_FLUENT_BIT)}
          </p>
          <CmdBlock text={d.windows ? "fluent-bit.exe --version" : "fluent-bit --version"}
            t={t} onToast={onToast} />
        </Step>

        <Step n={++n} title={t.agents_step_agent} soon t={t}>
          <CmdBlock text={d.agent} t={t} onToast={onToast} />
        </Step>

        <Step n={++n} title={t.agents_step_enroll} soon t={t}>
          <CmdBlock text={d.enroll || "sudo velox-agent enroll --mesh MESH_URL --token TOKEN"}
            t={t} onToast={onToast} />
          <p className="hint" style={{ marginTop: 8 }}>{t.agents_placeholder_note}</p>
        </Step>

        <Step n={++n} title={t.agents_step_verify} soon t={t}>
          <p className="hint" style={{ margin: "0 0 8px" }}>{t.agents_step_verify_p}</p>
          <CmdBlock text={d.verify || "velox-agent doctor"} t={t} onToast={onToast} />
        </Step>

        <div className="ag-foot">
          <p className="ag-note"><Icon name="info" size={14} />
            <span><b>{t.agents_airgap_h}.</b> {t.agents_airgap_p}</span>
          </p>
          {/* An <a>, not <Btn>: Btn renders a <button>, which cannot carry an
              href — the same shape the sidebar's help link uses. */}
          <a className="btn btn-ghost" href={d.docs || FB_DOCS} target="_blank" rel="noreferrer">
            <Icon name="book" size={15} />{t.agents_docs}<Icon name="ext" size={13} />
          </a>
        </div>
      </div>
    </>
  );
}

export { AgentsView, MIN_FLUENT_BIT };
