// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
/* ============================================================
   views_integration_v2.jsx — agent collection from the registry's
   Spec 2.0 packages (ADR-074), Fleet's "Add integration" shape:

     AgentCatalogue   — the package index (`/api/catalog_v2`) with search,
                        category, signal and pre-release filters; a package
                        some policy activates is green and lists them
     AddPackageDialog — where (a policy of this deployment, or a new one),
                        then a form GENERATED from the verified package
                        (`/api/package_v2`): one block per stream with its
                        toggle, the input's fields (general, then advanced
                        collapsed), the parser, the processor chain with
                        "add filter/processor" from the plugin catalogue,
                        and the OpenSearch block (template, pipeline,
                        lifecycle)

   Nothing here is a table of integrations: every widget comes from the
   field's `type`, every label from the package's own i18n, every
   addable plugin from the catalogue the core vendors. The core validates
   again on save (spec §5) — this is the friendly half, not the boundary.

   Secrets: a `password` field never reaches the revision. The form sends
   `{"$secret": id}` in the activation and the typed value in the draft's
   `secrets`; leaving a stored secret empty keeps it.
   ============================================================ */
import { useEffect, useMemo, useState } from "react";
import { STR } from "./i18n.jsx";
import { API } from "./api.jsx";
import { Banner, Btn, Dialog, Field, Icon, Menu } from "./ui.jsx";

const NEW_POLICY = "__new__";
const SIGNALS = ["logs", "metrics", "traces"];

function loc(map, lang, fallback) {
  if (!map) return fallback;
  return map[lang] || map.en || map.pt || Object.values(map)[0] || fallback;
}

function slug(name) {
  return name.trim().toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "").slice(0, 40) || "policy";
}

function monogram(title) {
  const w = String(title || "?").replace(/[^A-Za-z0-9 ]/g, " ").trim().split(/\s+/);
  return ((w[0] || "?")[0] + ((w[1] || w[0] || "")[0] || "")).toUpperCase().slice(0, 2);
}

const DURATION = /^[0-9]+(ms|s|m|h|d)$/;
const SIZE = /^[0-9]+([kKmMgGtT][bB]?|[bB])?$/;

/* ---------- values: package defaults ⇄ form state ⇄ activation ---------- */

// The form keeps what the operator types: strings for text widgets (a
// multi-value field is one value per line), booleans for toggles. Typed
// values are made on save.
function toForm(field, v) {
  if (v == null) return field.type === "bool" ? false : "";
  if (field.type === "bool") return !!v;
  if (field.type === "yaml") return typeof v === "string" ? v : JSON.stringify(v, null, 2);
  if (Array.isArray(v)) return v.map(String).join("\n");
  return String(v);
}

function fieldDefaults(fields, given) {
  const out = {};
  (fields || []).forEach(f => {
    const g = given ? given[f.name] : undefined;
    if (f.type === "password") out[f.name] = "";
    else out[f.name] = toForm(f, g !== undefined ? g : f.default);
  });
  return out;
}

function secretOf(v) {
  return v && typeof v === "object" && !Array.isArray(v) && typeof v.$secret === "string" ? v.$secret : null;
}

function secretId(pkgId, streamId, scope, name) {
  return [pkgId, streamId, scope, name].filter(Boolean).join("-").toLowerCase().replace(/[^a-z0-9_-]+/g, "-").slice(0, 64);
}

// One stream's form state from its package definition and, when editing,
// the stored activation.
function initialStream(s, act) {
  const procs = [];
  const given = act && act.processors;
  if (given && given.length) {
    given.forEach(p => {
      if (p.id) {
        const d = (s.processors.defaults || []).find(x => x.id === p.id);
        if (d) procs.push({ id: d.id, enabled: d.locked ? true : (p.enabled == null ? d.enabled !== false : !!p.enabled), fields: fieldDefaults(d.fields, p.fields) });
      } else if (p.plugin) {
        procs.push({ plugin: p.plugin, params: Object.fromEntries(Object.entries(p.params || {}).map(([k, v]) => [k, Array.isArray(v) ? v.join("\n") : (secretOf(v) ? "" : String(v))])) });
      }
    });
    // Defaults the activation does not name run as the package ships them.
    (s.processors.defaults || []).forEach(d => {
      if (!procs.some(p => p.id === d.id)) procs.push({ id: d.id, enabled: d.locked || d.enabled !== false, fields: fieldDefaults(d.fields) });
    });
  } else {
    (s.processors.defaults || []).forEach(d => procs.push({ id: d.id, enabled: d.locked || d.enabled !== false, fields: fieldDefaults(d.fields) }));
  }
  const storedSecrets = {};
  const collect = (fields, values, scope) => (fields || []).forEach(f => {
    const id = values && secretOf(values[f.name]);
    if (f.type === "password" && id) storedSecrets[`${scope}.${f.name}`] = id;
  });
  collect(s.input.fields, act && act.input, "input");
  collect(s.backend && s.backend.fields, act && act.backend, "backend");
  (s.processors.defaults || []).forEach(d => {
    const p = given && given.find(x => x.id === d.id);
    collect(d.fields, p && p.fields, `proc.${d.id}`);
  });
  return {
    enabled: act ? !!act.enabled : s.enabled !== false,
    input: fieldDefaults(s.input.fields, act && act.input),
    parser: (act && act.parser) || (s.parser_field && s.parser_field.default) || "none",
    procs,
    backend: fieldDefaults(s.backend && s.backend.fields, act && act.backend),
    storedSecrets,
  };
}

function visible(f, values, fields) {
  if (!f.show_if) return true;
  const other = (fields || []).find(x => x.name === f.show_if.field);
  if (!other) return true;
  let v = values[f.show_if.field];
  if (other.type === "integer" || other.type === "number") v = Number(v);
  if (f.show_if.equals !== undefined && f.show_if.equals !== null) return v === f.show_if.equals || String(v) === String(f.show_if.equals);
  return (f.show_if.in || []).some(x => x === v || String(x) === String(v));
}

function optionValues(f, stream) {
  if (f.options) return f.options.map(o => (o && typeof o === "object" && "value" in o ? o : { value: o }));
  if (f.options_from === "parsers") return [{ value: "none" }, ...(stream.parsers || []).map(p => ({ value: p.name, title: p.title }))];
  if (f.options_from === "multiline_parsers") return (stream.multiline_parsers || []).map(p => ({ value: p.name, title: p.title }));
  return [];
}

// Parse one field's form value into the activation value, or an error key.
function parseValue(f, raw, stream, t) {
  const lines = (s) => String(s).split("\n").map(x => x.trim()).filter(Boolean);
  const one = (s) => {
    switch (f.type) {
      case "integer": {
        if (!/^-?\d+$/.test(s)) return { err: t.pkg_err_int };
        const n = parseInt(s, 10);
        if (f.min != null && n < f.min) return { err: t.pkg_err_min.replace("{v}", f.min) };
        if (f.max != null && n > f.max) return { err: t.pkg_err_max.replace("{v}", f.max) };
        return { v: n };
      }
      case "number": {
        const n = Number(s);
        if (s === "" || Number.isNaN(n)) return { err: t.pkg_err_num };
        if (f.min != null && n < f.min) return { err: t.pkg_err_min.replace("{v}", f.min) };
        if (f.max != null && n > f.max) return { err: t.pkg_err_max.replace("{v}", f.max) };
        return { v: n };
      }
      case "duration": return DURATION.test(s) ? { v: s } : { err: t.pkg_err_duration };
      case "size": return SIZE.test(s) ? { v: s } : { err: t.pkg_err_size };
      case "regex": try { new RegExp(s); return { v: s }; } catch { return { err: t.pkg_err_regex }; }
      case "select": {
        const o = optionValues(f, stream).find(x => String(x.value) === s);
        return o ? { v: o.value } : { err: t.pkg_err_pattern };
      }
      case "string":
        if (f.pattern) { try { if (!new RegExp(f.pattern).test(s)) return { err: t.pkg_err_pattern }; } catch { /* the core judges */ } }
        return { v: s };
      default: return { v: s };
    }
  };
  if (f.type === "bool") return { v: !!raw };
  if (f.type === "text") return raw === "" ? { empty: true } : { v: String(raw) };
  if (f.type === "yaml") {
    if (String(raw).trim() === "") return { empty: true };
    try { const o = JSON.parse(raw); return o && typeof o === "object" && !Array.isArray(o) ? { v: o } : { err: t.pkg_err_json }; } catch { return { err: t.pkg_err_json }; }
  }
  if (f.multi) {
    const items = lines(raw);
    if (items.length === 0) return { empty: true };
    const out = [];
    for (const it of items) { const r = one(it); if (r.err) return r; out.push(r.v); }
    return { v: out };
  }
  const s = String(raw).trim();
  if (s === "") return { empty: true };
  return one(s);
}

/* ---------- one field ---------- */

function FieldInput({ f, value, onChange, stream, lang, t, error, idBase, stored }) {
  const id = `${idBase}-${f.name}`;
  const label = (
    <>
      {loc(f.title, lang, f.name)}
      {f.required && <span className="hint"> · {t.pkg_required}</span>}
    </>
  );
  let control;
  switch (f.type) {
    case "bool":
      control = (
        <label style={{ display: "flex", gap: 8, alignItems: "center" }}>
          <input type="checkbox" id={id} checked={!!value} data-testid={`pkg-field-${idBase}-${f.name}`} onChange={e => onChange(e.target.checked)} />
          <span className="mono hint">{f.name}</span>
        </label>
      );
      break;
    case "select":
      control = (
        <select className="select" id={id} value={value} data-testid={`pkg-field-${idBase}-${f.name}`} onChange={e => onChange(e.target.value)}>
          {optionValues(f, stream).map(o => <option key={String(o.value)} value={String(o.value)}>{o.title ? loc(o.title, lang, String(o.value)) : String(o.value)}</option>)}
        </select>
      );
      break;
    case "password":
      control = (
        <input className="input mono" type="password" id={id} value={value} autoComplete="new-password"
          placeholder={stored ? t.pkg_secret_kept : t.pkg_secret_ph}
          data-testid={`pkg-field-${idBase}-${f.name}`} onChange={e => onChange(e.target.value)} />
      );
      break;
    case "text":
    case "yaml":
      control = (
        <textarea className="input mono" id={id} rows={f.type === "yaml" ? 5 : 3} value={value} placeholder={f.placeholder || (f.type === "yaml" ? "{ }" : "")}
          data-testid={`pkg-field-${idBase}-${f.name}`} onChange={e => onChange(e.target.value)} />
      );
      break;
    default:
      control = f.multi ? (
        <textarea className="input mono" id={id} rows={Math.min(6, Math.max(2, String(value).split("\n").length))} value={value}
          placeholder={f.placeholder || ""} data-testid={`pkg-field-${idBase}-${f.name}`} onChange={e => onChange(e.target.value)} />
      ) : (
        <input className="input mono" id={id} value={value} placeholder={f.placeholder || (f.default != null ? String(f.default) : "")}
          inputMode={f.type === "integer" || f.type === "number" ? "numeric" : undefined}
          data-testid={`pkg-field-${idBase}-${f.name}`} onChange={e => onChange(e.target.value)} />
      );
  }
  const hint = [loc(f.description, lang, ""), f.multi && f.type !== "yaml" && f.type !== "text" ? t.pkg_multi_hint : ""].filter(Boolean).join(" · ");
  return <Field label={label} hint={hint || undefined} error={error} htmlFor={id} style={{ marginBottom: 10 }}>{control}</Field>;
}

// General fields, then the advanced ones collapsed — Fleet's "Advanced options".
function FieldGroup({ fields, values, set, stream, lang, t, errors, idBase, storedSecrets, scope, fbFloor }) {
  const usable = (fields || []).filter(f => !(fbFloor && f.since && cmpVer(fbFloor, f.since) < 0)).filter(f => visible(f, values, fields));
  const general = usable.filter(f => f.group !== "advanced");
  const advanced = usable.filter(f => f.group === "advanced");
  const render = (f) => (
    <FieldInput key={f.name} f={f} value={values[f.name]} onChange={v => set(f.name, v)} stream={stream} lang={lang} t={t}
      error={errors[`${idBase}.${f.name}`]} idBase={idBase} stored={!!storedSecrets[`${scope}.${f.name}`]} />
  );
  return (
    <>
      {general.map(render)}
      {advanced.length > 0 && (
        <details style={{ marginBottom: 8 }} data-testid={`pkg-advanced-${idBase}`}>
          <summary className="hint" style={{ cursor: "pointer", marginBottom: 8 }}>{t.pkg_advanced}</summary>
          {advanced.map(render)}
        </details>
      )}
    </>
  );
}

function cmpVer(a, b) {
  const p = (s) => String(s).split(/[-+]/)[0].split(".").map(n => parseInt(n, 10) || 0);
  const x = p(a), y = p(b);
  for (let i = 0; i < 3; i++) if ((x[i] || 0) !== (y[i] || 0)) return (x[i] || 0) - (y[i] || 0);
  return 0;
}

// An operator-added processor: the catalogue's params as fields.
function pluginFields(plugin) {
  return (plugin.params || [])
    .filter(p => p.operator_settable !== false && !p.key.includes("*"))
    .map(p => ({
      name: p.key, type: p.type === "password" || p.secret ? "password" : p.type, title: { en: p.key },
      description: p.description, required: !!p.required, multi: p.multi === "join" || p.multi === "repeat",
      default: undefined, group: p.advanced ? "advanced" : "general", options: p.options, since: p.since,
      bind: { key: p.key },
    }));
}

/* ---------- the dialog ---------- */

function AddPackageDialog({ open, onClose, lang, d, item, policies, agentsByPolicy, fixedPolicy, fbFloor, onDone, onToast }) {
  const t = STR[lang] || STR.pt;
  const [pkg, setPkg] = useState(null);
  const [pkgErr, setPkgErr] = useState(null);
  const [step, setStep] = useState(fixedPolicy ? 2 : 1);
  const [target, setTarget] = useState(fixedPolicy || "");
  const [newName, setNewName] = useState("");
  const [settings, setSettings] = useState(null);
  const [newOs, setNewOs] = useState("");
  const [newMesh, setNewMesh] = useState("");
  const [form, setForm] = useState({});
  const [errors, setErrors] = useState({});
  const [busy, setBusy] = useState(false);
  const [saveErr, setSaveErr] = useState(null);
  const [adding, setAdding] = useState({});

  const editing = !!fixedPolicy;

  useEffect(() => {
    if (!open || !item) return undefined;
    let alive = true;
    setPkg(null); setPkgErr(null); setSaveErr(null); setErrors({}); setAdding({});
    setStep(fixedPolicy ? 2 : 1);
    setTarget(fixedPolicy || ((policies || [])[0] || {}).id || NEW_POLICY);
    (async () => {
      try {
        // Editing reads the stored activation first: its version decides
        // which package the form describes.
        let act = null;
        if (fixedPolicy) {
          const doc = await API.meshPolicy(d.id, fixedPolicy);
          act = (((doc || {}).body || {}).packages || []).find(a => a.package_id === item.id) || null;
        }
        const p = await API.packageV2(item.id, act ? act.package_version : (item.offered || null));
        if (!alive) return;
        setPkg(p);
        setForm(Object.fromEntries(p.streams.map(s => [s.id, initialStream(s, act && act.streams ? act.streams[s.id] : (act ? { enabled: false } : null))])));
      } catch (e) { if (alive) setPkgErr(e.message); }
    })();
    API.meshSettings(d.id)
      .then(s => {
        if (!alive) return;
        setSettings(s || {});
        const os = (s && s.os_endpoints) || [];
        const mesh = (s && s.mesh_endpoints) || [];
        setNewOs(os.length === 1 ? os[0].id : "");
        setNewMesh(mesh.length === 1 ? mesh[0].id : "");
      })
      .catch(() => { if (alive) setSettings({}); });
    return () => { alive = false; };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, item && item.id, fixedPolicy]);

  const osEps = (settings && settings.os_endpoints) || [];
  const meshEps = (settings && settings.mesh_endpoints) || [];
  const canNew = !!settings && osEps.length > 0 && meshEps.length > 0;
  const whereReady = target && (target !== NEW_POLICY || (!!newName.trim() && !!newOs && !!newMesh));
  const policyName = target === NEW_POLICY ? slug(newName) : target;
  const streams = useMemo(() => (pkg ? pkg.streams.filter(s => !(fbFloor && s.since && cmpVer(fbFloor, s.since) < 0)) : []), [pkg, fbFloor]);

  if (!open || !item) return null;
  const name = loc(item.title, lang, item.id);
  const title = editing ? t.pkg_dlg_edit.replace("{name}", name).replace("{policy}", fixedPolicy) : t.pkg_dlg_title.replace("{name}", name);

  // Any edit clears the previous save attempt's errors: they describe values
  // that may no longer be there, and the next save re-checks everything.
  const setStream = (sid, fn) => { setErrors({}); setForm(f => ({ ...f, [sid]: fn(f[sid]) })); };

  // Build the activation and the secrets; collect errors keyed like the inputs.
  function build() {
    const errs = {};
    const secrets = {};
    const out = {};
    const take = (fields, values, s, scope, idBase, st) => {
      const vals = {};
      (fields || []).forEach(f => {
        if (fbFloor && f.since && cmpVer(fbFloor, f.since) < 0) return;
        const raw = values[f.name];
        if (f.type === "password") {
          const id = st.storedSecrets[`${scope}.${f.name}`] || secretId(item.id, s.id, scope === "input" || scope === "backend" ? "" : scope.replace("proc.", ""), f.name);
          if (raw) { secrets[id] = raw; vals[f.name] = { $secret: id }; }
          else if (st.storedSecrets[`${scope}.${f.name}`]) vals[f.name] = { $secret: id };
          else if (f.required && visible(f, values, fields)) errs[`${idBase}.${f.name}`] = t.pkg_err_required;
          return;
        }
        const r = parseValue(f, raw, s, t);
        if (r.err) { if (visible(f, values, fields)) errs[`${idBase}.${f.name}`] = r.err; return; }
        if (r.empty) { if (f.required && visible(f, values, fields)) errs[`${idBase}.${f.name}`] = t.pkg_err_required; return; }
        vals[f.name] = r.v;
      });
      return vals;
    };
    let any = false;
    streams.forEach(s => {
      const st = form[s.id];
      if (!st) return;
      if (!st.enabled) { out[s.id] = { enabled: false }; return; }
      any = true;
      const sa = { enabled: true, input: take(s.input.fields, st.input, s, "input", `${s.id}.input`, st) };
      if ((s.parsers || []).length) sa.parser = st.parser || "none";
      sa.processors = st.procs.map((p, i) => {
        if (p.id) {
          const def = s.processors.defaults.find(x => x.id === p.id);
          return { id: p.id, enabled: !!p.enabled, fields: p.enabled ? take(def.fields, p.fields, s, `proc.${p.id}`, `${s.id}.proc.${p.id}`, st) : {} };
        }
        const plugin = pkg.plugins[p.plugin];
        const fields = plugin ? pluginFields(plugin) : [];
        return { plugin: p.plugin, params: take(fields, p.params, s, `add.${i}.${p.plugin}`, `${s.id}.add.${i}`, st) };
      });
      if (s.backend && (s.backend.fields || []).length) sa.backend = take(s.backend.fields, st.backend, s, "backend", `${s.id}.backend`, st);
      out[s.id] = sa;
    });
    if (!any) errs.__none = t.pkg_pick_one;
    setErrors(errs);
    if (Object.keys(errs).length) return null;
    return { activation: { package_id: pkg.id, package_version: pkg.version, spec_version: "2.0", streams: out }, secrets };
  }

  async function save() {
    const built = build();
    if (!built) return;
    setBusy(true); setSaveErr(null);
    try {
      let pid = target;
      if (target === NEW_POLICY) {
        pid = slug(newName);
        await API.meshCreatePolicy(d.id, pid, newName.trim() || pid, newOs, newMesh);
      }
      const doc = await API.meshPolicy(d.id, pid);
      const body = (doc && doc.body) || {};
      if ((body.activations || []).length) throw new Error(t.pkg_mixed.replace("{policy}", pid));
      const packages = (body.packages || []).filter(a => a.package_id !== item.id).concat([built.activation]);
      const r = await API.meshSavePolicy({
        id: pid, name: body.name || pid, deployment: d.id, integrations: [],
        packages, secrets: built.secrets, endpoints: body.endpoints || {},
      });
      onToast && onToast(t.pkg_done.replace("{name}", name).replace("{policy}", pid).replace("{rev}", r.revision), true);
      onDone && onDone(pid, r);
      onClose && onClose();
    } catch (e) {
      setSaveErr(e.message);
    } finally { setBusy(false); }
  }

  return (
    <Dialog open={open} onClose={busy ? undefined : onClose} title={title} width={680} testid="add-package" busy={busy}>
      {pkgErr && <Banner tone="danger" title={t.pkg_load_err}>{pkgErr}</Banner>}
      {!pkg && !pkgErr && <p className="hint">{t.pkg_loading}</p>}

      {pkg && step === 1 && (
        <div data-testid="add-package-where">
          <p className="hint" style={{ margin: "0 0 12px" }}>{t.pkg_where_p}</p>
          <div className="onb-steps" role="radiogroup">
            {(policies || []).map(p => (
              <label key={p.id} className={`mesh-card${target === p.id ? " on" : ""}`} style={{ display: "block", cursor: "pointer" }} data-testid={`pkg-target-${p.id}`}>
                <input type="radio" name="pkg-target" checked={target === p.id} onChange={() => setTarget(p.id)} style={{ marginRight: 8 }} />
                <b className="mono">{p.id}</b>
                <span className="hint" style={{ marginLeft: 8 }}>{t.pkg_policy_agents.replace("{n}", (agentsByPolicy && agentsByPolicy[p.id]) || 0)}</span>
              </label>
            ))}
            <label className={`mesh-card${target === NEW_POLICY ? " on" : ""}`} style={{ display: "block", cursor: canNew ? "pointer" : "not-allowed", opacity: settings && !canNew ? 0.7 : 1 }} data-testid="pkg-target-new">
              <input type="radio" name="pkg-target" checked={target === NEW_POLICY} disabled={!canNew} onChange={() => setTarget(NEW_POLICY)} style={{ marginRight: 8 }} />
              <b>{t.pkg_new_policy}</b>
              {settings && !canNew && <div className="hint" style={{ marginLeft: 22 }}>{t.pkg_need_endpoints}</div>}
              {target === NEW_POLICY && (
                <div style={{ display: "grid", gap: 8, marginTop: 8, maxWidth: 380 }}>
                  <input className="input" value={newName} onChange={e => setNewName(e.target.value)} placeholder={t.pkg_new_policy_ph}
                    aria-label={t.pkg_new_policy_ph} data-testid="pkg-new-policy-name" />
                  {osEps.length > 1 && (
                    <select className="select" value={newOs} onChange={e => setNewOs(e.target.value)} aria-label={t.mesh_pol_os_endpoint}>
                      <option value="" disabled>{t.mesh_pol_os_endpoint}</option>
                      {osEps.map(e => <option key={e.id} value={e.id}>{e.name} · {e.host}</option>)}
                    </select>
                  )}
                  {meshEps.length > 1 && (
                    <select className="select" value={newMesh} onChange={e => setNewMesh(e.target.value)} aria-label={t.mesh_pol_mesh_endpoint}>
                      <option value="" disabled>{t.mesh_pol_mesh_endpoint}</option>
                      {meshEps.map(e => <option key={e.id} value={e.id}>{e.name} · {e.url}</option>)}
                    </select>
                  )}
                </div>
              )}
            </label>
          </div>
          <div className="dialog-actions">
            <Btn variant="ghost" onClick={onClose}>{t.cancel}</Btn>
            <Btn variant="primary" disabled={!whereReady} data-testid="add-package-next" onClick={() => setStep(2)}>{t.pkg_next}</Btn>
          </div>
        </div>
      )}

      {pkg && step === 2 && (
        <div data-testid="add-package-configure">
          <p className="hint" style={{ margin: "0 0 12px" }}>
            <b className="mono">{policyName}</b> · <span className="mono">{pkg.id}@{pkg.version}</span>
            {pkg.release !== "ga" && <span className="badge yellow" style={{ marginLeft: 8 }}>{t[`pkg_release_${pkg.release}`] || pkg.release}</span>}
          </p>
          {errors.__none && <div className="field-err" role="alert">{errors.__none}</div>}
          {streams.map(s => {
            const st = form[s.id];
            if (!st) return null;
            const idBase = s.id;
            return (
              <div key={s.id} className={`mesh-card${st.enabled ? " on" : ""}`} style={{ marginBottom: 10, cursor: "default" }} data-testid={`pkg-stream-${s.id}`} data-on={st.enabled ? "1" : "0"}>
                <label style={{ display: "flex", gap: 8, alignItems: "flex-start", cursor: "pointer" }}>
                  <input type="checkbox" checked={st.enabled} style={{ marginTop: 3 }} data-testid={`pkg-stream-toggle-${s.id}`}
                    onChange={e => setStream(s.id, x => ({ ...x, enabled: e.target.checked }))} />
                  <span style={{ minWidth: 0 }}>
                    <b>{loc(s.title, lang, s.id)}</b>
                    <span className="badge mono" style={{ marginLeft: 8 }}>{t[`pkg_signal_${s.signal}`] || s.signal}</span>
                    {loc(s.description, lang, "") && <div className="hint">{loc(s.description, lang, "")}</div>}
                    <div className="hint mono" style={{ fontSize: 11.5 }}>
                      {s.artifact ? `${t.pkg_artifact}: ${s.artifact}` : t.pkg_metrics_dest}
                    </div>
                  </span>
                </label>
                {st.enabled && (
                  <div style={{ marginTop: 10, paddingLeft: 24 }}>
                    <FieldGroup fields={s.input.fields} values={st.input} stream={s} lang={lang} t={t} errors={errors}
                      idBase={`${idBase}.input`} storedSecrets={st.storedSecrets} scope="input" fbFloor={fbFloor}
                      set={(k, v) => setStream(s.id, x => ({ ...x, input: { ...x.input, [k]: v } }))} />
                    {(s.parsers || []).length > 0 && (
                      <Field label={s.parser_field && s.parser_field.title ? loc(s.parser_field.title, lang, t.pkg_parser) : t.pkg_parser} htmlFor={`${idBase}-parser`} style={{ marginBottom: 10 }}>
                        <select className="select" id={`${idBase}-parser`} value={st.parser} data-testid={`pkg-parser-${s.id}`}
                          onChange={e => setStream(s.id, x => ({ ...x, parser: e.target.value }))}>
                          <option value="none">{t.pkg_parser_none}</option>
                          {s.parsers.map(p => <option key={p.name} value={p.name}>{p.title ? loc(p.title, lang, p.name) : p.name}</option>)}
                        </select>
                      </Field>
                    )}
                    {(st.procs.length > 0 || (s.processors.allow || []).length > 0) && (
                      <div style={{ margin: "6px 0 10px" }}>
                        <div className="section-title" style={{ margin: "0 0 6px" }}>{t.pkg_processors}</div>
                        {st.procs.map((p, i) => {
                          if (p.id) {
                            const def = s.processors.defaults.find(x => x.id === p.id);
                            if (!def) return null;
                            return (
                              <div key={p.id} style={{ marginBottom: 8 }} data-testid={`pkg-proc-${s.id}-${p.id}`}>
                                <label style={{ display: "flex", gap: 8, alignItems: "center" }}>
                                  <input type="checkbox" checked={!!p.enabled} disabled={def.locked}
                                    onChange={e => setStream(s.id, x => ({ ...x, procs: x.procs.map((q, j) => j === i ? { ...q, enabled: e.target.checked } : q) }))} />
                                  <span>{loc(def.title, lang, def.id)} <span className="hint mono">{def.plugin}</span>{def.locked && <span className="hint"> · {t.pkg_locked}</span>}</span>
                                </label>
                                {def.description && <div className="hint" style={{ marginLeft: 24 }}>{loc(def.description, lang, "")}</div>}
                                {p.enabled && (def.fields || []).length > 0 && (
                                  <div style={{ paddingLeft: 24, marginTop: 6 }}>
                                    <FieldGroup fields={def.fields} values={p.fields} stream={s} lang={lang} t={t} errors={errors}
                                      idBase={`${idBase}.proc.${p.id}`} storedSecrets={st.storedSecrets} scope={`proc.${p.id}`} fbFloor={fbFloor}
                                      set={(k, v) => setStream(s.id, x => ({ ...x, procs: x.procs.map((q, j) => j === i ? { ...q, fields: { ...q.fields, [k]: v } } : q) }))} />
                                  </div>
                                )}
                              </div>
                            );
                          }
                          const plugin = pkg.plugins[p.plugin];
                          const fields = plugin ? pluginFields(plugin) : [];
                          return (
                            <div key={`add-${i}`} className="card" style={{ padding: 10, marginBottom: 8 }} data-testid={`pkg-added-${s.id}-${i}`}>
                              <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: 6 }}>
                                <b>{plugin ? loc(plugin.title, lang, p.plugin) : p.plugin} <span className="hint mono">{p.plugin}</span></b>
                                <Btn variant="ghost" size="sm" icon="trash" onClick={() => setStream(s.id, x => ({ ...x, procs: x.procs.filter((_, j) => j !== i) }))}>{t.pkg_remove_processor}</Btn>
                              </div>
                              <FieldGroup fields={fields} values={p.params} stream={s} lang={lang} t={t} errors={errors}
                                idBase={`${idBase}.add.${i}`} storedSecrets={st.storedSecrets} scope={`add.${i}.${p.plugin}`} fbFloor={fbFloor}
                                set={(k, v) => setStream(s.id, x => ({ ...x, procs: x.procs.map((q, j) => j === i ? { ...q, params: { ...q.params, [k]: v } } : q) }))} />
                            </div>
                          );
                        })}
                        {(s.processors.allow || []).length > 0 && (
                          <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
                            <select className="select" value={adding[s.id] || ""} aria-label={t.pkg_pick_plugin} data-testid={`pkg-add-proc-pick-${s.id}`}
                              onChange={e => setAdding(a => ({ ...a, [s.id]: e.target.value }))} style={{ maxWidth: 260 }}>
                              <option value="">{t.pkg_pick_plugin}</option>
                              {s.processors.allow.filter(n => pkg.plugins[n]).map(n => <option key={n} value={n}>{loc(pkg.plugins[n].title, lang, n)} ({n})</option>)}
                            </select>
                            <Btn variant="outline" size="sm" icon="plus" disabled={!adding[s.id]} data-testid={`pkg-add-proc-${s.id}`}
                              onClick={() => {
                                const n = adding[s.id];
                                setStream(s.id, x => ({ ...x, procs: [...x.procs, { plugin: n, params: fieldDefaults(pluginFields(pkg.plugins[n])) }] }));
                                setAdding(a => ({ ...a, [s.id]: "" }));
                              }}>{t.pkg_add_processor}</Btn>
                          </div>
                        )}
                      </div>
                    )}
                    {s.backend && (s.backend.fields || []).length > 0 && (
                      <div style={{ marginTop: 6 }}>
                        <div className="section-title" style={{ margin: "0 0 6px" }}>{t.pkg_backend}</div>
                        <FieldGroup fields={s.backend.fields} values={st.backend} stream={s} lang={lang} t={t} errors={errors}
                          idBase={`${idBase}.backend`} storedSecrets={st.storedSecrets} scope="backend" fbFloor={fbFloor}
                          set={(k, v) => setStream(s.id, x => ({ ...x, backend: { ...x.backend, [k]: v } }))} />
                      </div>
                    )}
                  </div>
                )}
              </div>
            );
          })}
          {(pkg.changelog || []).length > 0 && (
            <details className="hint" style={{ margin: "8px 0" }}>
              <summary style={{ cursor: "pointer" }}>{t.pkg_changelog}</summary>
              <ul style={{ margin: "6px 0 0 16px" }}>
                {(pkg.changelog[0].changes || []).map((c, i) => <li key={i}>{loc(c.description, lang, "")}</li>)}
              </ul>
            </details>
          )}
          {saveErr && <Banner tone="danger" title={t.pkg_err} style={{ marginTop: 10 }}>{saveErr}</Banner>}
          {target !== NEW_POLICY && agentsByPolicy && !(agentsByPolicy[target] > 0) && <p className="hint" style={{ marginTop: 6 }}>{t.pkg_no_agents}</p>}
          <div className="dialog-actions">
            {!editing && <Btn variant="ghost" onClick={() => setStep(1)} disabled={busy}>{t.pkg_back}</Btn>}
            <Btn variant="ghost" onClick={onClose} disabled={busy}>{t.cancel}</Btn>
            <Btn variant="primary" icon={editing ? "check" : "plus"} disabled={busy} data-testid="add-package-save" onClick={save}>
              {busy ? t.pkg_saving : (editing ? t.pkg_save_edit : t.pkg_save)}
            </Btn>
          </div>
        </div>
      )}
      {busy && <div className="hint" style={{ marginTop: 8 }}><Icon name="clock" size={12} /> {t.pkg_saving}</div>}
    </Dialog>
  );
}

/* ---------- the catalogue ---------- */

function AgentCatalogue({ d, lang, mesh, locked, onToast, onChanged }) {
  const t = STR[lang] || STR.pt;
  const [view, setView] = useState(null);
  const [err, setErr] = useState("");
  const [pre, setPre] = useState(false);
  const [q, setQ] = useState("");
  const [cat, setCat] = useState("all");
  const [signal, setSignal] = useState("");
  const [dialog, setDialog] = useState(null);
  const [busy, setBusy] = useState(null);
  const [tick, setTick] = useState(0);

  useEffect(() => {
    let alive = true;
    API.catalogV2(d.id, pre)
      .then(v => { if (alive) { setView(v); setErr(""); } })
      .catch(e => { if (alive) setErr(e.message); });
    return () => { alive = false; };
  }, [d.id, pre, tick]);

  const policies = (mesh && mesh.policies) || [];
  const agentsByPolicy = {};
  ((mesh && mesh.agents) || []).forEach(a => { if (a.policy_id && a.status !== "unenrolled") agentsByPolicy[a.policy_id] = (agentsByPolicy[a.policy_id] || 0) + 1; });
  // The oldest Fluent Bit among the deployment's agents: fields and streams
  // newer than it are hidden (spec §5). Unknown agents gate nothing.
  const fbFloor = ((mesh && mesh.agents) || [])
    .map(a => (a.attributes || {})["velox.fluent_bit.version"])
    .filter(Boolean)
    .sort(cmpVer)[0] || null;

  if (!mesh) {
    return (
      <div data-testid="agent-catalogue">
        <div className="section-title">{t.pkg_section_h}</div>
        <p className="hint">{t.pkg_no_mesh}</p>
      </div>
    );
  }

  const items = (view && view.packages) || [];
  const cats = Array.from(new Set(items.flatMap(p => p.categories || []))).sort();
  const needle = q.trim().toLowerCase();
  const sigOf = (p) => { const v = (p.versions || []).find(x => x.version === p.offered) || (p.versions || [])[0] || {}; return v.signals || []; };
  const shown = items
    .filter(p => cat === "all" || (cat === "installed" ? p.policies.length > 0 : (p.categories || []).includes(cat)))
    .filter(p => !signal || sigOf(p).includes(signal))
    .filter(p => !needle || p.id.includes(needle) || loc(p.title, lang, "").toLowerCase().includes(needle) || loc(p.summary, lang, "").toLowerCase().includes(needle))
    .sort((a, b) => Number(b.policies.length > 0) - Number(a.policies.length > 0));

  async function removeFrom(p, pid) {
    setBusy(p.id);
    try {
      const doc = await API.meshPolicy(d.id, pid);
      const body = (doc && doc.body) || {};
      const r = await API.meshSavePolicy({
        id: pid, name: body.name || pid, deployment: d.id, integrations: [],
        packages: (body.packages || []).filter(a => a.package_id !== p.id), secrets: {}, endpoints: body.endpoints || {},
      });
      onToast(t.pkg_removed.replace("{name}", loc(p.title, lang, p.id)).replace("{policy}", pid).replace("{rev}", r.revision), true);
      setTick(x => x + 1); onChanged && onChanged();
    } catch (e) { onToast(e.message); } finally { setBusy(null); }
  }

  return (
    <div data-testid="agent-catalogue" style={{ marginTop: 22 }}>
      <AddPackageDialog open={!!dialog} onClose={() => setDialog(null)} lang={lang} d={d} item={dialog && dialog.item}
        policies={policies} agentsByPolicy={agentsByPolicy} fixedPolicy={dialog && dialog.policy} fbFloor={fbFloor}
        onToast={onToast} onDone={() => { setTick(x => x + 1); onChanged && onChanged(); }} />
      <div className="section-title" style={{ justifyContent: "space-between" }}>
        <span>{t.pkg_section_h} <span className="tab-count">{items.filter(p => p.policies.length).length}/{items.length}</span></span>
        <label className="hint" style={{ textTransform: "none", letterSpacing: 0, fontWeight: 400, display: "flex", gap: 6, alignItems: "center" }}>
          <input type="checkbox" checked={pre} onChange={e => setPre(e.target.checked)} data-testid="pkg-prerelease" />{t.pkg_prerelease}
        </label>
      </div>
      <p className="hint" style={{ margin: "0 0 12px" }}>{t.pkg_section_p}</p>
      {err && <Banner tone="danger" title={t.pkg_load_err}><span className="mono">{err}</span></Banner>}
      {!view && !err && <p className="hint">{t.pkg_loading}</p>}
      {view && view.source !== "registry" && (
        <Banner tone="warn" title={view.source === "cache" ? t.pkg_stale.replace("{age}", `${view.age_seconds || 0}s`) : t.pkg_empty} testid="pkg-degraded">
          {view.error && <pre>{view.error}</pre>}
        </Banner>
      )}
      {view && items.length > 0 && (
        <div className="integ-layout">
          <div>
            <label className="search" style={{ position: "relative", display: "flex", alignItems: "center", marginBottom: 10 }}>
              <Icon name="search" size={14} style={{ position: "absolute", left: 10, color: "var(--text-3)", pointerEvents: "none" }} />
              <input className="input" style={{ paddingLeft: 32 }} value={q} onChange={e => setQ(e.target.value)}
                placeholder={t.pkg_search_ph} aria-label={t.pkg_search_ph} data-testid="pkg-search" />
            </label>
            <nav className="catnav" aria-label={t.pkg_section_h}>
              <button type="button" aria-pressed={cat === "all"} onClick={() => setCat("all")}><span>{t.pkg_filter_all}</span><i>{items.length}</i></button>
              <button type="button" aria-pressed={cat === "installed"} onClick={() => setCat("installed")} data-testid="pkg-cat-installed">
                <span>{t.pkg_filter_installed}</span><i>{items.filter(p => p.policies.length).length}</i>
              </button>
              {cats.map(c => (
                <button type="button" key={c} aria-pressed={cat === c} onClick={() => setCat(c)}>
                  <span>{t[`pkg_cat_${c}`] || c}</span><i>{items.filter(p => (p.categories || []).includes(c)).length}</i>
                </button>
              ))}
            </nav>
            <div style={{ display: "flex", gap: 6, flexWrap: "wrap", marginTop: 10 }}>
              {SIGNALS.map(sg => (
                <button type="button" key={sg} className={`badge${signal === sg ? " green" : ""}`} style={{ cursor: "pointer" }}
                  aria-pressed={signal === sg} onClick={() => setSignal(signal === sg ? "" : sg)} data-testid={`pkg-signal-${sg}`}>
                  {t[`pkg_signal_${sg}`]}
                </button>
              ))}
            </div>
          </div>
          <div className="integ-grid">
            {shown.map(p => {
              const on = p.policies.length > 0;
              const title = loc(p.title, lang, p.id);
              const latest = (p.versions || []).find(v => v.version === p.latest) || (p.versions || [])[0] || {};
              return (
                <div key={p.id} className={`card integ-card${on ? " on" : ""}`} data-testid={`pkg-${p.id}`} data-state={on ? "installed" : "available"}>
                  <div className="top">
                    <span className={`integ-mono${on ? " on" : ""}`}>{monogram(title)}</span>
                    <b title={title}>{title}</b>
                  </div>
                  <div className="d">{loc(p.summary, lang, "")}</div>
                  {!p.offered && <p className="hint" style={{ color: "var(--warn-text)" }}>{t.pkg_not_offered.replace("{v}", latest.min_mesh_version || "?")}</p>}
                  <div className="ft">
                    {!on ? (
                      <Btn variant="outline" size="sm" icon="plus" disabled={!p.offered || !!locked || busy === p.id}
                        data-testid={`pkg-add-${p.id}`} onClick={() => setDialog({ item: p })}>{t.pkg_add}</Btn>
                    ) : (
                      <>
                        <span className="badge green" data-testid={`pkg-installed-${p.id}`}><Icon name="check" size={11} />{t.pkg_in_policies.replace("{n}", p.policies.length)}</span>
                        <Menu label={t.more_actions} testid={`pkg-menu-${p.id}`} items={[
                          { label: t.pkg_add_to_policy, icon: "plus", disabled: !p.offered || !!locked, onClick: () => setDialog({ item: p }) },
                          { sep: true },
                          ...p.policies.map(pid => ({ label: t.pkg_configure_in.replace("{policy}", pid), icon: "sliders", disabled: !!locked,
                            testid: `pkg-edit-${p.id}-${pid}`, onClick: () => setDialog({ item: p, policy: pid }) })),
                          { sep: true },
                          ...p.policies.map(pid => ({ label: t.pkg_remove_from.replace("{policy}", pid), icon: "trash", danger: true, disabled: !!locked || busy === p.id,
                            testid: `pkg-remove-${p.id}-${pid}`, onClick: () => removeFrom(p, pid) })),
                        ]} />
                      </>
                    )}
                    {sigOf(p).map(sg => <span key={sg} className="badge">{t[`pkg_signal_${sg}`] || sg}</span>)}
                    <span className="badge mono">{p.offered || p.latest}</span>
                    {latest.release && latest.release !== "ga" && <span className="badge yellow">{t[`pkg_release_${latest.release}`] || latest.release}</span>}
                  </div>
                </div>
              );
            })}
          </div>
        </div>
      )}
    </div>
  );
}

export { AgentCatalogue, AddPackageDialog };
