// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
/* ============================================================
   Settings view — dashboard access (port-forward / ingress), plus the
   About block (#55): which build is serving.
   Self-loads from /api/access_settings, saves via /api/save_access_settings.
   Build info is fetched once by the app shell and passed in.
   ============================================================ */
import { useState, useEffect } from "react";
import { STR } from "./i18n.jsx";
import { API } from "./api.jsx";
import { Field, Btn, Icon, Copyable, Confirm, FormSection, PageHeader, Banner } from "./ui.jsx";

// Local, tiny: this view had no formatter and needs exactly one.
function fmtS(s, ...args) {
  return args.reduce((acc, v, i) => acc.replaceAll(`{${i}}`, v), s || "");
}

// What the backend sends for a runtime fact it could not read.
const UNAVAILABLE = "unavailable";

// `sha256:` + 12 hex — the length `docker images` shows; the copy button
// still copies the full value.
function shortDigest(d) {
  return d.startsWith("sha256:") ? d.slice(0, 19) : d;
}

// About this installation (#55). Every value is the server's word: version and
// commit compiled into the binary, digest and operator read from the cluster.
// When a fact is unavailable the reason is shown, not hidden.
function AboutBlock({ info, t, onToast }) {
  const unavailable = (note) => (
    <span style={{ color: "var(--text-3)" }} title={note || undefined}>{t.about_unavailable}</span>
  );
  const rows = [
    [t.about_version, <span data-testid="about-version">{info.version}</span>],
    [t.about_commit, info.commit === "unknown"
      ? <span data-testid="about-commit" style={{ color: "var(--text-3)" }}>{t.about_commit_unknown}</span>
      : <span data-testid="about-commit"><Copyable text={info.commit} display={info.commit.slice(0, 7)} t={t} onCopy={onToast} /></span>],
    [t.about_image, info.image_digest === UNAVAILABLE
      ? unavailable(info.image_note)
      : <Copyable text={info.image_digest} display={shortDigest(info.image_digest)} t={t} onCopy={onToast} />],
    [t.about_operator, info.operator_image === UNAVAILABLE
      ? unavailable(info.operator_note)
      : <span title={info.operator_deployment}>{info.operator_image}</span>],
    [t.about_catalog, <span style={{ wordBreak: "break-all" }}>{info.catalog_source}</span>],
  ];
  const notes = [info.image_note, info.operator_note].filter(Boolean);
  return (
    <div data-testid="about-panel">
      {rows.map(([k, v]) => (
        <div className="kvrow" key={k}><span className="k">{k}</span><span className="v">{v}</span></div>
      ))}
      {/* One note per distinct reason: off-cluster both facts fail for the
          same reason, and saying it twice reads like two problems. */}
      {[...new Set(notes)].map(n => (
        <p key={n} data-testid="about-note"
          style={{ fontSize: 12, color: "var(--text-3)", fontFamily: "var(--font-mono)", marginBottom: 0, overflowWrap: "anywhere" }}>{n}</p>
      ))}
    </div>
  );
}

// Default retention per purpose (ADR-062). Admin-only: saving changes what NEW
// deployments get; existing ones move only through the explicit apply button,
// which skips any deployment whose user customized the policy in OpenSearch.
function RetentionBlock({ t, onToast }) {
  const [obs, setObs] = useState("");
  const [sec, setSec] = useState("");
  const [busy, setBusy] = useState(false);
  const [confirmApply, setConfirmApply] = useState(false);
  const [report, setReport] = useState(null);

  useEffect(() => {
    let alive = true;
    API.retentionDefaults()
      .then(d => {
        if (!alive || !d) return;
        setObs(String(d.observability_days));
        setSec(String(d.security_days));
      })
      .catch(() => {});
    return () => { alive = false; };
  }, []);

  const valid = v => /^\d+$/.test(v.trim()) && +v >= 1 && +v <= 3650;
  const blocked = !valid(obs) || !valid(sec);

  async function save() {
    setBusy(true);
    try {
      await API.saveRetentionDefaults(+obs, +sec);
      onToast(t.saved);
    } catch (e) {
      onToast(e.message);
    } finally {
      setBusy(false);
    }
  }

  async function applyAll() {
    setConfirmApply(false);
    setBusy(true);
    try {
      setReport(await API.applyDefaultRetention());
    } catch (e) {
      onToast(e.message);
    } finally {
      setBusy(false);
    }
  }

  const outcomeLabel = o => t["ret_outcome_" + o] || o;
  return (
    <div data-testid="retention-defaults">
      <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(160px, 1fr))", gap: 14 }}>
        <Field label={t.ret_obs_days} hint={t.ret_days_hint} error={valid(obs) || !obs ? "" : t.ret_days_err} htmlFor="ret-obs">
          <input id="ret-obs" className="input" inputMode="numeric" value={obs} onChange={e => setObs(e.target.value)} />
        </Field>
        <Field label={t.ret_sec_days} error={valid(sec) || !sec ? "" : t.ret_days_err} htmlFor="ret-sec">
          <input id="ret-sec" className="input" inputMode="numeric" value={sec} onChange={e => setSec(e.target.value)} />
        </Field>
      </div>
      <p className="hint" style={{ marginBottom: 12 }}>{t.ret_search_none}</p>
      <div style={{ display: "flex", gap: 10, flexWrap: "wrap" }}>
        <Btn variant="primary" icon="check" disabled={busy || blocked} onClick={save}>{t.save}</Btn>
        <Btn variant="outline" disabled={busy} onClick={() => setConfirmApply(true)}
          data-testid="retention-apply-all">{t.ret_apply_all}</Btn>
      </div>
      {report && (
        <div style={{ marginTop: 14 }} data-testid="retention-apply-report">
          {report.length === 0 && <p className="hint">{t.ret_apply_none}</p>}
          {report.map(r => (
            <div className="kvrow" key={`${r.namespace}/${r.name}`}>
              <span className="k">{r.name}</span>
              <span className="v" title={r.detail || undefined}>
                {outcomeLabel(r.outcome)}{r.days ? ` · ${r.days}d` : ""}
                {r.detail && <span style={{ color: "var(--danger)" }}> — {r.detail}</span>}
                {(r.otel || []).length > 0 && (
                  <span style={{ display: "block", fontSize: 12, color: "var(--text-3)" }}>{t.ret_otel_h}</span>
                )}
                {(r.otel || []).map(o => (
                  <span key={o.policy_id} style={{ display: "block", fontSize: 12 }} title={o.detail || undefined}>
                    {o.policy_id}: {outcomeLabel(o.state)}
                    {o.detail && <span style={{ color: "var(--danger)" }}> — {o.detail}</span>}
                  </span>
                ))}
              </span>
            </div>
          ))}
        </div>
      )}
      <Confirm open={confirmApply} title={t.ret_apply_all} body={t.ret_apply_confirm}
        confirmLabel={t.ret_apply_all} cancelLabel={t.cancel} icon="check" variant="primary"
        confirmTestid="retention-apply-confirm"
        onCancel={() => setConfirmApply(false)} onConfirm={applyAll} />
    </div>
  );
}

// OpenSearch operator upgrade (#176, ADR-066). Every verdict is the server's:
// what changes, what it costs each deployment, what blocks it. The button
// arms only once a cluster admin has created the one-time grant, and the
// server re-checks everything when it is pressed.
function OperatorBlock({ t, onToast }) {
  const [r, setR] = useState(null);
  const [err, setErr] = useState("");
  const [confirm, setConfirm] = useState(false);
  const [busy, setBusy] = useState(false);

  async function load() {
    try {
      const s = await API.operatorUpgradeStatus();
      // A backend older than this route answers it with the SPA's index.html
      // (the fallback, 200): a string, not the status. Treat anything without
      // the shape as "unavailable" instead of taking the whole screen down.
      if (!s || typeof s !== "object" || !s.job || !s.grant) {
        setR(null);
        setErr(t.about_unavailable);
        return;
      }
      setR(s);
      setErr("");
    } catch (e) {
      setErr(e.message);
    }
  }
  useEffect(() => { load(); }, []);

  // Poll while something can change under us: the job running, or the grant
  // the admin is about to create in another terminal.
  const watching = r && (r.job.state === "running" || (r.state === "available" && !r.grant.present));
  useEffect(() => {
    if (!watching) return undefined;
    const id = setInterval(load, 5000);
    return () => clearInterval(id);
  }, [watching]);

  async function start() {
    setConfirm(false);
    setBusy(true);
    try {
      await API.operatorUpgrade();
      await load();
    } catch (e) {
      onToast(e.message);
    } finally {
      setBusy(false);
    }
  }

  // One sentence per finding code; the server's English `detail` when a
  // code is new to this build.
  const say = f => (t["opu_f_" + f.code]
    ? fmtS(t["opu_f_" + f.code], f.deployment, f.subject)
    : f.detail);

  const card = children => <div data-testid="operator-upgrade">{children}</div>;
  if (err) return card(<p className="hint" style={{ color: "var(--danger-text)" }}>{err}</p>);
  if (!r) return card(<p className="hint">{t.opu_loading}</p>);
  if (r.state === "unmanaged") {
    return card(<p className="hint" data-testid="opu-unmanaged">{r.blockers.map(say).join(" ")}</p>);
  }

  const job = r.job;
  const downPools = r.warnings.filter(w => w.code === "single_replica_pool");
  const removed = r.crds.flatMap(c => c.removed_fields.map(f => `${c.name} ${f}`));
  const versionsGone = r.crds.flatMap(c => c.removed_versions.map(v => `${c.name} ${v}`));

  return card(<>
    <div className="kvrow"><span className="k">{t.opu_running}</span><span className="v" data-testid="opu-running">{r.running}</span></div>
    <div className="kvrow"><span className="k">{t.opu_vendored}</span><span className="v">{r.vendored}</span></div>

    {r.state === "up_to_date" && job.state !== "done" && (
      <p className="hint" data-testid="opu-up-to-date">{t.opu_up_to_date}</p>
    )}

    {job.state === "running" && (
      <p className="hint" data-testid="opu-job-running">{fmtS(t["opu_step_" + job.step] || job.step, job.to)}</p>
    )}
    {job.state === "done" && (
      <p className="hint" style={{ color: "var(--ok-text)" }} data-testid="opu-job-done">{fmtS(t.opu_done, job.from, job.to)}</p>
    )}
    {job.state === "failed" && (
      <p className="hint" style={{ color: "var(--danger-text)", overflowWrap: "anywhere" }} data-testid="opu-job-failed">
        {fmtS(t.opu_failed, job.error)}
      </p>
    )}

    {r.state === "available" && job.state !== "running" && (<>
      <p style={{ fontSize: 12.5, color: "var(--text-3)" }}>{t.opu_lead}</p>

      <h4 className="section-title" style={{ fontSize: 13 }}>{t.opu_impact_h}</h4>
      <p className="hint" data-testid="opu-crds">
        {removed.length || versionsGone.length
          ? fmtS(t.opu_crd_removes, [...versionsGone, ...removed].join(", "))
          : t.opu_crd_additive}
      </p>
      {r.deployments.length === 0 && <p className="hint">{t.opu_no_deployments}</p>}
      {r.deployments.map(d => (
        <div className="kvrow" key={`${d.namespace}/${d.name}`}>
          <span className="k">{d.name}</span>
          <span className="v">
            {d.health} · {d.pools.map(p => `${p.name} ×${p.replicas}`).join(", ")}
          </span>
        </div>
      ))}
      <p className="hint">{t.opu_restart_note}</p>

      {r.blockers.length > 0 && (
        <Banner tone="danger" title={t.opu_blockers_h} testid="opu-blockers">
          {r.blockers.map((b, n) => <div key={n}>{say(b)}</div>)}
        </Banner>
      )}
      {r.warnings.length > 0 && (
        <Banner tone="warn" title={t.opu_warnings_h} testid="opu-warnings">
          {r.warnings.map((w, n) => <div key={n}>{say(w)}</div>)}
        </Banner>
      )}

      <h4 className="section-title" style={{ fontSize: 13 }}>{t.opu_grant_h}</h4>
      {r.grant.present
        ? <p className="hint" data-testid="opu-grant-present">
            {fmtS(t.opu_grant_present, String(Math.ceil(r.grant.expires_in_secs / 60)))}
          </p>
        : <>
            <p className="hint">{t.opu_grant_needed}</p>
            <Copyable text={r.grant.command} display={r.grant.command} t={t} onCopy={onToast} />
          </>}

      <div style={{ marginTop: 12 }}>
        <Btn variant="primary" icon="layers" disabled={busy || !r.can_upgrade}
          onClick={() => setConfirm(true)} data-testid="opu-start">
          {fmtS(t.opu_button, r.vendored)}
        </Btn>
        {!r.can_upgrade && (
          <p className="hint" style={{ marginTop: 6 }}>
            {r.blockers.length ? t.opu_why_blocked : !r.grant.present ? t.opu_why_grant : ""}
          </p>
        )}
      </div>

      <Confirm open={confirm} title={fmtS(t.opu_button, r.vendored)}
        body={fmtS(t.opu_confirm, r.running, r.vendored)}
        confirmLabel={t.opu_confirm_go} cancelLabel={t.cancel} icon="layers" variant="primary"
        confirmTestid="opu-confirm" onCancel={() => setConfirm(false)} onConfirm={start}>
        {downPools.length > 0 && (
          <p className="hint" style={{ color: "var(--warn-text)" }}>
            {fmtS(t.opu_confirm_downtime, downPools.map(p => `${p.deployment} ${p.subject}`).join(", "))}
          </p>
        )}
      </Confirm>
    </>)}
  </>);
}

function SettingsView({ lang, onToast, buildInfo, isTenant }) {
  const t = STR[lang];
  const [access, setAccess] = useState("portforward");
  const [domain, setDomain] = useState("");
  // What an empty domain resolves to, computed by the backend from the ingress
  // controller's address.
  const [defaultDomain, setDefaultDomain] = useState("");
  const [ingress, setIngress] = useState("");
  const [classes, setClasses] = useState([]);
  const [tlsSecret, setTlsSecret] = useState("");
  const [tlsCert, setTlsCert] = useState("");
  const [tlsKey, setTlsKey] = useState("");
  const [busy, setBusy] = useState(false);
  // What the server currently has, so the form can tell the user what their
  // change will actually do.
  const [loaded, setLoaded] = useState(null);

  useEffect(() => {
    let alive = true;
    API.getAccessSettings()
      .then(s => {
        if (!alive || !s) return;
        setLoaded(s);
        setAccess(s.mode || "portforward");
        setDomain(s.base_domain || "");
        setIngress(s.ingress_class || (s.available_classes && s.available_classes[0]) || "");
        setClasses(s.available_classes || []);
        setTlsSecret(s.tls_secret || "");
        setDefaultDomain(s.default_base_domain || "");
      })
      .catch(() => {});
    return () => { alive = false; };
  }, []);

  async function save() {
    setBusy(true);
    try {
      await API.saveAccessSettings(access, domain, ingress, tlsSecret, tlsCert, tlsKey);
      // PEM material is one-shot input: it becomes a Secret server-side and is
      // never echoed back, so clear the fields after a successful save.
      setTlsCert(""); setTlsKey("");
      if (tlsCert.trim() && !tlsSecret.trim()) setTlsSecret("veloxsearch-dashboards-tls");
      onToast(t.saved);
    } catch (e) {
      onToast(e.message);
    } finally {
      setBusy(false);
    }
  }

  // Offer only the IngressClasses the cluster has (#139): the backend refuses
  // any other, because every create would then fail on its route. A stored
  // class the cluster lacks stays listed so the select shows what is saved.
  const classOptions = !ingress || classes.includes(ingress) ? classes : [ingress, ...classes];
  const classErr = access === "ingress" && loaded
    ? (!classes.length ? t.ingress_no_class
      : !classes.includes(ingress) ? fmtS(t.ingress_class_missing, ingress) : "")
    : "";

  // Cheap, honest PEM check: catches the overwhelmingly common mistake (pasting
  // the wrong file, or a fingerprint) without pretending to validate a
  // certificate — that is the cluster's job at apply time.
  const pemErr = (v, marker) =>
    v.trim() && !v.includes(`-----BEGIN ${marker}`) ? t.tls_pem_err : "";
  const certErr = pemErr(tlsCert, "CERTIFICATE");
  const keyErr = tlsKey.trim() && !tlsKey.includes("-----BEGIN") ? t.tls_pem_err : "";
  // Cert and key travel together: one without the other cannot terminate TLS.
  const pairErr = (tlsCert.trim() && !tlsKey.trim()) || (!tlsCert.trim() && tlsKey.trim())
    ? t.tls_pair_err : "";
  // Leaving the domain empty is a valid choice — it means "use the sslip.io
  // one" — but only when we could actually detect an ingress address to build
  // it from. Without one there is nothing to fall back to, so the field
  // becomes required rather than silently failing on save.
  const effectiveDomain = domain.trim() || defaultDomain;
  const domainMissing = access === "ingress" && !effectiveDomain;
  const blocked = !!(certErr || keyErr || pairErr || classErr) || domainMissing;
  // Switching away from ingress orphans the per-deployment Ingresses.
  const modeChanged = loaded && access !== loaded.mode;

  return (
    <div className="view-enter">
      <PageHeader title={t.settings_h} lead={t.settings_lead} />

      <FormSection title={t.set_h_access} desc={t.set_d_access} testid="set-access"
        actions={<Btn variant="primary" icon="check" data-testid="access-save" disabled={busy || blocked} onClick={save}>{t.save}</Btn>}>
        <div className="purpose-grid two" style={{ marginBottom: 14 }}>
          {[
            { v: "portforward", title: t.portfwd_t, desc: t.portfwd_d, icon: "server" },
            { v: "ingress", title: t.ingress_t, desc: t.ingress_d, icon: "globe" },
          ].map(o => (
            <button key={o.v} type="button" className="purpose row" aria-pressed={access === o.v} onClick={() => setAccess(o.v)}
              data-testid={`access-${o.v}`}>
              <span className="check"><Icon name="check" size={12} /></span>
              <span className="picon"><Icon name={o.icon} size={17} /></span>
              <span style={{ textAlign: "left", minWidth: 0 }}>
                <div className="ph">{o.title}</div>
                <div className="pd">{o.desc}</div>
              </span>
            </button>
          ))}
        </div>

        {/* Changing the mode is not a preference toggle: leaving ingress takes
            every deployment's public URL down. Say it where the choice is. */}
        {modeChanged && (
          <Banner tone="warn">{access === "portforward" ? t.access_leave_ingress : t.access_enter_ingress}</Banner>
        )}

        {access === "ingress" && (
          <div className="view-enter">
            <Field label={t.base_domain} hint={t.base_domain_hint} htmlFor="set-domain"
              error={domainMissing ? t.base_domain_required : ""}>
              <input id="set-domain" className="input mono" value={domain} placeholder={defaultDomain}
                onChange={e => setDomain(e.target.value)} />
            </Field>
            {!domain.trim() && defaultDomain && (
              <p className="hint" style={{ marginTop: -8, marginBottom: 14 }}>
                {fmtS(t.base_domain_default, defaultDomain)}
                <br />
                <code style={{ fontSize: 11.5 }}>{`meu-cluster-traces.${defaultDomain}`}</code>
              </p>
            )}
            <Field label={t.ingress_class} error={classErr} htmlFor="set-ingress-class">
              <select id="set-ingress-class" className="select mono" data-testid="ingress-class" value={ingress} onChange={e => setIngress(e.target.value)}>
                {classOptions.map(c => <option key={c} value={c}>{c}</option>)}
              </select>
            </Field>

            <div className="hr" style={{ margin: "18px 0" }} />
            <h3 className="section-title">{t.set_h_tls}</h3>
            <p className="hint" style={{ marginBottom: 12 }}>{t.set_d_tls}</p>
            <Field label={t.tls_secret} hint={t.tls_secret_hint} htmlFor="set-tls-secret">
              <input id="set-tls-secret" className="input mono" value={tlsSecret} placeholder="veloxsearch-dashboards-tls"
                onChange={e => setTlsSecret(e.target.value)} />
            </Field>
            <Field label={t.tls_cert} hint={t.tls_pem_hint} error={certErr} htmlFor="set-tls-cert">
              <textarea id="set-tls-cert" className={`textarea mono${certErr ? " invalid" : ""}`} rows={4} value={tlsCert} spellCheck={false}
                placeholder="-----BEGIN CERTIFICATE-----"
                onChange={e => setTlsCert(e.target.value)} />
            </Field>
            <Field label={t.tls_key} error={keyErr || pairErr} htmlFor="set-tls-key">
              <textarea id="set-tls-key" className={`textarea mono${keyErr ? " invalid" : ""}`} rows={4} value={tlsKey} spellCheck={false}
                placeholder="-----BEGIN PRIVATE KEY-----"
                onChange={e => setTlsKey(e.target.value)} />
            </Field>
          </div>
        )}
      </FormSection>

      {!isTenant && (
        <FormSection title={t.set_h_ret} desc={t.ret_defaults_lead} testid="set-retention">
          <RetentionBlock t={t} onToast={onToast} />
        </FormSection>
      )}

      {!isTenant && (
        <FormSection title={t.set_h_op} desc={t.set_d_op} testid="set-operator">
          <OperatorBlock t={t} onToast={onToast} />
        </FormSection>
      )}

      {buildInfo && (
        <FormSection title={t.set_h_about} desc={t.about_lead} testid="set-about">
          <AboutBlock info={buildInfo} t={t} onToast={onToast} />
        </FormSection>
      )}
    </div>
  );
}

export { SettingsView };
