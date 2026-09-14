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
import { Field, Btn, Icon, Copyable } from "./ui.jsx";

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
  const copied = () => onToast(t.copied);
  const unavailable = (note) => (
    <span style={{ color: "var(--text-3)" }} title={note || undefined}>{t.about_unavailable}</span>
  );
  const rows = [
    [t.about_version, <span data-testid="about-version">{info.version}</span>],
    [t.about_commit, info.commit === "unknown"
      ? <span data-testid="about-commit" style={{ color: "var(--text-3)" }}>{t.about_commit_unknown}</span>
      : <span data-testid="about-commit"><Copyable text={info.commit} display={info.commit.slice(0, 7)} onCopy={copied} /></span>],
    [t.about_image, info.image_digest === UNAVAILABLE
      ? unavailable(info.image_note)
      : <Copyable text={info.image_digest} display={shortDigest(info.image_digest)} onCopy={copied} />],
    [t.about_operator, info.operator_image === UNAVAILABLE
      ? unavailable(info.operator_note)
      : <span title={info.operator_deployment}>{info.operator_image}</span>],
    [t.about_catalog, <span style={{ wordBreak: "break-all" }}>{info.catalog_source}</span>],
  ];
  const notes = [info.image_note, info.operator_note].filter(Boolean);
  return (
    <div className="card pad" style={{ maxWidth: 560, marginTop: 18 }} data-testid="about-panel">
      <h3 className="section-title" style={{ marginTop: 0 }}>{t.about_h}</h3>
      <p style={{ fontSize: 12.5, color: "var(--text-3)" }}>{t.about_lead}</p>
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

function SettingsView({ lang, onToast, buildInfo }) {
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

  // Offer detected IngressClasses; fall back to the common controllers when
  // the cluster hasn't reported any (keeps the select usable).
  const classOptions = classes.length ? classes : ["traefik", "nginx", "istio"];

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
  const blocked = !!(certErr || keyErr || pairErr) || domainMissing;
  // Switching away from ingress orphans the per-deployment Ingresses.
  const modeChanged = loaded && access !== loaded.mode;

  return (
    <div className="view-enter">
      <h1 className="page-title">{t.settings_h}</h1>
      <p className="lead">{t.settings_lead}</p>

      <div className="card pad" style={{ maxWidth: 560 }}>
        <Field label={t.dash_access}>
          <div style={{ display: "grid", gap: 10 }}>
            {[
              { v: "portforward", title: t.portfwd, desc: t.portfwd_d, icon: "server" },
              { v: "ingress", title: t.ingress, desc: t.ingress_d, icon: "layers" },
            ].map(o => (
              <button key={o.v} className="purpose" aria-pressed={access === o.v}
                onClick={() => setAccess(o.v)}
                style={{ display: "flex", alignItems: "center", gap: 13, padding: 14 }}>
                <span className="check"><Icon name="check" size={12} /></span>
                <span style={{ color: access === o.v ? "var(--accent)" : "var(--text-3)" }}><Icon name={o.icon} size={17} /></span>
                <span style={{ textAlign: "left" }}>
                  <div style={{ fontFamily: "var(--font-mono)", fontWeight: 600 }}>{o.title}</div>
                  <div style={{ fontSize: 12.5, color: "var(--text-3)" }}>{o.desc}</div>
                </span>
              </button>
            ))}
          </div>
        </Field>

        {/* Changing the mode is not a preference toggle: leaving ingress takes
            every deployment's public URL down. Say it where the choice is. */}
        {modeChanged && (
          <p className="hint" style={{ color: "var(--warn)" }}>
            {access === "portforward" ? t.access_leave_ingress : t.access_enter_ingress}
          </p>
        )}

        {access === "ingress" && (
          <div className="view-enter">
            <Field label={t.base_domain} hint={t.base_domain_hint}
              error={domainMissing ? t.base_domain_required : ""}>
              <input className="input" value={domain} placeholder={defaultDomain}
                onChange={e => setDomain(e.target.value)} />
            </Field>
            {/* What the empty field actually produces, spelled out with a real
                example host — "a default will be used" tells nobody anything. */}
            {!domain.trim() && defaultDomain && (
              <p className="hint" style={{ marginTop: -8 }}>
                {fmtS(t.base_domain_default, defaultDomain)}
                <br />
                <code style={{ fontSize: 11.5 }}>{`meu-cluster-traces.${defaultDomain}`}</code>
              </p>
            )}
            <Field label={t.ingress_class}>
              <select className="select" value={ingress} onChange={e => setIngress(e.target.value)}>
                {classOptions.map(c => <option key={c} value={c}>{c}</option>)}
              </select>
            </Field>

            {/* TLS is its own concern — it was mixed into the access fields
                with no separation at all. */}
            <div className="hr" />
            <h3 className="section-title" style={{ marginTop: 0 }}>{t.tls_h}</h3>
            <Field label={t.tls_secret} hint={t.tls_secret_hint}>
              <input className="input" value={tlsSecret} placeholder="veloxsearch-dashboards-tls"
                onChange={e => setTlsSecret(e.target.value)} />
            </Field>
            <Field label={t.tls_cert} hint={t.tls_pem_hint} error={certErr}>
              <textarea className={`input${certErr ? " invalid" : ""}`} rows={4} value={tlsCert} spellCheck={false}
                placeholder="-----BEGIN CERTIFICATE-----"
                onChange={e => setTlsCert(e.target.value)} />
            </Field>
            <Field label={t.tls_key} error={keyErr || pairErr}>
              <textarea className={`input${keyErr ? " invalid" : ""}`} rows={4} value={tlsKey} spellCheck={false}
                placeholder="-----BEGIN PRIVATE KEY-----"
                onChange={e => setTlsKey(e.target.value)} />
            </Field>
          </div>
        )}

        <Btn variant="primary" icon="check" disabled={busy || blocked} onClick={save} style={{ marginTop: 6 }}>{t.save}</Btn>
      </div>

      {buildInfo && <AboutBlock info={buildInfo} t={t} onToast={onToast} />}
    </div>
  );
}

export { SettingsView };
