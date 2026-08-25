// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
/* ============================================================
   Create view — guided 5-step wizard (the Backup step is optional)
   ============================================================ */
import React, { useState, useEffect } from "react";
import { STR, SIZES, sizeMeta } from "./i18n.jsx";
import { Icon, Field, Btn, Copyable } from "./ui.jsx";
import { SnapshotStep } from "./views_snapshot.jsx";
import { API } from "./api.jsx";

const PURPOSES = ["observability", "security", "search"];

// Sentinel for "type a version myself" in the version select. Not a version, so
// it can never be sent as one (ADR-048 rev. 2).
const OTHER_VERSION = "__other__";

// The name becomes a Kubernetes object name and a DNS label (the deployment's
// subdomain, ADR-020), so the backend enforces RFC 1123. Checking it here means
// the user learns the rule while typing instead of at submit time. The backend
// remains the authority — this only mirrors its rule.
const NAME_RE = /^[a-z0-9]([-a-z0-9]*[a-z0-9])?$/;
const NAME_MAX = 40;

function nameError(raw, t) {
  const n = raw.trim();
  if (!n) return "";                       // untouched: hint, not an error
  if (n.length > NAME_MAX) return t.name_err_long;
  if (!NAME_RE.test(n)) return t.name_err_chars;
  return "";
}

function PurposeCard({ p, selected, lang, onSelect }) {
  const t = STR[lang];
  const meta = {
    observability: { icon: "activity", title: t.p_obs_t, desc: t.p_obs_d, keep: t.p_obs_keep, col: t.p_obs_col, set: t.p_obs_set, best: t.p_obs_best },
    security: { icon: "shield", title: t.p_sec_t, desc: t.p_sec_d, keep: t.p_sec_keep, col: t.p_sec_col, set: t.p_sec_set, best: t.p_sec_best },
    search: { icon: "search", title: t.p_search_t, desc: t.p_search_d, keep: t.p_search_keep, col: t.p_search_col, set: t.p_search_set, best: t.p_search_best },
  }[p];
  const rows = [
    [t.keeps_data, meta.keep], [t.collects, meta.col], [t.sets_up, meta.set], [t.best_for, meta.best],
  ];
  return (
    <button className="purpose" aria-pressed={selected} onClick={() => onSelect(p)}>
      <span className="check"><Icon name="check" size={12} /></span>
      <div className="ph"><span className="picon"><Icon name={meta.icon} size={17} /></span>{meta.title}</div>
      <div className="pd">{meta.desc}</div>
      {rows.map(([k, v], i) => (
        <div className="pk" key={i}>
          <div className="k">{k}</div>
          <div className="v">{v}</div>
        </div>
      ))}
    </button>
  );
}

function CreateView({ lang, onCreate, onCancel }) {
  const t = STR[lang];
  const [step, setStep] = useState(0);
  const [name, setName] = useState("");
  const [purpose, setPurpose] = useState("observability");
  // OpenSearch version to deploy (ADR-048 rev. 2). The list is the backend's —
  // the newest few it confirmed upstream, or the pinned catalog when the check
  // never answered. `OTHER` reveals a free-text box; the backend still refuses
  // a malformed tag or one whose images are not published.
  const [versions, setVersions] = useState([]);
  const [version, setVersion] = useState("");
  const [customVersion, setCustomVersion] = useState("");
  const [size, setSize] = useState("small");
  // No longer a form field: the data-sources step moved to the Integrations
  // tab. Kept as the create payload's value so a new deployment still gets the
  // always-on K3S monitoring (ADR-018) — and Review says so, rather than the
  // wizard deriving it from a scan the user cannot see.
  const [sources] = useState({ kubernetes: true, "k8s-events": false });
  // Workloads discover() found in the cluster, filtered to ones with a recipe
  // we can ship (Detected.recipe). Drives the data-step pre-fill (#2 / ADR-018).
  const [advanced, setAdvanced] = useState(false);
  const [extra, setExtra] = useState("");
  // Sizing tiers come from the backend (ADR-016, single source = k8s::sizing).
  // Seed with the bundled SIZES so the wizard renders before the fetch resolves
  // and still works offline; replace with the server's presets on mount.
  const [sizes, setSizes] = useState(SIZES);
  // Advanced "custom size" inputs; heap is resolved server-side (half memory).
  const [customMem, setCustomMem] = useState("");
  const [customDisk, setCustomDisk] = useState("");
  const [customHeap, setCustomHeap] = useState("");
  // Storage classification (ADR-031): a node-local/absent default means creating
  // a cluster auto-installs Longhorn. We don't ASK — we inform: a heads-up on the
  // review step before, and a live progress notice while it installs.
  const [storage, setStorage] = useState(null);
  const [creating, setCreating] = useState(false);
  // Snapshot repository (ADR-049). `null` = the step was skipped, which is the
  // default and costs nothing: the Backup tab configures it later just as well.
  const [snapshot, setSnapshot] = useState(null);

  // The data-sources step is gone: enabling an integration is a day-2 decision
  // and the Integrations tab is where it lives, with live doc counts the wizard
  // could never show. `sources` keeps its default so a new deployment still
  // gets the always-on K3S monitoring (ADR-018) — Review states it.
  const steps = [t.step_purpose, t.step_size, t.step_backup, t.step_review];
  const lastStep = steps.length - 1;
  const sz = sizes[size] || sizeMeta(size);

  // Fetch the preset tiers once. On failure keep the bundled defaults — never
  // surface a console error (the #32 gate fails the build on those).
  useEffect(() => {
    let live = true;
    API.sizingPresets()
      .then((list) => {
        if (!live || !Array.isArray(list) || !list.length) return;
        const m = {};
        list.forEach((p) => { m[p.name] = p; });
        setSizes(m);
      })
      .catch(() => { /* offline / pre-auth: bundled SIZES stand in */ });
    return () => { live = false; };
  }, []);

  // Version choices, once. Offline/pre-auth leaves the select empty and the
  // backend's own default applies — never a console error (the #32 gate).
  useEffect(() => {
    let live = true;
    API.availableVersions()
      .then((v) => {
        if (!live || !v || !Array.isArray(v.versions) || !v.versions.length) return;
        setVersions(v.versions);
        setVersion((prev) => prev || v.default || v.versions[0]);
      })
      .catch(() => { /* offline: backend default applies */ });
    return () => { live = false; };
  }, []);

  // Advanced path posts the custom inputs to resolve the derived size — the
  // backend applies the heap = half-memory rule so the UI never computes it.
  useEffect(() => {
    if (!advanced) return undefined;
    let live = true;
    API.customSizing(customMem, customDisk)
      .then((p) => { if (live && p && p.heap) setCustomHeap(p.heap); })
      .catch(() => { /* keep the last resolved heap; no console noise */ });
    return () => { live = false; };
  }, [advanced, customMem, customDisk]);

  // Classify the cluster's default storage once on mount (read-only — never
  // triggers an install). Drives the "will auto-install Longhorn" heads-up.
  // On failure (offline / pre-auth) leave it null: the create flow proceeds and
  // the backend gate still installs + the toast still reports it.
  useEffect(() => {
    let live = true;
    API.storageStatus()
      .then((s) => { if (live && s) setStorage(s); })
      .catch(() => { /* no cluster / pre-auth: no heads-up, no console noise */ });
    return () => { live = false; };
  }, []);

  // While the create-triggered Longhorn install runs, poll the same read-only
  // status so the progress notice updates (step → "installed") through the
  // existing install-job snapshot, not a new channel. Stops on unmount (the
  // parent navigates to the new deployment once create resolves).
  useEffect(() => {
    if (!creating) return undefined;
    let live = true;
    const tick = () =>
      API.storageStatus().then((s) => { if (live && s) setStorage(s); }).catch(() => {});
    tick();
    const h = setInterval(tick, 3000);
    return () => { live = false; clearInterval(h); };
  }, [creating]);

  // "" means "let the backend decide" — an empty manual box must not be sent
  // as a version, it must fall back to the default.
  const chosenVersion = version === OTHER_VERSION ? customVersion.trim() : version;
  const nameErr = nameError(name, t);
  const valid = name.trim().length > 0 && !nameErr
    && (version !== OTHER_VERSION || !!chosenVersion);

  // Longhorn not in place yet → creating this cluster auto-installs it
  // (ADR-031/043). We inform, never ask: show a live install notice while
  // the (blocking) create runs.
  const willInstallStorage = !!(storage && storage.needs_longhorn && !storage.durable);
  // Nodes missing Longhorn prerequisites (`#15`, ADR-043): the backend maps
  // each `MissingDependency` to a package + per-distro install commands.
  // Creation is blocked until the list is empty.
  const missingPkgs = (storage && storage.missing_packages) || [];

  function finish() {
    if (willInstallStorage) setCreating(true);
    onCreate({
      name: name.trim() || "logs", purpose, size, sources, version: chosenVersion,
      extra: advanced ? extra : "",
      // Custom-size overrides (ADR-016) only when Advanced is open; node count
      // is never sent — it stays the fixed 3 the backend enforces.
      memory: advanced ? customMem.trim() : "",
      disk: advanced ? customDisk.trim() : "",
      // Optional (ADR-049). The backend registers the repository only after the
      // cluster goes green — the operator's reconciler needs it running.
      snapshot,
    });
  }

  return (
    <div className="view-enter">
      <h1 className="page-title">{t.nav_create}</h1>
      <p className="lead">{t.create_lead}</p>

      {/* stepper */}
      <div className="stepper">
        {steps.map((s, i) => (
          <React.Fragment key={i}>
            <div className={`step ${i === step ? "active" : ""} ${i < step ? "done" : ""}`}>
              <span className="node">{i < step ? <Icon name="check" size={13} /> : i + 1}</span>
              <span className="txt">{s}</span>
            </div>
            {i < steps.length - 1 && <span className={`line ${i < step ? "done" : ""}`} />}
          </React.Fragment>
        ))}
      </div>

      <div className="card pad">
        {/* STEP 1 — name + purpose */}
        {step === 0 && (
          <div className="view-enter">
            <Field label={t.name} hint={t.name_hint} htmlFor="create-name" error={nameErr}>
              <input id="create-name" className={`input${nameErr ? " invalid" : ""}`} name="name"
                placeholder={t.name_ph} value={name}
                onChange={e => setName(e.target.value)} autoFocus />
            </Field>
            {/* OpenSearch version (ADR-048 rev. 2) — right after the name, so
                the deployment's identity and its version are decided together.
                A version is only chosen ONCE here: afterwards it moves solely
                through the upgrade operation, which cannot be undone. */}
            <Field label={t.version_label} hint={t.version_hint} htmlFor="create-version">
              <select id="create-version" className="input" value={version}
                onChange={e => setVersion(e.target.value)}>
                {versions.map((v, i) => (
                  <option key={v} value={v}>{v}{i === 0 ? ` · ${t.upg_note_current}` : ""}</option>
                ))}
                <option value={OTHER_VERSION}>{t.version_other}</option>
              </select>
            </Field>
            {version === OTHER_VERSION && (
              <Field label={t.version_manual} hint={t.version_manual_hint} htmlFor="create-version-custom">
                <input id="create-version-custom" className="input" placeholder="3.8.0"
                  value={customVersion} onChange={e => setCustomVersion(e.target.value)} />
              </Field>
            )}

            <div className="section-title" style={{ marginTop: 22 }}>{t.purpose_q}</div>
            <div className="purpose-grid">
              {PURPOSES.map(p => (
                <PurposeCard key={p} p={p} selected={purpose === p} lang={lang} onSelect={setPurpose} />
              ))}
            </div>
          </div>
        )}

        {/* STEP 2 — size */}
        {step === 1 && (
          <div className="view-enter">
            <Field label={t.size} hint={t.size_hint}>
              <div style={{ display: "grid", gap: 10 }}>
                {Object.entries(sizes).map(([k, v]) => (
                  <button key={k} className="purpose" aria-pressed={size === k}
                    onClick={() => setSize(k)} style={{ display: "flex", alignItems: "center", gap: 14, padding: 15 }}>
                    <span className="check"><Icon name="check" size={12} /></span>
                    <span style={{ color: size === k ? "var(--accent)" : "var(--text-3)" }}><Icon name="server" size={18} /></span>
                    <span style={{ flex: 1, textAlign: "left" }}>
                      <div style={{ fontFamily: "var(--font-mono)", fontWeight: 600, marginBottom: 2 }}>{v.label}</div>
                      <div style={{ fontSize: 12.5, color: "var(--text-3)", fontFamily: "var(--font-mono)" }}>
                        {v.nodes} {t.nodes} · {v.heap} heap · {v.disk} disk
                      </div>
                    </span>
                  </button>
                ))}
              </div>
            </Field>

            <button className="btn-link" style={{ marginTop: 6 }} onClick={() => setAdvanced(a => {
              const next = !a;
              // Seed the custom inputs from the selected preset the first time
              // Advanced opens, so the resolved heap reflects a real starting point.
              if (next && !customMem) { setCustomMem(sz.mem); setCustomDisk(sz.disk); }
              return next;
            })}>
              <Icon name="chevR" size={12} style={{ transform: advanced ? "rotate(90deg)" : "none", transition: "transform .15s", verticalAlign: "middle", marginRight: 4 }} />
              {t.advanced}
            </button>
            {advanced && (
              <div className="view-enter" style={{ marginTop: 14, display: "grid", gap: 14, gridTemplateColumns: "1fr 1fr" }}>
                <Field label={t.node_mem} tip={t.node_mem_hint}>
                  <input className="input" value={customMem} onChange={e => setCustomMem(e.target.value)} />
                </Field>
                <Field label={t.disk}>
                  <input className="input" value={customDisk} onChange={e => setCustomDisk(e.target.value)} />
                </Field>
                <Field label={t.jvm} tip={t.jvm_hint}>
                  {/* Heap is server-resolved (half the memory) — always read-only. */}
                  <input className="input" value={(() => { const h = customHeap || sz.heap; return `-Xms${h} -Xmx${h}`; })()} disabled />
                </Field>
                <Field label={t.node_count}>
                  {/* Always 3 nodes (ADR-016) — fixed, never an override. */}
                  <input className="input" value={sz.nodes} disabled />
                </Field>
                <div style={{ gridColumn: "1 / -1" }}>
                  <Field label={t.extra} hint={t.extra_hint}>
                    <textarea className="textarea" value={extra} onChange={e => setExtra(e.target.value)}
                      placeholder={"indices.query.bool.max_clause_count: 2048"} />
                  </Field>
                </div>
              </div>
            )}
          </div>
        )}

        {/* STEP 3 — snapshot (optional, ADR-047) */}
        {step === 2 && (
          <SnapshotStep cfg={snapshot} onChange={setSnapshot} lang={lang} />
        )}

        {/* STEP 5 — review */}
        {step === 3 && (
          <div className="view-enter">
            <div className="section-title">{t.review_h}</div>
            <p className="hint" style={{ marginTop: -6, marginBottom: 18 }}>{t.review_p}</p>
            <div style={{ background: "var(--surface-2)", border: "1px solid var(--border)", borderRadius: "var(--radius)", padding: "4px 16px" }}>
              {/* The suffix is generated server-side at submit (ADR-020); the
                  old "-xxxx" read like a real value. */}
              <div className="kvrow"><span className="k">{t.review_name}</span>
                <span className="v">{name.trim() || "—"}<span style={{ color: "var(--text-3)" }}> · {t.review_suffix}</span></span>
              </div>
              <div className="kvrow"><span className="k">{t.review_purpose}</span><span className="v">{STR[lang]["p_" + (purpose === "observability" ? "obs" : purpose === "security" ? "sec" : "search") + "_t"]}</span></div>
              <div className="kvrow"><span className="k">{t.version_label}</span><span className="v">{chosenVersion || "—"}</span></div>
              <div className="kvrow"><span className="k">{t.review_size}</span><span className="v">{advanced ? (lang === "pt" ? "Personalizado" : lang === "es" ? "Personalizado" : "Custom") : sz.label} · {sz.nodes} {t.nodes} · {(advanced && customHeap) || sz.heap} · {(advanced && customDisk) || sz.disk}</span></div>
              <div className="kvrow">
                <span className="k">{t.review_sources}</span>
                <span className="v">
                  {purpose === "search" ? "API" :
                    Object.keys(sources).filter(k => sources[k]).length
                      ? Object.keys(sources).filter(k => sources[k]).join(", ")
                      : t.review_none}
                </span>
              </div>
              <div className="kvrow">
                <span className="k">{t.review_backup}</span>
                <span className="v" data-testid="review-backup">
                  {snapshot
                    ? `${snapshot.bucket || "—"}${snapshot.policy && snapshot.policy.enabled ? ` · ${snapshot.policy.cron}` : ""}`
                    : t.review_none}
                </span>
              </div>
            </div>
            {/* ADR-053: a pointer, not a decision. The stack is a day-2 install
                — it needs a green cluster and PVCs, and its resource cost can
                only be stated honestly once this deployment is allocated. */}
            {purpose === "observability" && (
              <p className="hint" style={{ marginTop: 12 }}>{t.review_otel_hint}</p>
            )}
            {/* Storage heads-up (ADR-031): no durable default SC → creating
                auto-installs Longhorn. Inform before, not ask. */}
            {willInstallStorage && !creating && (
              <div style={{
                display: "flex", gap: 12, padding: 16, marginTop: 14, borderRadius: "var(--radius)",
                background: "var(--info-soft)", border: "1px solid var(--border)", color: "var(--text-2)",
              }}>
                <span style={{ color: "var(--info)", flexShrink: 0 }}><Icon name="server" size={18} /></span>
                <div>
                  <div style={{ fontWeight: 600, fontSize: 14, marginBottom: 4 }}>{t.storage_will_install_h}</div>
                  <div style={{ fontSize: 13 }}>{t.storage_will_install_p}</div>
                </div>
              </div>
            )}
            {/* Live install notice while the create-triggered Longhorn install
                runs (progress from the shared install-job snapshot). */}
            {creating && (
              <div style={{
                display: "flex", gap: 12, padding: 16, marginTop: 14, borderRadius: "var(--radius)",
                background: "var(--info-soft)", border: "1px solid var(--border)", color: "var(--text-2)",
              }}>
                <span style={{ color: "var(--info)", flexShrink: 0 }}><Icon name="server" size={18} /></span>
                <div style={{ flex: 1 }}>
                  <div style={{ fontWeight: 600, fontSize: 14, marginBottom: 4 }}>
                    {storage?.durable ? t.created_h : t.storage_installing_h}
                  </div>
                  <div style={{ fontSize: 13 }}>
                    {storage?.durable ? t.storage_installed_p : t.storage_installing_p}
                  </div>
                  {storage?.installing && !storage?.durable && (
                    <div className="hint" style={{ marginTop: 6, fontFamily: "var(--font-mono)", fontSize: 12.5 }}>
                      {t.storage_step}: {storage.installing}
                    </div>
                  )}
                  {storage?.error && (
                    <div style={{ color: "var(--danger)", fontSize: 13, marginTop: 6, fontFamily: "var(--font-mono)" }}>
                      {storage.error}
                    </div>
                  )}
                </div>
              </div>
            )}
            {/* Missing node packages (#15, ADR-043): Longhorn reported nodes
                that can't run it. Per node: package + per-distro one-liners
                (copyable) + the raw reason. Creation stays blocked meanwhile. */}
            {missingPkgs.length > 0 && (
              <div style={{
                display: "flex", gap: 12, padding: 16, marginTop: 14, borderRadius: "var(--radius)",
                background: "var(--danger-soft)", border: "1px solid var(--danger-border)", color: "var(--text-2)",
              }}>
                <span style={{ color: "var(--danger)", flexShrink: 0 }}><Icon name="shield" size={18} /></span>
                <div style={{ flex: 1, minWidth: 0 }}>
                  <div style={{ fontWeight: 600, fontSize: 14, marginBottom: 4, color: "var(--text)" }}>{t.storage_missing_h}</div>
                  <div style={{ fontSize: 13, marginBottom: 10 }}>{t.storage_missing_p}</div>
                  {missingPkgs.map((m, i) => (
                    <div key={i} style={{
                      padding: "10px 12px", marginTop: i ? 10 : 0, borderRadius: "var(--radius)",
                      background: "var(--surface-2)", border: "1px solid var(--border)",
                    }}>
                      <div style={{ fontFamily: "var(--font-mono)", fontSize: 13, marginBottom: 6 }}>
                        <strong>{m.node}</strong>
                        {m.package
                          ? <> · {t.storage_missing_pkg}: <span style={{ color: "var(--danger)" }}>{m.package}</span></>
                          : <span style={{ color: "var(--text-3)" }}> · {t.storage_missing_unknown}</span>}
                      </div>
                      {m.install && (
                        <div style={{ display: "grid", gap: 6 }}>
                          {[["Debian", m.install.debian], ["Ubuntu", m.install.ubuntu], ["Arch", m.install.arch]].map(([distro, cmd]) => (
                            <div key={distro} style={{ display: "flex", alignItems: "center", gap: 8, minWidth: 0 }}>
                              <span style={{ fontSize: 11.5, color: "var(--text-3)", fontFamily: "var(--font-mono)", width: 52, flexShrink: 0 }}>{distro}</span>
                              <Copyable text={cmd} />
                            </div>
                          ))}
                        </div>
                      )}
                      <div style={{ fontSize: 12, color: "var(--text-3)", fontFamily: "var(--font-mono)", marginTop: 6 }}>
                        {t.storage_missing_reason}: {m.reason}
                      </div>
                    </div>
                  ))}
                </div>
              </div>
            )}
            {/* Provisioning is minutes, not seconds — say so before the click,
                not with a spinner after it. */}
            <p className="hint" style={{ marginTop: 14 }}>{t.create_eta}</p>
          </div>
        )}

        {/* nav */}
        <div className="hr" />
        <div style={{ display: "flex", justifyContent: "space-between", gap: 12 }}>
          <Btn variant="outline" icon="chevL" data-testid="wizard-back"
            onClick={() => step === 0 ? onCancel() : setStep(s => s - 1)}>
            {step === 0 ? t.cancel : t.back}
          </Btn>
          {step < lastStep
            ? <Btn variant="primary" iconR="chevR" data-testid="wizard-next" disabled={step === 0 && !valid} onClick={() => setStep(s => s + 1)}>{t.next}</Btn>
            : <Btn variant="primary" icon="spark" data-testid="create-submit" disabled={!valid || creating || missingPkgs.length > 0} onClick={finish}>{t.create_btn}</Btn>}
        </div>
      </div>
    </div>
  );
}

export { CreateView };
