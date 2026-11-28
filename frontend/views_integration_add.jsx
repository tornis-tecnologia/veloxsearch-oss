// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
/* ============================================================
   views_integration_add.jsx — the one onboarding for an integration
   (ADR-073), modelled on Elastic's "Add integration":

     1. where to collect   — this cluster's collector, or one of the
                             deployment's Velox Mesh policies (or a new one)
     2. configure          — the package's inputs, on/off, with the variables
                             their templates reference, defaults from the
                             signed manifest
     3. add                — the core installs the backend half and either
                             deploys the collector or stores a new policy
                             revision the agents pick up on their next check-in

   The form is GENERATED from `POST /api/integration_artifacts`: inputs,
   targets and variables come off the verified package, never from a table
   kept here. A package with one input and no variables (every 1.0 package)
   renders as one toggle and nothing to fill.

   Editing an existing activation is the same dialog with the target fixed
   and the recorded selection as the initial values. The Mesh tab's Policies
   screen opens it too, with the policy as the fixed target (ADR-073: one
   onboarding, two doors).
   ============================================================ */
import { useEffect, useMemo, useState } from "react";
import { STR } from "./i18n.jsx";
import { API } from "./api.jsx";
import { Banner, Btn, Dialog, Field, Icon } from "./ui.jsx";
import { PreflightFindings } from "./views_integration_status.jsx";

const CLUSTER = "cluster";
const NEW_POLICY = "__new__";

function loc(map, lang, fallback) {
  if (!map) return fallback;
  return map[lang] || map.en || map.pt || Object.values(map)[0] || fallback;
}

function slug(name) {
  return name.trim().toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "").slice(0, 40) || "policy";
}

/* The initial form state for a target: every input the package pre-selects
   (or the recorded selection when editing), each variable at its default. */
function initialState(schema, target, initial) {
  const on = {};
  const vals = {};
  (schema.inputs || []).forEach(i => {
    const rec = initial && initial.inputs ? initial.inputs[i.id] : undefined;
    const hasRecord = initial && initial.inputs && Object.keys(initial.inputs).length > 0;
    on[i.id] = hasRecord ? !!rec : i.enabled;
    vals[i.id] = {};
    (i.vars || []).forEach(v => {
      const fromRec = rec && rec.vars ? rec.vars[v.name] : undefined;
      vals[i.id][v.name] = fromRec != null ? String(fromRec) : (v.default != null ? String(v.default) : "");
    });
  });
  return { on, vals };
}

function appliesTo(v, target) {
  const kind = target === CLUSTER ? "cluster" : "host";
  return !v.targets || v.targets.length === 0 || v.targets.includes(kind);
}

function collectableOn(i, target) {
  const kind = target === CLUSTER ? "cluster" : "host";
  return (i.targets || []).includes(kind);
}

function AddIntegrationDialog({ open, onClose, lang, d, item, policies, meshReady, fixedTarget, editing: editingProp, initialTarget, initial, agentsByPolicy, onDone, onToast }) {
  const t = STR[lang] || STR.pt;
  const [schema, setSchema] = useState(null);
  const [schemaErr, setSchemaErr] = useState(null);
  const [step, setStep] = useState(fixedTarget ? 2 : 1);
  const [target, setTarget] = useState(fixedTarget || initialTarget || CLUSTER);
  const [newName, setNewName] = useState("");
  const [form, setForm] = useState({ on: {}, vals: {} });
  const [errors, setErrors] = useState({});
  const [busy, setBusy] = useState(false);
  const [saveErr, setSaveErr] = useState(null);
  // A new policy is bound to one OpenSearch and one Mesh endpoint of the
  // deployment's settings (ADR-072 amendment 2), both required. Read here so
  // "new policy" can offer them — one of each is picked silently, more are a
  // choice, none disables the option and says where to configure them.
  const [settings, setSettings] = useState(null);
  const [newOs, setNewOs] = useState("");
  const [newMesh, setNewMesh] = useState("");

  // A fixed target skips "where". It is an EDIT when the caller says so (or,
  // by default, whenever the target is fixed): the Policies tab fixes the
  // target for a plain add too and passes `editing={false}`.
  const editing = editingProp != null ? !!editingProp : !!fixedTarget;
  const title = (editing ? t.add_integ_edit_title : t.add_integ_title).replace("{name}", item ? loc(item.title, lang, item.id) : "");

  useEffect(() => {
    if (!open || !item) return undefined;
    let alive = true;
    setSchema(null); setSchemaErr(null); setSaveErr(null); setErrors({});
    setStep(fixedTarget ? 2 : 1);
    setTarget(fixedTarget || initialTarget || CLUSTER);
    API.integrationArtifacts(item.id)
      .then(s => { if (alive) { setSchema(s); setForm(initialState(s, fixedTarget || initialTarget || CLUSTER, initial)); } })
      .catch(e => { if (alive) setSchemaErr(e.message); });
    if (meshReady && !fixedTarget) {
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
    }
    return () => { alive = false; };
    // The dialog re-reads the package each time it opens on an item.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, item && item.id]);

  const inputsHere = useMemo(() => (schema ? (schema.inputs || []).filter(i => collectableOn(i, target)) : []), [schema, target]);

  if (!open || !item) return null;

  const policyLabel = (id) => id === CLUSTER ? t.add_integ_cluster : id;
  const osEps = (settings && settings.os_endpoints) || [];
  const meshEps = (settings && settings.mesh_endpoints) || [];
  const canNew = !!settings && osEps.length > 0 && meshEps.length > 0;
  const newReady = target !== NEW_POLICY || (!!newName.trim() && !!newOs && !!newMesh);

  function validate() {
    const errs = {};
    inputsHere.forEach(i => {
      if (!form.on[i.id]) return;
      (i.vars || []).forEach(v => {
        if (!appliesTo(v, target)) return;
        const val = (form.vals[i.id] || {})[v.name] ?? "";
        if (v.required && val.trim() === "") errs[`${i.id}.${v.name}`] = t.add_integ_required;
        else if (v.kind === "integer" && val.trim() !== "" && !/^-?\d+$/.test(val.trim())) errs[`${i.id}.${v.name}`] = t.add_integ_invalid_int;
      });
    });
    if (!inputsHere.some(i => form.on[i.id])) errs.__none = t.add_integ_pick_one;
    setErrors(errs);
    return Object.keys(errs).length === 0;
  }

  function selection() {
    const inputs = {};
    inputsHere.forEach(i => {
      if (!form.on[i.id]) return;
      const vars = {};
      (i.vars || []).forEach(v => {
        if (!appliesTo(v, target)) return;
        const val = (form.vals[i.id] || {})[v.name];
        if (val != null && String(val).trim() !== "") vars[v.name] = String(val).trim();
      });
      inputs[i.id] = { vars };
    });
    return { inputs };
  }

  async function save() {
    if (!validate()) return;
    setBusy(true); setSaveErr(null);
    try {
      let policy = target;
      if (target === NEW_POLICY) {
        const id = slug(newName);
        await API.meshCreatePolicy(d.id, id, newName.trim() || id, newOs, newMesh);
        policy = id;
      }
      const r = await API.addIntegration(d.id, item.id, policy, selection());
      const name = loc(item.title, lang, item.id);
      if (policy === CLUSTER) {
        onToast && onToast(t.add_integ_done_cluster.replace("{name}", name), true);
      } else {
        onToast && onToast(t.add_integ_done_policy.replace("{name}", name).replace("{policy}", policy).replace("{rev}", r.revision), true);
      }
      onDone && onDone(policy, r);
      onClose && onClose();
    } catch (e) {
      setSaveErr(e.message);
    } finally { setBusy(false); }
  }

  const noAgents = (pid) => agentsByPolicy && !(agentsByPolicy[pid] > 0);

  return (
    <Dialog open={open} onClose={busy ? undefined : onClose} title={title} width={560} testid="add-integration" busy={busy}>
      {schemaErr && <Banner tone="danger" title={t.add_integ_err}>{schemaErr}</Banner>}
      {!schema && !schemaErr && <p className="hint">{t.add_integ_loading}</p>}

      {schema && step === 1 && (
        <div data-testid="add-integration-where">
          <p className="hint" style={{ margin: "0 0 12px" }}>{t.add_integ_where_p}</p>
          <div className="onb-steps" role="radiogroup" aria-label={t.add_integ_where_h}>
            <label className={`mesh-card${target === CLUSTER ? " on" : ""}`} style={{ display: "block", cursor: "pointer" }} data-testid="add-target-cluster">
              <input type="radio" name="target" checked={target === CLUSTER} onChange={() => setTarget(CLUSTER)} style={{ marginRight: 8 }} />
              <b>{t.add_integ_cluster}</b>
              <div className="hint" style={{ marginLeft: 22 }}>{t.add_integ_cluster_p}</div>
              {/* ADR-075: what would stand in the way, before anything is written. */}
              {target === CLUSTER && <div style={{ marginTop: 8 }}><PreflightFindings d={d} id={item.id} lang={lang} /></div>}
            </label>
            {meshReady && (policies || []).map(p => (
              <label key={p.id} className={`mesh-card${target === p.id ? " on" : ""}`} style={{ display: "block", cursor: "pointer" }} data-testid={`add-target-${p.id}`}>
                <input type="radio" name="target" checked={target === p.id} onChange={() => setTarget(p.id)} style={{ marginRight: 8 }} />
                <b className="mono">{p.id}</b>
                <span className="hint" style={{ marginLeft: 8 }}>
                  {t.add_integ_policy_p.replace("{n}", (agentsByPolicy && agentsByPolicy[p.id]) || 0)}
                  {(p.integrations || []).includes(item.id) ? ` · ${t.add_integ_already}` : ""}
                </span>
              </label>
            ))}
            {meshReady && (
              <label className={`mesh-card${target === NEW_POLICY ? " on" : ""}`} style={{ display: "block", cursor: canNew ? "pointer" : "not-allowed", opacity: settings && !canNew ? 0.7 : 1 }} data-testid="add-target-new">
                <input type="radio" name="target" checked={target === NEW_POLICY} disabled={!canNew} onChange={() => setTarget(NEW_POLICY)} style={{ marginRight: 8 }} />
                <b>{t.add_integ_new_policy}</b>
                {settings && !canNew && (
                  <div className="hint" style={{ marginLeft: 22 }} data-testid="add-new-policy-need-endpoints">{t.add_integ_new_policy_need_ep}</div>
                )}
                {target === NEW_POLICY && (
                  <div style={{ display: "grid", gap: 8, marginTop: 8, maxWidth: 360 }}>
                    <input className="input" value={newName} onChange={e => setNewName(e.target.value)}
                      placeholder={t.add_integ_new_policy_ph} aria-label={t.add_integ_new_policy_ph}
                      data-testid="add-new-policy-name" />
                    {osEps.length > 1 && (
                      <select className="select" value={newOs} aria-label={t.mesh_pol_os_endpoint} data-testid="add-new-policy-os"
                        onChange={e => setNewOs(e.target.value)}>
                        <option value="" disabled>{t.mesh_pol_os_endpoint}</option>
                        {osEps.map(e => <option key={e.id} value={e.id}>{e.name} · {e.host}</option>)}
                      </select>
                    )}
                    {meshEps.length > 1 && (
                      <select className="select" value={newMesh} aria-label={t.mesh_pol_mesh_endpoint} data-testid="add-new-policy-mesh"
                        onChange={e => setNewMesh(e.target.value)}>
                        <option value="" disabled>{t.mesh_pol_mesh_endpoint}</option>
                        {meshEps.map(e => <option key={e.id} value={e.id}>{e.name} · {e.url}</option>)}
                      </select>
                    )}
                    {osEps.length === 1 && meshEps.length === 1 && (
                      <span className="hint mono" style={{ fontSize: 12 }}>{osEps[0].host} · {meshEps[0].url}</span>
                    )}
                  </div>
                )}
              </label>
            )}
          </div>
          {!meshReady && <p className="hint" style={{ marginTop: 10 }}>{t.add_integ_no_mesh}</p>}
          <div className="dialog-actions">
            <Btn variant="ghost" onClick={onClose}>{t.cancel}</Btn>
            <Btn variant="primary" data-testid="add-integration-next"
              disabled={!newReady}
              onClick={() => { setForm(initialState(schema, target, initial)); setErrors({}); setStep(2); }}>
              {t.add_integ_next}
            </Btn>
          </div>
        </div>
      )}

      {schema && step === 2 && (
        <div data-testid="add-integration-configure">
          <p className="hint" style={{ margin: "0 0 4px" }}>
            {t.add_integ_target_is} <b className="mono">{target === NEW_POLICY ? slug(newName) : policyLabel(target)}</b>
          </p>
          <p className="hint" style={{ margin: "0 0 12px" }}>{t.add_integ_configure_p}</p>
          {inputsHere.length === 0 && <Banner tone="warn" title={t.add_integ_no_inputs_target} />}
          {errors.__none && <div className="field-err" role="alert">{errors.__none}</div>}
          {inputsHere.map(i => {
            const on = !!form.on[i.id];
            const vars = (i.vars || []).filter(v => appliesTo(v, target));
            return (
              <div key={i.id} className={`mesh-card${on ? " on" : ""}`} style={{ marginBottom: 10, cursor: "default" }} data-testid={`add-input-${i.id}`} data-on={on ? "1" : "0"}>
                <label style={{ display: "flex", gap: 8, alignItems: "flex-start", cursor: "pointer" }}>
                  <input type="checkbox" checked={on} data-testid={`add-input-toggle-${i.id}`}
                    onChange={e => setForm(f => ({ ...f, on: { ...f.on, [i.id]: e.target.checked } }))} style={{ marginTop: 3 }} />
                  <span style={{ minWidth: 0 }}>
                    <b>{loc(i.title, lang, i.id)}</b>
                    <span className="badge mono" style={{ marginLeft: 8 }}>{i.signal}</span>
                    {loc(i.description, lang, "") && <div className="hint">{loc(i.description, lang, "")}</div>}
                    <div className="hint mono" style={{ fontSize: 11.5 }}>{i.index}</div>
                  </span>
                </label>
                {on && vars.length > 0 && (
                  <div style={{ marginTop: 10, paddingLeft: 24 }}>
                    {vars.map(v => {
                      const key = `${i.id}.${v.name}`;
                      const val = (form.vals[i.id] || {})[v.name] ?? "";
                      const set = (nv) => setForm(f => ({ ...f, vals: { ...f.vals, [i.id]: { ...(f.vals[i.id] || {}), [v.name]: nv } } }));
                      return (
                        <Field key={v.name} label={<>{loc(v.description, lang, v.name)}{v.required && <span className="hint"> · {t.add_integ_required}</span>}</>}
                          error={errors[key]} htmlFor={`var-${key}`} style={{ marginBottom: 10 }}>
                          {v.kind === "boolean" ? (
                            <label style={{ display: "flex", gap: 8, alignItems: "center" }}>
                              <input type="checkbox" id={`var-${key}`} checked={val === "true"} data-testid={`add-var-${key}`}
                                onChange={e => set(e.target.checked ? "true" : "false")} />
                              <span className="mono hint">{v.name}</span>
                            </label>
                          ) : (
                            <input className="input mono" id={`var-${key}`} value={val} data-testid={`add-var-${key}`}
                              inputMode={v.kind === "integer" ? "numeric" : undefined}
                              placeholder={v.default != null ? String(v.default) : v.name}
                              onChange={e => set(e.target.value)} />
                          )}
                        </Field>
                      );
                    })}
                  </div>
                )}
              </div>
            );
          })}
          {saveErr && <Banner tone="danger" title={t.add_integ_err} style={{ marginTop: 10 }}>{saveErr}</Banner>}
          {target !== CLUSTER && target !== NEW_POLICY && noAgents(target) && (
            <p className="hint" style={{ marginTop: 6 }}>{t.add_integ_done_no_agents}</p>
          )}
          <div className="dialog-actions">
            {!editing && <Btn variant="ghost" onClick={() => setStep(1)} disabled={busy}>{t.add_integ_back}</Btn>}
            <Btn variant="ghost" onClick={onClose} disabled={busy}>{t.cancel}</Btn>
            <Btn variant="primary" icon={editing ? "check" : "plus"} disabled={busy || inputsHere.length === 0}
              data-testid="add-integration-save" onClick={save}>
              {busy ? t.add_integ_saving : (editing ? t.add_integ_save_edit : t.add_integ_save)}
            </Btn>
          </div>
        </div>
      )}
      {busy && <div className="hint" style={{ marginTop: 8 }}><Icon name="clock" size={12} /> {t.add_integ_saving}</div>}
    </Dialog>
  );
}

export { AddIntegrationDialog, CLUSTER as CLUSTER_TARGET, loc as locText };
