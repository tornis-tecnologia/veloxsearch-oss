// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
/* ============================================================
   Create view — guided 4-step wizard with a summary that is always visible.

     1 Básico    name, OpenSearch version, purpose, retention
     2 Tamanho   three comparable columns, "N fit in your cluster", the
                 capacity after creating, and the risks of THIS choice
     3 Backup    optional (ADR-049)
     4 Revisão   confirmation only — nothing new is decided here

   The summary aside repeats every choice with an "edit" link and shows what
   the cluster looks like after the create (memory and storage, from the same
   numbers the Capacity screen uses). The old wizard kept the size decision
   context-free and surfaced the single-copy risk only on Review, where the
   person had already decided.
   ============================================================ */
import React, { useState, useEffect, useRef } from "react";
import { STR, SIZES, sizeMeta } from "./i18n.jsx";
import { Icon, Field, Btn, Copyable, Banner, Confirm, PageHeader, fmtQty, fmtInt, fmtGib } from "./ui.jsx";
import { SnapshotStep } from "./views_snapshot.jsx";
import { API } from "./api.jsx";

const PURPOSES = ["observability", "security", "search"];
const OTHER_VERSION = "__other__";

// The name becomes a Kubernetes object name and a DNS label (ADR-020).
const NAME_RE = /^[a-z0-9]([-a-z0-9]*[a-z0-9])?$/;
const NAME_MAX = 40;

function nameError(raw, t) {
  const n = raw.trim();
  if (!n) return "";
  if (n.length > NAME_MAX) return t.name_err_long;
  if (!NAME_RE.test(n)) return t.name_err_chars;
  return "";
}

// ADR-062: the installation's defaults, until the server's arrive.
const BUILTIN_RETENTION = { observability_days: 30, security_days: 90 };
function defaultDays(purpose, defaults) {
  if (purpose === "search") return null;
  return purpose === "security" ? defaults.security_days : defaults.observability_days;
}

const GiB = 1024 ** 3;
// "2Gi" / "1536m" → bytes, for the capacity-after-create math.
function qtyBytes(q) {
  const m = String(q || "").trim().match(/^([\d.]+)\s*(Gi|Mi|Ki|Ti|G|M|K|T|g|m|k)?$/);
  if (!m) return 0;
  const mult = { Gi: GiB, g: GiB, G: GiB, Mi: 1024 ** 2, m: 1024 ** 2, M: 1024 ** 2, Ki: 1024, k: 1024, K: 1024, Ti: 1024 ** 4, T: 1024 ** 4, "": 1 }[m[2] || ""];
  return parseFloat(m[1]) * mult;
}

function PurposeCard({ p, selected, lang, onSelect, retention }) {
  const t = STR[lang];
  const days = (s, d) => s.replaceAll("{0}", String(d));
  const meta = {
    observability: { icon: "activity", title: t.p_obs_t, desc: t.p_obs_d, keep: days(t.p_obs_keep, retention.observability_days), col: t.p_obs_col, set: days(t.p_obs_set, retention.observability_days), best: t.p_obs_best },
    security: { icon: "shield", title: t.p_sec_t, desc: t.p_sec_d, keep: days(t.p_sec_keep, retention.security_days), col: t.p_sec_col, set: days(t.p_sec_set, retention.security_days), best: t.p_sec_best },
    search: { icon: "search", title: t.p_search_t, desc: t.p_search_d, keep: t.p_search_keep, col: t.p_search_col, set: t.p_search_set, best: t.p_search_best },
  }[p];
  const rows = [
    [t.keeps_data, meta.keep], [t.collects, meta.col], [t.sets_up, meta.set], [t.best_for, meta.best],
  ];
  return (
    <button type="button" className="purpose" aria-pressed={selected} onClick={() => onSelect(p)} data-testid={`purpose-${p}`}>
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

// One size as a column of the same rows, so the three compare at a glance.
function SizeCard({ k, v, selected, fit, lang, t, onSelect }) {
  const memB = qtyBytes(v.mem), diskB = qtyBytes(v.disk);
  // "Cabem <b>5</b> no seu cluster": the count is the thing to read.
  let fitNode = null;
  if (fit) {
    if (fit.count <= 0) fitNode = t.wiz_fit_none;
    else if (fit.count === 1) fitNode = t.wiz_fit_one;
    else { const [a, b] = t.wiz_fit.split("{0}"); fitNode = <>{a}<b>{fit.count}</b>{b}</>; }
  }
  return (
    <button type="button" className="purpose size-card" aria-pressed={selected} onClick={() => onSelect(k)} data-testid={`size-${k}`}>
      <span className="check"><Icon name="check" size={12} /></span>
      <div className="ph"><span className="picon"><Icon name="server" size={16} /></span>{t[`size_${k}`] || v.label}</div>
      {fitNode && <div className={`fit${fit.count <= 0 ? " none" : ""}`}>{fitNode}</div>}
      <dl>
        <dt>{t.wiz_mem_node}</dt><dd>{fmtQty(v.mem, lang)}</dd>
        <dt>{t.wiz_heap}</dt><dd>{fmtQty(v.heap, lang)}</dd>
        <dt>{t.wiz_disk_node}</dt><dd>{fmtQty(v.disk, lang)}</dd>
        <dt>{t.wiz_total_mem}</dt><dd>{fmtGib(memB * v.nodes / GiB, lang)}</dd>
        <dt>{t.wiz_total_disk}</dt><dd>{fmtGib(diskB * v.nodes / GiB, lang)}</dd>
      </dl>
      <div className="for">{t[`wiz_for_${k}`] || ""}</div>
    </button>
  );
}

function CreateView({ lang, hostNodes = [], capacity = null, onCreate, onCancel }) {
  const t = STR[lang];
  const fmt = (s, ...args) => s.replace(/\{(\d+)\}/g, (_, i) => String(args[+i] ?? "?"));
  const [step, setStep] = useState(0);
  const [name, setName] = useState("");
  const [purpose, setPurpose] = useState("observability");
  const [versions, setVersions] = useState([]);
  const [version, setVersion] = useState("");
  const [customVersion, setCustomVersion] = useState("");
  const [size, setSize] = useState("small");
  const [sources, setSources] = useState({ kubernetes: true, "k8s-events": false });
  const [advanced, setAdvanced] = useState(false);
  const [extra, setExtra] = useState("");
  const [sizes, setSizes] = useState(SIZES);
  const [customMem, setCustomMem] = useState("");
  const [customDisk, setCustomDisk] = useState("");
  const [customHeap, setCustomHeap] = useState("");
  const [storage, setStorage] = useState(null);
  const [installingStorage, setInstallingStorage] = useState(false);
  const [submitting, setSubmitting] = useState(false);
  const submitted = useRef(false);
  const [snapshot, setSnapshot] = useState(null);
  const [retention, setRetention] = useState(BUILTIN_RETENTION);
  const [retOverride, setRetOverride] = useState("");
  const [cancelAsk, setCancelAsk] = useState(false);

  // Host-cluster honesty (#26/#32): both warnings are SILENT when hostNodes
  // is empty (capacity unknown).
  const singleCopy = hostNodes.length > 0 && hostNodes.length < 3;
  const kernelIncompat =
    hostNodes.some(n => (n.kernel_version || "").startsWith("6.1.0-52")) &&
    version.startsWith("3.8");

  const steps = [t.step_purpose, t.step_size, t.step_backup, t.step_review];
  const lastStep = steps.length - 1;
  const sz = sizes[size] || sizeMeta(size);

  useEffect(() => {
    let live = true;
    API.sizingPresets()
      .then((list) => {
        if (!live || !Array.isArray(list) || !list.length) return;
        const m = {};
        list.forEach((p) => { m[p.name] = p; });
        setSizes(m);
      })
      .catch(() => {});
    return () => { live = false; };
  }, []);

  useEffect(() => {
    let live = true;
    API.retentionDefaults()
      .then((d) => { if (live && d && d.observability_days) setRetention(d); })
      .catch(() => {});
    return () => { live = false; };
  }, []);

  useEffect(() => {
    let live = true;
    API.availableVersions()
      .then((v) => {
        if (!live || !v || !Array.isArray(v.versions) || !v.versions.length) return;
        setVersions(v.versions);
        setVersion((prev) => prev || v.default || v.versions[0]);
      })
      .catch(() => {});
    return () => { live = false; };
  }, []);

  useEffect(() => {
    if (!advanced) return undefined;
    let live = true;
    API.customSizing(customMem, customDisk)
      .then((p) => { if (live && p && p.heap) setCustomHeap(p.heap); })
      .catch(() => {});
    return () => { live = false; };
  }, [advanced, customMem, customDisk]);

  useEffect(() => {
    let live = true;
    API.storageStatus()
      .then((s) => { if (live && s) setStorage(s); })
      .catch(() => {});
    return () => { live = false; };
  }, []);

  useEffect(() => {
    if (!installingStorage) return undefined;
    let live = true;
    const tick = () =>
      API.storageStatus().then((s) => { if (live && s) setStorage(s); }).catch(() => {});
    tick();
    const h = setInterval(tick, 3000);
    return () => { live = false; clearInterval(h); };
  }, [installingStorage]);

  const chosenVersion = version === OTHER_VERSION ? customVersion.trim() : version;
  const nameErr = nameError(name, t);
  const effDefault = defaultDays(purpose, retention);
  const retTrim = retOverride.trim();
  const retErr = retTrim && !(/^\d+$/.test(retTrim) && +retTrim >= 1 && +retTrim <= 3650) ? t.ret_days_err : "";
  const retDays = purpose === "search" ? null : (retTrim && !retErr ? +retTrim : effDefault);
  const valid = name.trim().length > 0 && !nameErr && !retErr
    && (version !== OTHER_VERSION || !!chosenVersion);

  const willInstallStorage = !!(storage && storage.needs_longhorn && !storage.durable);
  const missingPkgs = (storage && storage.missing_packages) || [];

  // What this choice reserves, and how the cluster looks afterwards — the
  // same totals the Capacity screen reads (requested when metrics are off).
  const memNode = advanced ? customMem.trim() || sz.mem : sz.mem;
  const diskNode = advanced ? customDisk.trim() || sz.disk : sz.disk;
  const nodes = sz.nodes || 3;
  const addMem = qtyBytes(memNode) * nodes;
  const addDisk = qtyBytes(diskNode) * nodes;
  const capRow = (res, add, total) => {
    if (!res || !total) return null;
    const used = res.used != null && capacity && capacity.metrics_available ? res.used : (res.requested ?? res.used ?? 0);
    const before = (used / total) * 100;
    const after = ((used + add) / total) * 100;
    return { before, after, over: used + add > total };
  };
  const capMem = capacity ? capRow(capacity.mem, addMem, capacity.mem && capacity.mem.total) : null;
  const capDisk = capacity && capacity.storage ? capRow(capacity.storage, addDisk, capacity.storage.total) : null;
  const fitFor = (k) => (capacity && Array.isArray(capacity.fit)) ? capacity.fit.find(f => f.size === k) : null;

  async function finish() {
    if (submitted.current) return;
    submitted.current = true;
    setSubmitting(true);
    if (willInstallStorage) setInstallingStorage(true);
    try {
      await onCreate({
        name: name.trim() || "logs", purpose, size, sources, version: chosenVersion,
        extra: advanced ? extra : "",
        memory: advanced ? customMem.trim() : "",
        disk: advanced ? customDisk.trim() : "",
        snapshot,
        retentionDays: purpose !== "search" && retTrim && !retErr ? +retTrim : null,
      });
    } catch (e) {
      submitted.current = false;
      setSubmitting(false);
      setInstallingStorage(false);
    }
  }

  const dirty = name.trim() || step > 0;
  const purposeLabel = STR[lang]["p_" + (purpose === "observability" ? "obs" : purpose === "security" ? "sec" : "search") + "_t"];
  const sizeLabel = advanced ? t.size_custom : (t[`size_${size}`] || sz.label);

  const summary = (
    <div className="card pad summary" data-testid="wizard-summary">
      <h3 className="section-title">{t.wiz_summary}</h3>
      <dl>
        <dt>{t.name}</dt><dd>{name.trim() ? <span style={{ whiteSpace: "nowrap" }}>{name.trim()}<span style={{ color: "var(--text-3)" }}>-····</span></span> : "—"}{step !== 0 && <button type="button" className="edit" onClick={() => setStep(0)}>{t.wiz_edit}</button>}</dd>
        <dt>{t.review_purpose}</dt><dd className="sans">{purposeLabel}</dd>
        <dt>{t.version_label}</dt><dd>{chosenVersion || "—"}</dd>
        {purpose !== "search" && <><dt>{t.ret_label}</dt><dd>{retDays ? fmt(t.ret_days_value, retDays) : "—"}</dd></>}
        <dt>{t.review_size}</dt><dd className="sans">{sizeLabel} · <span className="mono">{fmtQty(memNode, lang)} · {fmtQty(diskNode, lang)}</span>{step !== 1 && <button type="button" className="edit" onClick={() => setStep(1)}>{t.wiz_edit}</button>}</dd>
        <dt>{t.review_backup}</dt><dd className="sans">{snapshot ? (snapshot.bucket || t.wiz_backup_on) : t.wiz_backup_off}{step !== 2 && <button type="button" className="edit" onClick={() => setStep(2)}>{t.wiz_edit}</button>}</dd>
      </dl>
      {(capMem || capDisk) && (
        <div className="cap">
          <div className="ct">{t.wiz_cap_h}</div>
          {[[t.wiz_cap_mem, capMem], [t.wiz_cap_disk, capDisk]].filter(([, c]) => c).map(([label, c]) => (
            <div className="row" key={label}>
              <div className="h"><span>{label}</span><b>{fmtInt(c.before, lang)}% → {fmtInt(Math.min(999, c.after), lang)}%</b></div>
              <div className="bar2">
                <i className={c.over ? "over" : "add"} style={{ width: `${Math.min(100, c.after)}%` }} />
                <i style={{ width: `${Math.min(100, c.before)}%` }} />
              </div>
              {c.over && <span className="hint" style={{ color: "var(--danger-text)", fontSize: 12 }}>{fmt(t.wiz_cap_over, label.toLowerCase())}</span>}
            </div>
          ))}
        </div>
      )}
    </div>
  );

  return (
    <div className="view-enter">
      <PageHeader title={t.nav_create} lead={t.create_lead} />

      <div className="stepper">
        {steps.map((s, i) => (
          <React.Fragment key={i}>
            <button type="button" className={`step ${i === step ? "active" : ""} ${i < step ? "done" : ""}`}
              style={{ background: "none", border: "none", padding: 0 }} disabled={i > step} onClick={() => i < step && setStep(i)}>
              <span className="node">{i < step ? <Icon name="check" size={13} /> : i + 1}</span>
              <span className="txt">{s}</span>
            </button>
            {i < steps.length - 1 && <span className={`line ${i < step ? "done" : ""}`} />}
          </React.Fragment>
        ))}
      </div>

      <div className="wizard">
        <div className="card pad">
          {/* STEP 1 — basics */}
          {step === 0 && (
            <div className="view-enter">
              <h2 className="sub-title">{t.wiz_basic_h}</h2>
              <p className="hint" style={{ marginBottom: 16 }}>{t.name_hint}</p>
              <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(220px, 1fr))", gap: 14 }}>
                <Field label={t.name} htmlFor="create-name" error={nameErr}>
                  <input id="create-name" className={`input mono${nameErr ? " invalid" : ""}`} name="name"
                    placeholder={t.name_ph} value={name}
                    onChange={e => setName(e.target.value)} autoFocus />
                </Field>
                <Field label={t.version_label} hint={t.version_hint} htmlFor="create-version">
                  <select id="create-version" className="select mono" value={version}
                    onChange={e => setVersion(e.target.value)}>
                    {versions.map((v, i) => (
                      <option key={v} value={v}>{v}{i === 0 ? ` · ${t.version_latest}` : ""}</option>
                    ))}
                    <option value={OTHER_VERSION}>{t.version_other}</option>
                  </select>
                </Field>
              </div>
              {version === OTHER_VERSION && (
                <Field label={t.version_manual} hint={t.version_manual_hint} htmlFor="create-version-custom">
                  <input id="create-version-custom" className="input mono" placeholder="3.8.0"
                    value={customVersion} onChange={e => setCustomVersion(e.target.value)} />
                </Field>
              )}
              {kernelIncompat && <Banner tone="warn" title={t.kernel_bad_h}>{t.kernel_bad_p}</Banner>}

              <div className="section-title" style={{ marginTop: 10 }}>{t.purpose_q}</div>
              <div className="purpose-grid">
                {PURPOSES.map(p => (
                  <PurposeCard key={p} p={p} selected={purpose === p} lang={lang} retention={retention}
                    onSelect={(np) => { setPurpose(np); setRetOverride(""); }} />
                ))}
              </div>
              {purpose !== "search" && (
                <div style={{ marginTop: 16, maxWidth: 360 }}>
                  <Field label={t.ret_label} htmlFor="create-retention" error={retErr}
                    hint={fmt(t.ret_wizard_hint, effDefault)}>
                    <input id="create-retention" className={`input${retErr ? " invalid" : ""}`} inputMode="numeric"
                      data-testid="create-retention" placeholder={String(effDefault)} value={retOverride}
                      onChange={e => setRetOverride(e.target.value)} />
                  </Field>
                </div>
              )}
            </div>
          )}

          {/* STEP 2 — size */}
          {step === 1 && (
            <div className="view-enter">
              <h2 className="sub-title">{t.wiz_size_q}</h2>
              <p className="hint" style={{ marginBottom: 16 }}>{t.wiz_size_p}</p>
              {!advanced && (
                <div className="purpose-grid">
                  {Object.entries(sizes).map(([k, v]) => (
                    <SizeCard key={k} k={k} v={v} selected={size === k} fit={fitFor(k)} lang={lang} t={t} onSelect={setSize} />
                  ))}
                </div>
              )}
              <button type="button" className="btn-link" style={{ marginTop: 12 }} data-testid="size-advanced" onClick={() => setAdvanced(a => {
                const next = !a;
                if (next && !customMem) { setCustomMem(sz.mem); setCustomDisk(sz.disk); }
                return next;
              })}>
                <Icon name={advanced ? "chevD" : "chevR"} size={13} />
                {advanced ? t.wiz_custom_close : t.wiz_custom}
              </button>
              {advanced && (
                <div className="view-enter" style={{ marginTop: 14, display: "grid", gap: 14, gridTemplateColumns: "repeat(auto-fit, minmax(200px, 1fr))" }}>
                  <Field label={t.node_mem} tip={t.node_mem_hint} htmlFor="create-mem">
                    <input id="create-mem" className="input mono" value={customMem} onChange={e => setCustomMem(e.target.value)} />
                  </Field>
                  <Field label={t.disk} htmlFor="create-disk">
                    <input id="create-disk" className="input mono" value={customDisk} onChange={e => setCustomDisk(e.target.value)} />
                  </Field>
                  <div style={{ gridColumn: "1 / -1", display: "flex", gap: 24, flexWrap: "wrap" }}>
                    <div className="fact">{t.jvm}: <b>{fmtQty(customHeap || sz.heap, lang)}</b></div>
                    <div className="fact">{t.node_count}: <b>{sz.nodes}</b></div>
                  </div>
                  <div style={{ gridColumn: "1 / -1" }}>
                    <Field label={t.extra} hint={t.extra_hint} htmlFor="create-extra">
                      <textarea id="create-extra" className="textarea mono" value={extra} onChange={e => setExtra(e.target.value)}
                        placeholder={"indices.query.bool.max_clause_count: 2048"} />
                    </Field>
                  </div>
                </div>
              )}
              {/* The risks of THIS choice, where the choice is made. */}
              <div style={{ marginTop: 18 }}>
                {singleCopy && (
                  <Banner tone="warn" title={t.single_copy_h}>{fmt(t.single_copy_p, hostNodes.length)}</Banner>
                )}
                {willInstallStorage && !installingStorage && (
                  <Banner tone="info" icon="server" title={t.storage_will_install_h}>{t.storage_will_install_p}</Banner>
                )}
              </div>
            </div>
          )}

          {/* STEP 3 — backup (optional, ADR-047) */}
          {step === 2 && (
            <SnapshotStep cfg={snapshot} onChange={setSnapshot} lang={lang} />
          )}

          {/* STEP 4 — review: confirmation only */}
          {step === 3 && (
            <div className="view-enter">
              <h2 className="sub-title">{t.review_h}</h2>
              <p className="hint" style={{ marginBottom: 16 }}>{t.review_p}</p>
              <div style={{ background: "var(--surface-2)", border: "1px solid var(--border)", borderRadius: "var(--radius)", padding: "4px 16px" }}>
                <div className="kvrow"><span className="k">{t.review_name}</span>
                  <span className="v">{name.trim() || "—"}<span style={{ color: "var(--text-3)" }}> · {t.review_suffix}</span></span>
                </div>
                <div className="kvrow"><span className="k">{t.review_purpose}</span><span className="v sans">{purposeLabel}</span></div>
                <div className="kvrow"><span className="k">{t.ret_label}</span>
                  <span className="v" data-testid="review-retention">{retDays ? fmt(t.ret_days_value, retDays) : t.ret_forever}</span>
                </div>
                <div className="kvrow"><span className="k">{t.version_label}</span><span className="v">{chosenVersion || "—"}</span></div>
                <div className="kvrow"><span className="k">{t.review_size}</span><span className="v sans">{sizeLabel} · <span className="mono">{nodes} {t.nodes} · {fmtQty(memNode, lang)} · {fmtQty(diskNode, lang)}</span></span></div>
                <div className="kvrow">
                  <span className="k">{t.review_sources}</span>
                  <span className="v sans">
                    {purpose === "search" ? "API" : (
                      <label className="check-row" data-testid="rev-k3s-toggle" style={{ justifyContent: "flex-end" }}>
                        <input type="checkbox" checked={!!sources.kubernetes}
                          onChange={e => setSources(s => ({ ...s, kubernetes: e.target.checked }))} />
                        {sources.kubernetes ? t.rev_k3s_label : t.review_none}
                      </label>
                    )}
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
              {purpose === "observability" && <p className="hint" style={{ marginTop: 12 }}>{t.review_otel_hint}</p>}

              {installingStorage && (
                <Banner tone="info" icon="server" style={{ marginTop: 14 }}
                  title={storage?.durable ? t.created_h : t.storage_installing_h}>
                  {storage?.durable ? t.storage_installed_p : t.storage_installing_p}
                  {storage?.installing && !storage?.durable && <div className="mono" style={{ marginTop: 6, fontSize: 12.5 }}>{t.storage_step}: {storage.installing}</div>}
                  {storage?.error && <pre style={{ color: "var(--danger-text)" }}>{storage.error}</pre>}
                </Banner>
              )}
              {missingPkgs.length > 0 && (
                <Banner tone="danger" icon="shield" title={t.storage_missing_h} style={{ marginTop: 14 }}>
                  <div style={{ marginBottom: 10 }}>{t.storage_missing_p}</div>
                  {missingPkgs.map((m, i) => (
                    <div key={i} style={{ padding: "10px 12px", marginTop: i ? 10 : 0, borderRadius: "var(--radius)", background: "var(--surface-2)", border: "1px solid var(--border)" }}>
                      <div className="mono" style={{ fontSize: 13, marginBottom: 6 }}>
                        <strong>{m.node}</strong>
                        {m.package
                          ? <> · {t.storage_missing_pkg}: <span style={{ color: "var(--danger-text)" }}>{m.package}</span></>
                          : <span style={{ color: "var(--text-3)" }}> · {t.storage_missing_unknown}</span>}
                      </div>
                      {m.install && (
                        <div style={{ display: "grid", gap: 6 }}>
                          {[["Debian", m.install.debian], ["Ubuntu", m.install.ubuntu], ["Arch", m.install.arch]].map(([distro, cmd]) => (
                            <div key={distro} style={{ display: "flex", alignItems: "center", gap: 8, minWidth: 0 }}>
                              <span className="mono" style={{ fontSize: 11.5, color: "var(--text-3)", width: 52, flexShrink: 0 }}>{distro}</span>
                              <Copyable text={cmd} t={t} />
                            </div>
                          ))}
                        </div>
                      )}
                      <div className="mono" style={{ fontSize: 12, color: "var(--text-3)", marginTop: 6 }}>{t.storage_missing_reason}: {m.reason}</div>
                    </div>
                  ))}
                </Banner>
              )}
              <p className="hint" style={{ marginTop: 14 }}>{t.create_eta}</p>
            </div>
          )}

          <div className="wiz-foot">
            <Btn variant="outline" icon="chevL" data-testid="wizard-back"
              onClick={() => step === 0 ? (dirty ? setCancelAsk(true) : onCancel()) : setStep(s => s - 1)}>
              {step === 0 ? t.cancel : t.back}
            </Btn>
            {step < lastStep
              ? <Btn variant="primary" iconR="chevR" data-testid="wizard-next" disabled={step === 0 && !valid} onClick={() => setStep(s => s + 1)}>{t.next}</Btn>
              : <Btn variant="primary" icon={submitting ? undefined : "spark"} data-testid="create-submit"
                  aria-busy={submitting} disabled={!valid || submitting || missingPkgs.length > 0} onClick={finish}>
                  {submitting && <span className="btn-spinner" aria-hidden="true" />}
                  {submitting ? t.create_submitting : t.create_btn}
                </Btn>}
          </div>
        </div>

        <div className="aside">{summary}</div>
      </div>

      <Confirm open={cancelAsk} title={t.wiz_cancel_h} body={t.wiz_cancel_p} icon="x"
        confirmLabel={t.wiz_cancel_yes} cancelLabel={t.wiz_keep}
        onCancel={() => setCancelAsk(false)} onConfirm={() => { setCancelAsk(false); onCancel(); }} />
    </div>
  );
}

export { CreateView };
