// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
/* ============================================================
   Backup tab + the wizard's Backup step — snapshot repository and the
   scheduled snapshot policy (ADR-049).

   One S3-compatible target the user owns: AWS S3, MinIO, Wasabi, anything
   speaking the S3 API. The form is the SAME component in both places, so the
   wizard and the day-2 edit cannot drift apart:

     - `SnapshotForm`  the fields themselves (bucket / endpoint / credentials
                       and the collapsed default policy)
     - `SnapshotTab`   the deployment tab: its own read, verify probe, and the
                       restart confirmation when the keystore moves

   Credentials are write-only. The API returns them as the `secret_kept`
   sentinel and sending the sentinel back means "unchanged" — nothing here ever
   displays a stored key.

   The restart rule is NOT computed here (ADR-049 invariant 4): the server is
   asked what a save would do, and this renders the answer. Changing the
   credentials rolls the nodes (the keystore is read at pod start); changing
   only the schedule or the bucket does not.
   ============================================================ */
import React, { useState, useEffect } from "react";
import { STR } from "./i18n.jsx";
import { Icon, Field, Btn, Confirm } from "./ui.jsx";
import { LockNotice } from "./views_activity.jsx";
import { API } from "./api.jsx";

// Mirrors `snapshot::SECRET_KEPT` — the value the API sends instead of a key.
const SECRET_KEPT = "secret_kept";

// Field rows. `align-items: start` keeps labels on one baseline when a column's
// content is taller, and `auto-fit`/`minmax` collapses to one column on narrow
// viewports instead of squeezing a number input to nothing.
const ROW2 = {
  display: "grid",
  gridTemplateColumns: "repeat(auto-fit, minmax(190px, 1fr))",
  gap: 14,
  alignItems: "start",
};
const ROW3 = {
  display: "grid",
  gridTemplateColumns: "repeat(auto-fit, minmax(130px, 1fr))",
  gap: 14,
  alignItems: "start",
};
// One hint per row rather than per field — see the comment at the row itself.
const HINT = { margin: "-8px 0 16px" };

// Mirrors `snapshot::SnapshotConfig::default()` / `PolicyConfig::default()`.
// The defaults are a working daily backup on purpose: a user who enables
// backup and touches nothing else still gets one.
function emptyConfig() {
  return {
    enabled: true,
    bucket: "",
    base_path: "",
    endpoint: "",
    region: "us-east-1",
    path_style_access: true,
    access_key: "",
    secret_key: "",
    policy: {
      enabled: true,
      cron: "0 2 * * *",
      timezone: "UTC",
      indices: "*",
      include_global_state: true,
      max_age_days: 7,
      max_count: 14,
      min_count: 3,
    },
  };
}

/* --- A credential the server never sends back in clear (the ADR-045
   SecretInput contract, kept identical so the two screens behave the same). --- */
function SecretField({ id, value, onChange, testid, t }) {
  const stored = value === SECRET_KEPT;
  const [show, setShow] = useState(false);
  if (stored) {
    return (
      <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
        <span className="badge green"><span className="dot" />•••••••</span>
        <button className="btn-link" type="button" onClick={() => onChange("")}
          data-testid={testid && `${testid}-replace`}>
          {t.auth_secret_replace}
        </button>
      </div>
    );
  }
  return (
    <div style={{ display: "flex", gap: 8 }}>
      <input id={id} className="input" type={show ? "text" : "password"} value={value || ""}
        autoComplete="new-password" onChange={(e) => onChange(e.target.value)} data-testid={testid} />
      <Btn variant="outline" type="button" onClick={() => setShow((s) => !s)}
        aria-label={show ? "hide" : "show"} icon={show ? "eyeOff" : "eye"} />
    </div>
  );
}

/* --- The fields. Shared by the wizard step and the deployment tab. --- */
function SnapshotForm({ cfg, onChange, lang, idPrefix = "snap" }) {
  const t = STR[lang];
  const [advanced, setAdvanced] = useState(false);
  const set = (k, v) => onChange({ ...cfg, [k]: v });
  const setPolicy = (k, v) => onChange({ ...cfg, policy: { ...cfg.policy, [k]: v } });
  const num = (v, fallback) => {
    const n = parseInt(v, 10);
    return Number.isFinite(n) && n >= 0 ? n : fallback;
  };

  return (
    <div style={{ display: "grid", gap: 4 }}>
      <Field label={t.snap_bucket} hint={t.snap_bucket_hint} htmlFor={`${idPrefix}-bucket`}>
        <input id={`${idPrefix}-bucket`} className="input" value={cfg.bucket}
          placeholder="velox-snapshots" onChange={(e) => set("bucket", e.target.value)}
          data-testid="snap-bucket" />
      </Field>

      <Field label={t.snap_endpoint} hint={t.snap_endpoint_hint} htmlFor={`${idPrefix}-endpoint`}>
        <input id={`${idPrefix}-endpoint`} className="input" value={cfg.endpoint}
          placeholder="http://minio.minio.svc:9000" onChange={(e) => set("endpoint", e.target.value)}
          data-testid="snap-endpoint" />
      </Field>

      <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 14 }}>
        <Field label={t.snap_access_key} htmlFor={`${idPrefix}-access`}>
          <SecretField id={`${idPrefix}-access`} value={cfg.access_key} t={t}
            onChange={(v) => set("access_key", v)} testid="snap-access-key" />
        </Field>
        <Field label={t.snap_secret_key} htmlFor={`${idPrefix}-secret`}>
          <SecretField id={`${idPrefix}-secret`} value={cfg.secret_key} t={t}
            onChange={(v) => set("secret_key", v)} testid="snap-secret-key" />
        </Field>
      </div>

      {/* The defaults below already describe a working daily backup, so this
          stays collapsed — the ADR-015 "basics first" rule. */}
      <button className="btn-link" type="button" style={{ justifySelf: "start", marginTop: 4 }}
        onClick={() => setAdvanced((a) => !a)} data-testid="snap-advanced">
        <Icon name={advanced ? "chevD" : "chevR"} size={13} /> {t.snap_policy_h}
      </button>

      {advanced && (
        <div style={{
          marginTop: 10, padding: 16, borderRadius: "var(--radius)",
          background: "var(--surface-2)", border: "1px solid var(--border)",
        }}>
          <label style={{ display: "flex", alignItems: "center", gap: 10, cursor: "pointer", marginBottom: 14 }}>
            <input type="checkbox" checked={!!cfg.policy.enabled}
              onChange={(e) => setPolicy("enabled", e.target.checked)}
              style={{ accentColor: "var(--accent)", width: 16, height: 16 }}
              data-testid="snap-policy-enabled" />
            <span style={{ fontSize: 14 }}>{t.snap_policy_enabled}</span>
          </label>

          {/* Two groups, because these were one box holding two unrelated
              things: WHEN snapshots are taken (the policy) and WHERE inside the
              bucket they land (the repository). Within a group every field in a
              row carries label+input only — a per-field hint is ~19px tall, so
              mixing hinted and unhinted fields in one row made the inputs sit
              at different heights. The hints moved to one line under each row,
              which also stopped the columns fighting for width. */}
          <div style={ROW2}>
            <Field label={t.snap_cron} htmlFor={`${idPrefix}-cron`}>
              <input id={`${idPrefix}-cron`} className="input" value={cfg.policy.cron}
                disabled={!cfg.policy.enabled} onChange={(e) => setPolicy("cron", e.target.value)}
                style={{ fontFamily: "var(--font-mono)" }} data-testid="snap-cron" />
            </Field>
            <Field label={t.snap_timezone} htmlFor={`${idPrefix}-tz`}>
              <input id={`${idPrefix}-tz`} className="input" value={cfg.policy.timezone}
                disabled={!cfg.policy.enabled} onChange={(e) => setPolicy("timezone", e.target.value)}
                placeholder="UTC" data-testid="snap-timezone" />
            </Field>
          </div>
          <p className="hint" style={HINT}>{t.snap_cron_hint}</p>

          <div style={ROW3}>
            <Field label={t.snap_max_age} htmlFor={`${idPrefix}-age`}>
              <input id={`${idPrefix}-age`} className="input" type="number" min="1"
                value={cfg.policy.max_age_days} disabled={!cfg.policy.enabled}
                onChange={(e) => setPolicy("max_age_days", num(e.target.value, 7))}
                data-testid="snap-max-age" />
            </Field>
            <Field label={t.snap_max_count} htmlFor={`${idPrefix}-max`}>
              <input id={`${idPrefix}-max`} className="input" type="number" min="1"
                value={cfg.policy.max_count} disabled={!cfg.policy.enabled}
                onChange={(e) => setPolicy("max_count", num(e.target.value, 14))} />
            </Field>
            <Field label={t.snap_min_count} htmlFor={`${idPrefix}-min`}>
              <input id={`${idPrefix}-min`} className="input" type="number" min="0"
                value={cfg.policy.min_count} disabled={!cfg.policy.enabled}
                onChange={(e) => setPolicy("min_count", num(e.target.value, 3))} />
            </Field>
          </div>
          <p className="hint" style={HINT}>{t.snap_retention_hint}</p>

          <Field label={t.snap_indices} hint={t.snap_indices_hint} htmlFor={`${idPrefix}-indices`}>
            <input id={`${idPrefix}-indices`} className="input" value={cfg.policy.indices}
              disabled={!cfg.policy.enabled} onChange={(e) => setPolicy("indices", e.target.value)}
              style={{ fontFamily: "var(--font-mono)" }} />
          </Field>

          <div className="hr" style={{ margin: "6px 0 16px" }} />
          <div style={{ fontSize: 13, fontFamily: "var(--font-mono)", color: "var(--text-2)", marginBottom: 12 }}>
            {t.snap_bucket_opts}
          </div>

          <div style={ROW2}>
            <Field label={t.snap_base_path} htmlFor={`${idPrefix}-base`}>
              <input id={`${idPrefix}-base`} className="input" value={cfg.base_path}
                onChange={(e) => set("base_path", e.target.value)} />
            </Field>
            <Field label={t.snap_region} htmlFor={`${idPrefix}-region`}>
              <input id={`${idPrefix}-region`} className="input" value={cfg.region}
                onChange={(e) => set("region", e.target.value)} placeholder="us-east-1" />
            </Field>
          </div>
          <p className="hint" style={HINT}>{t.snap_base_path_hint}</p>

          {/* Its own row: the old layout padded this checkbox down by 18px to
              fake alignment with the input beside it, which broke at every
              other width. */}
          <label style={{ display: "flex", alignItems: "center", gap: 10, cursor: "pointer" }}>
            <input type="checkbox" checked={!!cfg.path_style_access}
              onChange={(e) => set("path_style_access", e.target.checked)}
              style={{ accentColor: "var(--accent)", width: 16, height: 16 }} />
            <span style={{ fontSize: 13.5 }}>{t.snap_path_style}</span>
          </label>
        </div>
      )}
    </div>
  );
}

/* --- The wizard's optional step. Off by default: skipping it must cost
   nothing, and the deployment tab means skipping is never a one-way door. --- */
function SnapshotStep({ cfg, onChange, lang }) {
  const t = STR[lang];
  const on = !!cfg;
  return (
    <div className="view-enter">
      <div className="section-title">{t.snap_h}</div>
      <p className="hint" style={{ marginTop: -6, marginBottom: 16 }}>{t.snap_step_p}</p>

      <label className="recipe" style={{
        display: "flex", alignItems: "center", gap: 12, cursor: "pointer", padding: "13px 16px",
        borderColor: on ? "var(--accent-border)" : "var(--border)",
      }}>
        <input type="checkbox" checked={on} data-testid="snap-toggle"
          onChange={(e) => onChange(e.target.checked ? emptyConfig() : null)}
          style={{ accentColor: "var(--accent)", width: 16, height: 16 }} />
        <span style={{ flex: 1, minWidth: 0 }}>
          <span style={{ fontSize: 14 }}>{t.snap_step_toggle}</span>
          <span style={{ display: "block", fontSize: 12.5, color: "var(--text-3)", marginTop: 2 }}>
            {t.snap_step_toggle_d}
          </span>
        </span>
      </label>

      {!on && (
        <div style={{
          display: "flex", gap: 12, padding: 16, marginTop: 14, borderRadius: "var(--radius)",
          background: "var(--info-soft)", border: "1px solid var(--border)", color: "var(--text-2)", fontSize: 14,
        }}>
          <span style={{ color: "var(--info)", flexShrink: 0 }}><Icon name="bolt" size={18} /></span>
          {t.snap_step_skip}
        </div>
      )}

      {on && (
        <div style={{ marginTop: 18 }}>
          <SnapshotForm cfg={cfg} onChange={onChange} lang={lang} idPrefix="wiz-snap" />
          {/* The plugin install is a pod-spec change; on a NEW deployment it
              costs nothing (there is nothing running yet), which is exactly why
              configuring here is cheaper than configuring later. */}
          <p className="hint" style={{ marginTop: 14 }}>{t.snap_step_note}</p>
        </div>
      )}
    </div>
  );
}

/* --- Result of the non-writing `_verify` probe. The S3 reason is the whole
   value of this call, so it is shown verbatim. --- */
function VerifyPanel({ probe, t }) {
  if (!probe) return null;
  const tone = probe.ok ? "var(--accent)" : "var(--danger)";
  return (
    <div className="card pad" data-testid="snap-verify-result"
      style={{ marginTop: 16, borderColor: probe.ok ? "var(--accent-border)" : "var(--danger-border)" }}>
      <div style={{ display: "flex", alignItems: "center", gap: 8, color: tone, fontFamily: "var(--font-mono)", fontSize: 14 }}>
        <Icon name={probe.ok ? "check" : "bolt"} size={15} />
        {probe.ok ? t.snap_verify_ok : t.snap_verify_fail}
      </div>
      {probe.error && (
        <pre style={{
          margin: "12px 0 0", whiteSpace: "pre-wrap", wordBreak: "break-word",
          color: "var(--danger)", fontSize: 12.5, fontFamily: "var(--font-mono)",
        }}>{probe.error}</pre>
      )}
    </div>
  );
}

/* --- The deployment's Backup tab. --- */
function SnapshotTab({ d, lang, onToast, locked }) {
  const t = STR[lang];
  const [loaded, setLoaded] = useState(null);
  const [cfg, setCfg] = useState(null);
  const [probe, setProbe] = useState(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState("");
  const [confirm, setConfirm] = useState(false);

  useEffect(() => {
    let alive = true;
    API.snapshotConfig(d.id)
      .then((c) => {
        if (!alive || !c) return;
        setLoaded(c);
        setCfg(c.config);
      })
      .catch((e) => { if (alive) setErr(e.message); });
    return () => { alive = false; };
  }, [d.id]);

  if (!cfg) {
    return (
      <div className="view-enter">
        <p className="hint">{err || t.cred_loading}</p>
      </div>
    );
  }

  const state = loaded.state || {};
  const update = (next) => { setCfg(next); setProbe(null); setErr(""); };

  async function runVerify() {
    setBusy(true);
    setProbe(null);
    try {
      setProbe(await API.verifySnapshotRepo(d.id));
    } catch (e) {
      setProbe({ ok: false, checks: [], error: e.message });
    } finally {
      setBusy(false);
    }
  }

  // The server decides whether this save restarts the nodes and whether it is
  // allowed at all (invariant 4). A refusal renders here, under the control,
  // rather than as a toast (ADR-045 UI rule 1).
  async function attemptSave() {
    setBusy(true);
    setErr("");
    try {
      const plan = await API.planSnapshotConfig(d.id, cfg);
      if (plan.will_restart) { setConfirm(true); return; }
      await commit();
    } catch (e) {
      setErr(e.message);
    } finally {
      setBusy(false);
    }
  }

  async function commit() {
    setConfirm(false);
    setBusy(true);
    setErr("");
    try {
      await API.saveSnapshotConfig(d.id, cfg);
      onToast(t.snap_saved);
      const c = await API.snapshotConfig(d.id);
      setLoaded(c);
      setCfg(c.config);
    } catch (e) {
      setErr(e.message);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="view-enter">
      <div className="section-title">{t.snap_h}</div>
      <LockNotice reason={locked} />
      <p className="lead" style={{ marginTop: -6 }}>{t.snap_tab_p}</p>

      {/* Live state, in the operator's own words. PENDING while the cluster
          provisions is not an error — the repository reconciler waits for the
          cluster to be running. */}
      {state.configured && (
        <div style={{
          display: "flex", gap: 12, padding: 14, marginBottom: 18, borderRadius: "var(--radius)",
          background: state.policy_state === "ERROR" ? "var(--danger-soft)" : "var(--surface-2)",
          border: `1px solid ${state.policy_state === "ERROR" ? "var(--danger-border)" : "var(--border)"}`,
        }} data-testid="snap-state">
          <span style={{ color: state.policy_state === "ERROR" ? "var(--danger)" : "var(--accent)", flexShrink: 0 }}>
            <Icon name={state.policy_state === "ERROR" ? "shield" : "check"} size={18} />
          </span>
          <div style={{ minWidth: 0 }}>
            <div style={{ fontSize: 14, fontWeight: 600 }}>
              {state.policy_state === "ERROR" ? t.snap_state_error
                : state.policy_state === "PENDING" ? t.snap_state_pending
                  : t.snap_state_ok}
            </div>
            <div style={{ fontSize: 13, color: "var(--text-3)", marginTop: 2, fontFamily: "var(--font-mono)" }}>
              {state.repo}{state.schedule ? ` · ${state.schedule}` : ""}
            </div>
            {state.last_error && (
              <pre style={{
                margin: "8px 0 0", whiteSpace: "pre-wrap", wordBreak: "break-word",
                color: "var(--danger)", fontSize: 12.5, fontFamily: "var(--font-mono)",
              }}>{state.last_error}</pre>
            )}
          </div>
        </div>
      )}

      <label className="recipe" style={{
        display: "flex", alignItems: "center", gap: 12, cursor: "pointer", padding: "13px 16px",
        marginBottom: cfg.enabled ? 18 : 0,
        borderColor: cfg.enabled ? "var(--accent-border)" : "var(--border)",
      }}>
        <input type="checkbox" checked={!!cfg.enabled} data-testid="snap-enabled"
          onChange={(e) => update({ ...cfg, enabled: e.target.checked })}
          style={{ accentColor: "var(--accent)", width: 16, height: 16 }} />
        <span style={{ flex: 1, minWidth: 0 }}>
          <span style={{ fontSize: 14 }}>{t.snap_step_toggle}</span>
          <span style={{ display: "block", fontSize: 12.5, color: "var(--text-3)", marginTop: 2 }}>
            {t.snap_step_toggle_d}
          </span>
        </span>
      </label>

      {cfg.enabled && <SnapshotForm cfg={cfg} onChange={update} lang={lang} idPrefix="tab-snap" />}

      <div style={{ display: "flex", gap: 10, marginTop: 18, flexWrap: "wrap" }}>
        {state.configured && (
          <Btn variant="outline" icon="plug" disabled={busy} onClick={runVerify} data-testid="snap-verify">
            {busy ? t.snap_verifying : t.snap_verify}
          </Btn>
        )}
        <Btn variant="primary" icon="check" disabled={busy || !!locked} data-testid="snap-save" onClick={attemptSave}>
          {t.snap_save}
        </Btn>
      </div>

      {err && (
        <pre data-testid="snap-error" style={{
          margin: "14px 0 0", whiteSpace: "pre-wrap", wordBreak: "break-word",
          color: "var(--danger)", fontSize: 13, fontFamily: "var(--font-mono)",
        }}>{err}</pre>
      )}

      <VerifyPanel probe={probe} t={t} />

      {/* Only a credential change reaches here: the keystore is read at pod
          start, so the nodes have to roll. A schedule or bucket edit saves
          straight through with no dialog. */}
      <Confirm open={confirm} title={t.snap_restart_h} body={t.snap_restart_p}
        icon="server" variant="primary"
        confirmLabel={t.snap_restart_yes} cancelLabel={t.cancel}
        confirmTestid="snap-confirm" cancelTestid="snap-confirm-cancel"
        onCancel={() => { setConfirm(false); setBusy(false); }} onConfirm={commit} />
    </div>
  );
}

export { SnapshotTab, SnapshotStep, SnapshotForm, emptyConfig, SECRET_KEPT };
