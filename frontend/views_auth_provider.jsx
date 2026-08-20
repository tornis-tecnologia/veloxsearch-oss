// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
/* ============================================================
   Authentication tab — external identity providers per deployment
   (ADR-045, issue #56).

   Governs who signs in to THIS deployment's OpenSearch Dashboards and API.
   It is not the VeloxSearch panel login.

   Shape of the screen, in the order the ADR specifies:
     1. pick the kind (radio cards, blocked kinds say why on the card)
     2. pick a provider preset, which prefills the URL/filter templates
     3. fill the few required fields; the rest sits in "Advanced options"
     4. map directory groups / claims onto OpenSearch roles
     5. test the connection — result renders inline, never as a toast
     6. confirm the blast radius, then save

   Credentials are write-only: the API returns them as a sentinel, and sending
   the sentinel back means "unchanged". Nothing here ever displays a stored
   secret.
   ============================================================ */
import { useState, useEffect } from "react";
import { STR } from "./i18n.jsx";
import { API } from "./api.jsx";
import { Icon, Btn, Field, Confirm } from "./ui.jsx";

// Which fields belong to each kind, so the form and the payload stay in sync
// with the backend's per-kind config structs.
const KINDS = ["internal", "ldap", "oidc", "saml", "jwt", "proxy"];
const ADVANCED_KINDS = ["jwt", "proxy"];

const ICONS = {
  internal: "shield", ldap: "db", oidc: "spark", saml: "layers",
  jwt: "bolt", proxy: "plug",
};

// Presets prefill templates rather than hiding them: every value stays
// editable, the placeholders are obvious, and "Other" starts blank.
const PRESETS = {
  oidc: [
    { id: "keycloak", label: "Keycloak", connect_url: "https://KEYCLOAK-HOST/realms/REALM/.well-known/openid-configuration", roles_key: "roles" },
    { id: "entra", label: "Entra ID", connect_url: "https://login.microsoftonline.com/TENANT-ID/v2.0/.well-known/openid-configuration", subject_key: "preferred_username", roles_key: "roles" },
    { id: "okta", label: "Okta", connect_url: "https://OKTA-DOMAIN/oauth2/default/.well-known/openid-configuration", roles_key: "groups" },
    { id: "google", label: "Google", connect_url: "https://accounts.google.com/.well-known/openid-configuration", subject_key: "email" },
  ],
  saml: [
    { id: "keycloak", label: "Keycloak", metadata_url: "https://KEYCLOAK-HOST/realms/REALM/protocol/saml/descriptor" },
    { id: "adfs", label: "ADFS", metadata_url: "https://ADFS-HOST/FederationMetadata/2007-06/FederationMetadata.xml" },
    { id: "okta", label: "Okta", metadata_url: "https://OKTA-DOMAIN/app/APP-ID/sso/saml/metadata" },
  ],
  ldap: [
    {
      id: "ad", label: "Active Directory",
      usersearch: "(sAMAccountName={0})", userrolename: "memberOf",
      rolesearch: "(member={0})", rolename: "cn", enable_ssl: true,
    },
    {
      id: "openldap", label: "OpenLDAP",
      usersearch: "(uid={0})", rolesearch: "(member={0})", rolename: "cn",
      enable_ssl: true,
    },
  ],
};

const EMPTY = {
  internal: {},
  ldap: {
    hosts: [], bind_dn: "", bind_password: "", userbase: "", usersearch: "",
    username_attribute: "", rolebase: "", rolesearch: "", userrolename: "",
    rolename: "", resolve_nested_roles: false, enable_ssl: false,
    enable_start_tls: false, pemtrustedcas_content: "",
  },
  oidc: {
    connect_url: "", client_id: "", client_secret: "", subject_key: "",
    roles_key: "", scope: "", logout_url: "", pemtrustedcas_content: "",
  },
  saml: {
    metadata_url: "", idp_entity_id: "", sp_entity_id: "", roles_key: "",
    subject_key: "", exchange_key: "", pemtrustedcas_content: "",
  },
  jwt: { signing_key: "", jwt_header: "", jwt_url_parameter: "", subject_key: "", roles_key: "" },
  proxy: { user_header: "", roles_header: "", roles_separator: "", internal_proxies: "" },
};

// The API returns the spec flat with a `kind` discriminator; the form keeps the
// fields of every kind side by side so switching back and forth doesn't lose
// what was already typed.
function splitSpec(spec) {
  const kind = (spec && spec.kind) || "internal";
  const fields = { ...EMPTY };
  fields[kind] = { ...EMPTY[kind] };
  Object.keys(EMPTY[kind] || {}).forEach((k) => {
    if (spec[k] !== undefined && spec[k] !== null) fields[kind][k] = spec[k];
  });
  return { kind, fields };
}

/* --- A credential the server never sends back in clear. Shows "set" with a
   Replace action instead of a fake dotted value, so nobody believes the dots
   are the password. --- */
function SecretInput({ id, value, sentinel, onChange, testid, t }) {
  const stored = value === sentinel;
  const [show, setShow] = useState(false);
  if (stored) {
    return (
      <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
        <span className="badge green"><span className="dot" />•••••••</span>
        <button className="btn-link" type="button" onClick={() => onChange("")} data-testid={testid && `${testid}-replace`}>
          {t.auth_secret_replace}
        </button>
      </div>
    );
  }
  return (
    <div style={{ position: "relative", display: "flex", gap: 8 }}>
      <input
        id={id}
        className="input"
        type={show ? "text" : "password"}
        value={value}
        autoComplete="new-password"
        onChange={(e) => onChange(e.target.value)}
        data-testid={testid}
      />
      <Btn variant="outline" type="button" onClick={() => setShow((s) => !s)}
        aria-label={show ? "hide" : "show"} icon={show ? "eyeOff" : "eye"} />
    </div>
  );
}

function KindCard({ kind, t, selected, disabled, reason, onSelect }) {
  return (
    <button
      className="purpose"
      type="button"
      aria-pressed={selected}
      disabled={disabled}
      onClick={() => onSelect(kind)}
      data-testid={`auth-kind-${kind}`}
      style={disabled ? { opacity: 0.55, cursor: "not-allowed" } : undefined}
    >
      <span className="check"><Icon name="check" size={12} /></span>
      <div className="ph">
        <span className="picon"><Icon name={ICONS[kind]} size={17} /></span>
        {t[`auth_${kind}_t`]}
      </div>
      <div className="pd">{t[`auth_${kind}_d`]}</div>
      {disabled && reason && (
        <div className="pk">
          <div className="k">{t.auth_blocked_h}</div>
          <div className="v" style={{ fontSize: 12.5, color: "var(--text-2)" }}>{reason}</div>
        </div>
      )}
    </button>
  );
}

function RoleMappings({ maps, roles, t, onChange }) {
  const set = (i, key, v) => onChange(maps.map((m, j) => (j === i ? { ...m, [key]: v } : m)));
  return (
    <div style={{ marginTop: 8 }}>
      <div className="section-title">{t.auth_map_h}</div>
      <p className="hint" style={{ marginTop: -6, marginBottom: 12 }}>{t.auth_map_p}</p>
      {maps.length === 0 && (
        <p className="hint" style={{ marginBottom: 12, color: "var(--warn)" }}>{t.auth_map_empty}</p>
      )}
      {maps.map((m, i) => (
        <div key={i} style={{ display: "grid", gridTemplateColumns: "1fr 200px auto", gap: 10, marginBottom: 10 }}>
          <input
            className="input"
            aria-label={t.auth_map_group}
            placeholder={t.auth_map_group}
            value={m.backend_role}
            onChange={(e) => set(i, "backend_role", e.target.value)}
            data-testid={`auth-map-group-${i}`}
          />
          <input
            className="input"
            aria-label={t.auth_map_role}
            list="velox-os-roles"
            value={m.role}
            onChange={(e) => set(i, "role", e.target.value)}
            data-testid={`auth-map-role-${i}`}
          />
          <Btn variant="outline" icon="trash" type="button"
            aria-label={t.delete}
            onClick={() => onChange(maps.filter((_, j) => j !== i))} />
        </div>
      ))}
      <datalist id="velox-os-roles">
        {roles.map((r) => <option key={r} value={r} />)}
      </datalist>
      <Btn variant="outline" icon="plus" type="button" data-testid="auth-map-add"
        onClick={() => onChange([...maps, { backend_role: "", role: roles[0] || "" }])}>
        {t.auth_map_add}
      </Btn>
    </div>
  );
}

function ProbePanel({ probe, t }) {
  if (!probe) return null;
  const tone = probe.ok ? "var(--accent)" : "var(--danger)";
  return (
    <div className="card pad" data-testid="auth-probe-result"
      style={{ marginTop: 16, borderColor: probe.ok ? "var(--accent-border)" : "var(--danger-border)" }}>
      <div style={{ display: "flex", alignItems: "center", gap: 8, color: tone, fontFamily: "var(--font-mono)", fontSize: 14 }}>
        <Icon name={probe.ok ? "check" : "bolt"} size={15} />
        {probe.ok ? t.auth_test_ok : t.auth_test_fail}
      </div>
      {probe.checks.length > 0 && (
        <ul className="hint" style={{ margin: "10px 0 0", paddingLeft: 18, display: "grid", gap: 5, lineHeight: 1.5 }}>
          {probe.checks.map((c, i) => <li key={i}>{c}</li>)}
        </ul>
      )}
      {probe.error && (
        <pre style={{
          margin: "12px 0 0", whiteSpace: "pre-wrap", wordBreak: "break-word",
          color: "var(--danger)", fontSize: 12.5, fontFamily: "var(--font-mono)",
        }}>{probe.error}</pre>
      )}
    </div>
  );
}

function AuthProviderTab({ d, lang, onToast, locked }) {
  const t = STR[lang];
  const [cfg, setCfg] = useState(null);
  const [kind, setKind] = useState("internal");
  const [fields, setFields] = useState(EMPTY);
  const [maps, setMaps] = useState([]);
  const [preset, setPreset] = useState("");
  const [advanced, setAdvanced] = useState(false);
  const [showOther, setShowOther] = useState(false);
  const [probe, setProbe] = useState(null);
  const [busy, setBusy] = useState(false);
  const [confirm, setConfirm] = useState(false);

  useEffect(() => {
    let alive = true;
    API.authProvider(d.id)
      .then((c) => {
        if (!alive || !c) return;
        setCfg(c);
        const { kind: k, fields: f } = splitSpec(c.spec || {});
        setKind(k);
        setFields(f);
        setMaps((c.spec && c.spec.role_mappings) || []);
        if (ADVANCED_KINDS.includes(k)) setShowOther(true);
      })
      .catch((e) => onToast(e.message));
    return () => { alive = false; };
  }, [d.id]);

  if (!cfg) return <div className="view-enter"><p className="hint">{t.cred_loading}</p></div>;

  const cur = fields[kind] || {};
  const set = (k, v) => {
    setFields((f) => ({ ...f, [kind]: { ...f[kind], [k]: v } }));
    setProbe(null); // an edited form is no longer the thing that was tested
  };
  const available = cfg.kinds_available || [];
  const spec = { kind, ...cur, role_mappings: maps };

  function applyPreset(p) {
    setPreset(p.id);
    setFields((f) => {
      const next = { ...f[kind] };
      Object.entries(p).forEach(([k, v]) => {
        if (k !== "id" && k !== "label") next[k] = v;
      });
      return { ...f, [kind]: next };
    });
    setProbe(null);
  }

  async function runTest() {
    setBusy(true);
    setProbe(null);
    try {
      setProbe(await API.testAuthProvider(d.id, spec));
    } catch (e) {
      setProbe({ ok: false, checks: [], error: e.message });
    } finally {
      setBusy(false);
    }
  }

  async function save() {
    setConfirm(false);
    setBusy(true);
    try {
      await API.saveAuthProvider(d.id, spec);
      onToast(t.auth_saved);
      const c = await API.authProvider(d.id);
      setCfg(c);
      const { fields: f } = splitSpec(c.spec || {});
      setFields((prev) => ({ ...prev, [kind]: f[kind] }));
    } catch (e) {
      onToast(e.message);
    } finally {
      setBusy(false);
    }
  }

  const secret = cfg.secret_kept;
  const txt = (name, label, opts = {}) => (
    <Field label={label} hint={opts.hint} htmlFor={`auth-${name}`} hintId={`auth-${name}-hint`}>
      <input
        id={`auth-${name}`}
        className="input"
        value={cur[name] || ""}
        placeholder={opts.placeholder}
        aria-describedby={opts.hint ? `auth-${name}-hint` : undefined}
        onChange={(e) => set(name, e.target.value)}
        data-testid={`auth-${name}`}
      />
    </Field>
  );
  const check = (name, label) => (
    <label style={{ display: "flex", alignItems: "center", gap: 9, margin: "0 0 12px", fontSize: 13.5 }}>
      <input type="checkbox" checked={!!cur[name]} data-testid={`auth-${name}`}
        onChange={(e) => set(name, e.target.checked)} />
      {label}
    </label>
  );
  const secretField = (name, label, hint) => (
    <Field label={label} hint={hint} htmlFor={`auth-${name}`}>
      <SecretInput id={`auth-${name}`} value={cur[name] || ""} sentinel={secret} t={t}
        testid={`auth-${name}`} onChange={(v) => set(name, v)} />
    </Field>
  );

  return (
    <div className="view-enter" style={{ maxWidth: 720 }}>
      <p className="lead">{t.auth_lead}</p>

      {/* 1. the kind */}
      <div className="section-title">{t.auth_kind_q}</div>
      <div className="purpose-grid" style={{ marginTop: 10 }}>
        {["internal", "ldap", "oidc", "saml"].map((k) => (
          <KindCard key={k} kind={k} t={t}
            selected={kind === k}
            disabled={!available.includes(k)}
            reason={cfg.redirect_blocked_reason}
            onSelect={(x) => { setKind(x); setPreset(""); setProbe(null); }} />
        ))}
      </div>
      <button className="btn-link" type="button" style={{ marginTop: 12 }}
        onClick={() => setShowOther((s) => !s)} data-testid="auth-other-toggle">
        {t.auth_other}
      </button>
      {showOther && (
        <div className="purpose-grid" style={{ marginTop: 10 }}>
          {ADVANCED_KINDS.map((k) => (
            <KindCard key={k} kind={k} t={t} selected={kind === k}
              disabled={!available.includes(k)}
              onSelect={(x) => { setKind(x); setProbe(null); }} />
          ))}
        </div>
      )}

      {/* the reassurance that makes the whole screen safe to touch */}
      <div className="card pad" style={{ marginTop: 18 }} data-testid="auth-breakglass">
        <div style={{ display: "flex", gap: 10 }}>
          <span style={{ color: "var(--accent)", flexShrink: 0, marginTop: 1 }}><Icon name="shield" size={17} /></span>
          <div>
            <div style={{ fontFamily: "var(--font-mono)", fontSize: 14, marginBottom: 4 }}>{t.auth_breakglass_h}</div>
            <p className="hint" style={{ margin: 0 }}>
              {t.auth_breakglass_p.replace("{user}", cfg.break_glass_user)}
            </p>
          </div>
        </div>
      </div>

      {kind !== "internal" && (
        <div className="card pad" style={{ marginTop: 18 }}>
          {/* 2. presets */}
          {PRESETS[kind] && (
            <Field label={t.auth_preset}>
              <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
                {PRESETS[kind].map((p) => (
                  <Btn key={p.id} type="button"
                    variant={preset === p.id ? "primary" : "outline"}
                    data-testid={`auth-preset-${p.id}`}
                    onClick={() => applyPreset(p)}>{p.label}</Btn>
                ))}
                <Btn variant={preset === "other" ? "primary" : "outline"} type="button"
                  onClick={() => { setPreset("other"); setFields((f) => ({ ...f, [kind]: { ...EMPTY[kind] } })); }}>
                  {t.auth_preset_other}
                </Btn>
              </div>
            </Field>
          )}

          {/* 3. the few fields that matter */}
          {kind === "ldap" && (
            <>
              <Field label={t.auth_f_hosts} hint={t.auth_f_hosts_hint} htmlFor="auth-hosts" hintId="auth-hosts-hint">
                <textarea id="auth-hosts" className="textarea" rows={2} data-testid="auth-hosts"
                  aria-describedby="auth-hosts-hint"
                  value={(cur.hosts || []).join("\n")}
                  onChange={(e) => set("hosts", e.target.value.split("\n").map((s) => s.trim()).filter(Boolean))} />
              </Field>
              {txt("userbase", t.auth_f_userbase)}
              {txt("usersearch", t.auth_f_usersearch, { hint: t.auth_f_usersearch_hint })}
              {txt("bind_dn", t.auth_f_bind_dn, { hint: t.auth_f_bind_dn_hint })}
              {secretField("bind_password", t.auth_f_bind_pw)}
              {check("enable_ssl", t.auth_f_ssl)}
            </>
          )}

          {kind === "oidc" && (
            <>
              {txt("connect_url", t.auth_f_connect_url, { hint: t.auth_f_connect_url_hint })}
              {txt("client_id", t.auth_f_client_id)}
              {secretField("client_secret", t.auth_f_client_secret)}
              {cfg.public_url && (
                <Field label={t.auth_redirect_url} hint={t.auth_redirect_url_hint}>
                  <input className="input" value={cfg.public_url} readOnly data-testid="auth-redirect-url" />
                </Field>
              )}
            </>
          )}

          {kind === "saml" && (
            <>
              {txt("metadata_url", t.auth_f_metadata)}
              {txt("idp_entity_id", t.auth_f_idp_entity)}
              {secretField("exchange_key", t.auth_f_exchange, t.auth_f_exchange_hint)}
            </>
          )}

          {kind === "jwt" && (
            <>
              {secretField("signing_key", t.auth_f_signing_key)}
              {txt("jwt_header", t.auth_f_jwt_header)}
            </>
          )}

          {kind === "proxy" && (
            <>
              {txt("user_header", t.auth_f_user_header)}
              {txt("roles_header", t.auth_f_roles_header)}
              {txt("internal_proxies", t.auth_f_proxies, { hint: t.auth_f_proxies_hint })}
            </>
          )}

          {/* everything else stays out of the way until asked for */}
          {kind !== "proxy" && (
            <>
              <button className="btn-link" type="button" style={{ margin: "4px 0 10px" }}
                data-testid="auth-advanced-toggle" onClick={() => setAdvanced((a) => !a)}>
                {t.auth_advanced}
              </button>
              {advanced && (
                <div className="view-enter">
                  {kind === "ldap" && (
                    <>
                      {txt("rolebase", t.auth_f_rolebase)}
                      {txt("rolesearch", t.auth_f_rolesearch)}
                      {txt("userrolename", "userrolename")}
                      {txt("rolename", "rolename")}
                      {check("resolve_nested_roles", t.auth_f_nested)}
                      {check("enable_start_tls", t.auth_f_starttls)}
                      <Field label={t.auth_f_ca} hint={t.auth_f_ca_hint} htmlFor="auth-ca">
                        <textarea id="auth-ca" className="textarea" rows={3}
                          value={cur.pemtrustedcas_content || ""}
                          onChange={(e) => set("pemtrustedcas_content", e.target.value)} />
                      </Field>
                    </>
                  )}
                  {kind === "oidc" && (
                    <>
                      {txt("subject_key", t.auth_f_subject_key, { placeholder: "preferred_username" })}
                      {txt("roles_key", t.auth_f_roles_key, { placeholder: "roles" })}
                      {txt("scope", t.auth_f_scope)}
                      {txt("logout_url", t.auth_f_logout)}
                      <Field label={t.auth_f_ca} hint={t.auth_f_ca_hint} htmlFor="auth-ca">
                        <textarea id="auth-ca" className="textarea" rows={3}
                          value={cur.pemtrustedcas_content || ""}
                          onChange={(e) => set("pemtrustedcas_content", e.target.value)} />
                      </Field>
                    </>
                  )}
                  {kind === "saml" && (
                    <>
                      {txt("sp_entity_id", t.auth_f_sp_entity, { placeholder: "opensearch-dashboards" })}
                      {txt("roles_key", t.auth_f_roles_key, { placeholder: "roles" })}
                      {txt("subject_key", t.auth_f_subject_key)}
                      {/* An ADFS behind the customer's own PKI is the common
                          case, and without this the cluster cannot fetch the
                          IdP metadata at all. */}
                      <Field label={t.auth_f_ca} hint={t.auth_f_ca_hint} htmlFor="auth-ca">
                        <textarea id="auth-ca" className="textarea" rows={3}
                          value={cur.pemtrustedcas_content || ""}
                          onChange={(e) => set("pemtrustedcas_content", e.target.value)} />
                      </Field>
                    </>
                  )}
                  {kind === "jwt" && (
                    <>
                      {txt("subject_key", t.auth_f_subject_key, { placeholder: "sub" })}
                      {txt("roles_key", t.auth_f_roles_key, { placeholder: "roles" })}
                    </>
                  )}
                </div>
              )}
            </>
          )}

          <div className="hr" />
          {/* 4. who gets which role */}
          <RoleMappings maps={maps} roles={cfg.builtin_roles || []} t={t}
            onChange={(m) => { setMaps(m); setProbe(null); }} />
        </div>
      )}

      {/* 5 + 6 */}
      <div style={{ display: "flex", gap: 10, marginTop: 18, flexWrap: "wrap" }}>
        {kind !== "internal" && (
          <Btn variant="outline" icon="plug" disabled={busy} onClick={runTest} data-testid="auth-test">
            {busy ? t.auth_testing : t.auth_test}
          </Btn>
        )}
        <Btn variant="primary" icon="check" disabled={busy || !!locked} data-testid="auth-save"
          onClick={() => setConfirm(true)}>
          {kind === "internal" ? t.auth_revert : t.auth_save}
        </Btn>
      </div>

      <ProbePanel probe={probe} t={t} />

      <Confirm open={confirm} title={t.auth_confirm_h} body={t.auth_confirm_p}
        icon="shield" variant="primary"
        confirmLabel={t.auth_confirm_yes} cancelLabel={t.cancel}
        confirmTestid="auth-confirm" cancelTestid="auth-confirm-cancel"
        onCancel={() => setConfirm(false)} onConfirm={save} />
    </div>
  );
}

export { AuthProviderTab, splitSpec, PRESETS };
