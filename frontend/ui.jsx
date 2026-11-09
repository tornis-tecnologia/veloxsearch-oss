// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
/* ============================================================
   Shared UI primitives + simple line icons

   The design system's components live here so every screen renders the same
   banner, the same dialog and the same tabs. A screen that needs a box with a
   border and a tone reaches for <Banner>, never for an inline style — the
   first version of this app had four hand-rolled notice boxes in the create
   wizard alone, each with its own padding, and no two screens agreed.
   ============================================================ */
import { Component, useState, useEffect, useRef, useCallback } from "react";

/* --- Icons: simple stroke line icons --- */
function Icon({ name, size = 16, className, style }) {
  const s = { width: size, height: size, ...style };
  const common = {
    viewBox: "0 0 24 24", fill: "none", stroke: "currentColor",
    strokeWidth: 2, strokeLinecap: "round", strokeLinejoin: "round",
    style: s, className, "aria-hidden": "true",
  };
  const P = {
    search: <><circle cx="11" cy="11" r="7" /><path d="M21 21l-4.3-4.3" /></>,
    plus: <><path d="M12 5v14M5 12h14" /></>,
    gear: <><circle cx="12" cy="12" r="3" /><path d="M12 2v3M12 19v3M4.9 4.9L7 7M17 17l2.1 2.1M2 12h3M19 12h3M4.9 19.1L7 17M17 7l2.1-2.1" /></>,
    chevR: <path d="M9 18l6-6-6-6" />,
    chevL: <path d="M15 18l-6-6 6-6" />,
    chevD: <path d="M6 9l6 6 6-6" />,
    check: <path d="M20 6L9 17l-5-5" />,
    x: <path d="M18 6L6 18M6 6l12 12" />,
    eye: <><path d="M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7S2 12 2 12z" /><circle cx="12" cy="12" r="3" /></>,
    eyeOff: <><path d="M9.9 4.2A10 10 0 0 1 12 4c6.5 0 10 7 10 7a18 18 0 0 1-2.3 3.2M6.6 6.6A18 18 0 0 0 2 11s3.5 7 10 7a10 10 0 0 0 3.7-.7" /><path d="M3 3l18 18" /></>,
    copy: <><rect x="9" y="9" width="11" height="11" rx="2" /><path d="M5 15V5a2 2 0 0 1 2-2h10" /></>,
    download: <><path d="M12 3v12M7 10l5 5 5-5" /><path d="M4 17v2a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-2" /></>,
    server: <><rect x="3" y="4" width="18" height="7" rx="2" /><rect x="3" y="13" width="18" height="7" rx="2" /><path d="M7 7.5h.01M7 16.5h.01" /></>,
    sun: <><circle cx="12" cy="12" r="4" /><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4" /></>,
    moon: <path d="M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z" />,
    trash: <><path d="M3 6h18M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6" /></>,
    shield: <path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z" />,
    plug: <><path d="M9 2v6M15 2v6M7 8h10v3a5 5 0 0 1-10 0z M12 16v6" /></>,
    activity: <path d="M22 12h-4l-3 9L9 3l-3 9H2" />,
    layers: <><path d="M12 2l9 5-9 5-9-5 9-5z" /><path d="M3 12l9 5 9-5M3 17l9 5 9-5" /></>,
    sliders: <><path d="M4 21v-7M4 10V3M12 21v-9M12 8V3M20 21v-5M20 12V3M1 14h6M9 8h6M17 16h6" /></>,
    bolt: <path d="M13 2L3 14h7l-1 8 10-12h-7l1-8z" />,
    arrowR: <path d="M5 12h14M13 5l7 7-7 7" />,
    arrowUp: <path d="M12 19V5M5 12l7-7 7 7" />,
    ext: <><path d="M14 3h7v7M21 3l-9 9" /><path d="M19 14v5a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7a2 2 0 0 1 2-2h5" /></>,
    spark: <path d="M12 2l2 7 7 2-7 2-2 7-2-7-7-2 7-2 2-7z" />,
    clock: <><circle cx="12" cy="12" r="9" /><path d="M12 7v5l3 2" /></>,
    db: <><ellipse cx="12" cy="5" rx="8" ry="3" /><path d="M4 5v6c0 1.7 3.6 3 8 3s8-1.3 8-3V5M4 11v6c0 1.7 3.6 3 8 3s8-1.3 8-3v-6" /></>,
    grid: <><rect x="3" y="3" width="7" height="7" rx="1.5" /><rect x="14" y="3" width="7" height="7" rx="1.5" /><rect x="3" y="14" width="7" height="7" rx="1.5" /><rect x="14" y="14" width="7" height="7" rx="1.5" /></>,
    gauge: <path d="M12 14l4-4M3.5 18a10 10 0 1 1 17 0" />,
    dots: <><circle cx="5" cy="12" r="1.3" fill="currentColor" /><circle cx="12" cy="12" r="1.3" fill="currentColor" /><circle cx="19" cy="12" r="1.3" fill="currentColor" /></>,
    menu: <path d="M3 6h18M3 12h18M3 18h18" />,
    help: <><circle cx="12" cy="12" r="10" /><path d="M9.1 9a3 3 0 0 1 5.8 1c0 2-3 3-3 3M12 17h.01" /></>,
    info: <><circle cx="12" cy="12" r="10" /><path d="M12 16v-4M12 8h.01" /></>,
    alert: <path d="M10.3 3.9L1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0zM12 9v4M12 17h.01" />,
    user: <><path d="M20 21v-2a4 4 0 0 0-4-4H8a4 4 0 0 0-4 4v2" /><circle cx="12" cy="7" r="4" /></>,
    logout: <><path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4" /><path d="M16 17l5-5-5-5M21 12H9" /></>,
    globe: <><circle cx="12" cy="12" r="10" /><path d="M2 12h20M12 2a15 15 0 0 1 0 20a15 15 0 0 1 0-20" /></>,
    book: <><path d="M4 19.5A2.5 2.5 0 0 1 6.5 17H20" /><path d="M6.5 2H20v20H6.5A2.5 2.5 0 0 1 4 19.5v-15A2.5 2.5 0 0 1 6.5 2z" /></>,
  };
  return <svg {...common}>{P[name] || null}</svg>;
}

/* --- Brand logo: simple cluster motif (circles + lines, allowed) --- */
function Logo({ size = 22 }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" aria-hidden="true">
      <circle cx="12" cy="5" r="2.4" fill="currentColor" stroke="none" />
      <circle cx="5" cy="17" r="2.4" fill="currentColor" stroke="none" />
      <circle cx="19" cy="17" r="2.4" fill="currentColor" stroke="none" />
      <path d="M12 7.4L6 14.8M12 7.4l6 7.4M7 17h10" opacity="0.55" />
    </svg>
  );
}

/* --- Formatting, per language ---
   Numbers follow the UI language: "3.471.722" and "2,0 GB" in pt, "3,471,722"
   and "2.0 GB" in en. One place, so a tile and a table never disagree on the
   decimal mark. */
const LOCALE = { pt: "pt-BR", en: "en-US", es: "es-ES" };
function locale(lang) { return LOCALE[lang] || "en-US"; }
function fmtInt(n, lang) { return new Intl.NumberFormat(locale(lang)).format(Math.round(Number(n) || 0)); }
function fmtDec(n, lang, digits = 1) {
  return new Intl.NumberFormat(locale(lang), { minimumFractionDigits: digits, maximumFractionDigits: digits }).format(Number(n) || 0);
}
// Short counts for tight places: 3.47 mi / 12,4 mil.
function fmtShort(n, lang) {
  n = Number(n) || 0;
  const abs = Math.abs(n);
  if (abs >= 1e9) return `${fmtDec(n / 1e9, lang, 2)} ${lang === "en" ? "B" : "bi"}`;
  if (abs >= 1e6) return `${fmtDec(n / 1e6, lang, 2)} ${lang === "en" ? "M" : "mi"}`;
  if (abs >= 1e4) return `${fmtDec(n / 1e3, lang, 1)} ${lang === "en" ? "K" : "mil"}`;
  return fmtInt(n, lang);
}
// Bytes of DATA (what OpenSearch holds): decimal GB/MB, the unit people quote.
function fmtBytes(b, lang) {
  b = Number(b) || 0;
  if (b >= 1e12) return `${fmtDec(b / 1e12, lang, 2)} TB`;
  if (b >= 1e9) return `${fmtDec(b / 1e9, lang, 1)} GB`;
  if (b >= 1e6) return `${fmtDec(b / 1e6, lang, 0)} MB`;
  return `${fmtInt(b / 1e3, lang)} kB`;
}
// A Kubernetes quantity ("2Gi", "1536m", "1g", "512Mi") as CAPACITY, in GiB —
// what a node reserves. Unknown shapes come back as typed.
function fmtQty(q, lang) {
  const m = String(q || "").trim().match(/^([\d.]+)\s*(Gi|Mi|Ki|Ti|G|M|K|T|g|m|k)?$/);
  if (!m) return q || "—";
  const n = parseFloat(m[1]);
  const u = m[2] || "";
  const gib = { Gi: 1, g: 1, G: 1, Mi: 1 / 1024, m: 1 / 1024, M: 1 / 1024, Ki: 1 / 1024 ** 2, k: 1 / 1024 ** 2, K: 1 / 1024 ** 2, Ti: 1024, T: 1024, "": 1 / 1024 ** 3 }[u];
  const v = n * gib;
  if (v >= 1024) return `${fmtDec(v / 1024, lang, 1)} TiB`;
  if (v >= 1) return `${Number.isInteger(v) ? fmtInt(v, lang) : fmtDec(v, lang, 1)} GiB`;
  return `${fmtInt(v * 1024, lang)} MiB`;
}
// A size in GiB (from a tile's math) with the IEC unit people see in Kubernetes.
function fmtGib(v, lang) { return `${Number.isInteger(+v) || Math.abs(v - Math.round(v)) < 0.05 ? fmtInt(v, lang) : fmtDec(v, lang, 1)} GiB`; }

/* --- Info tooltip dot --- */
function InfoTip({ text }) {
  return (
    <span className="info-dot" tabIndex={0}>
      i
      <span className="tip" role="tooltip">{text}</span>
    </span>
  );
}

/* --- Button --- */
function Btn({ variant = "ghost", size, icon, iconR, children, className, ...rest }) {
  const cls = `btn btn-${variant}${size === "lg" ? " btn-lg" : size === "sm" ? " btn-sm" : ""}${className ? ` ${className}` : ""}`;
  return (
    <button className={cls} type="button" {...rest}>
      {icon && <Icon name={icon} size={15} />}
      {children}
      {iconR && <Icon name={iconR} size={15} />}
    </button>
  );
}

/* --- Field wrapper ---
   `htmlFor` + `hintId` associate the label and the hint with the control, so
   screen readers announce both. `error` renders under the field — a toast is a
   duplicate of an error, never the only signal. --- */
function Field({ label, tip, hint, htmlFor, hintId, error, children, style }) {
  return (
    <div className="field" style={style}>
      {label && (
        <label htmlFor={htmlFor}>
          {label}
          {tip && <InfoTip text={tip} />}
        </label>
      )}
      {hint && <div className="hint" id={hintId}>{hint}</div>}
      {children}
      {error && <div className="field-err" role="alert">{error}</div>}
    </div>
  );
}

/* --- A read-only fact rendered where a form field would sit: "3 nodes,
   always". A disabled input says "you could change this"; this says you
   cannot, and why. --- */
function FactField({ label, value, hint }) {
  return (
    <div className="field">
      <label>{label}</label>
      <div className="fact">{value}</div>
      {hint && <div className="hint" style={{ margin: "6px 0 0" }}>{hint}</div>}
    </div>
  );
}

/* --- Form section: title and help on the left, controls on the right. One
   shape for every settings screen, so "which Save saves this?" never comes
   up — a section has its own fields and its own actions. --- */
function FormSection({ title, desc, children, actions, testid, id }) {
  return (
    <section className="form-section" data-testid={testid} id={id}>
      <div className="fs-head">
        <h3>{title}</h3>
        {desc && <p>{desc}</p>}
      </div>
      <div className="fs-body">
        {children}
        {actions && <div className="fs-actions">{actions}</div>}
      </div>
    </section>
  );
}

/* --- Metric bar --- */
function Metric({ label, value, pct }) {
  const tone = pct >= 90 ? "danger" : pct >= 75 ? "warn" : "";
  return (
    <div className="metric">
      <div className="metric-head">
        <span className="lbl">{label}</span>
        <span className="val">{value}</span>
      </div>
      <div className={`bar ${tone}`}>
        <span style={{ width: Math.max(2, Math.min(100, pct)) + "%" }} />
      </div>
    </div>
  );
}

/* --- MiniMeter: a Metric that fits on one line. Same thresholds as .bar, so a
   row and its bar never disagree about what "hot" means. `pct` null = no
   percentage to draw (a value with no known total). --- */
function MiniMeter({ label, value, pct }) {
  const known = pct !== null && pct !== undefined;
  const p = Math.max(0, Math.min(100, pct || 0));
  const tone = known && p >= 90 ? "danger" : known && p >= 75 ? "warn" : "";
  return (
    <div className="mini">
      <div className="mh"><span>{label}</span><b>{value}</b></div>
      <div className={`bar ${tone}`}><span style={{ width: known ? `${Math.max(2, p)}%` : 0 }} /></div>
    </div>
  );
}

/* --- StatTile: one number, one label, one qualifier. The qualifier carries the
   judgement ("receiving", "degraded"), so the number is never interpreted on
   its own. --- */
function StatTile({ label, value, unit, sub, tone, testid }) {
  return (
    <div className="stat-tile" data-testid={testid}>
      <div className="lbl">{label}</div>
      <div className="num" title={String(value)}>{value}{unit && <small>{unit}</small>}</div>
      {sub && <div className={`sub ${tone || ""}`}>{sub}</div>}
    </div>
  );
}

/* --- Gauge: dependency-free SVG ring. --- */
function Gauge({ label, pct, sub, size = 116 }) {
  const p = Math.max(0, Math.min(100, pct || 0));
  const stroke = p >= 90 ? "var(--danger)" : p >= 75 ? "var(--warn)" : "var(--accent)";
  const r = 42, c = 2 * Math.PI * r;
  const off = c * (1 - p / 100);
  return (
    <div className="gauge">
      <svg viewBox="0 0 100 100" width={size} height={size} aria-hidden="true">
        <circle cx="50" cy="50" r={r} fill="none" stroke="var(--surface-3)" strokeWidth="9" />
        <circle cx="50" cy="50" r={r} fill="none" stroke={stroke} strokeWidth="9"
          strokeLinecap="round" strokeDasharray={c} strokeDashoffset={off}
          transform="rotate(-90 50 50)" style={{ transition: "stroke-dashoffset .6s cubic-bezier(.2,.7,.2,1), stroke .3s" }} />
        <text x="50" y="49" textAnchor="middle" dominantBaseline="central"
          style={{ fill: "var(--text)", font: "600 21px var(--font-mono)" }}>{Math.round(p)}%</text>
      </svg>
      <div className="gauge-label">{label}</div>
      {sub && <div className="gauge-sub tnum">{sub}</div>}
    </div>
  );
}

/* --- Clipboard (#141) ---
   The one copy path for every copy button. `navigator.clipboard` is missing on
   a plain-HTTP origin and rejects when the browser denies the write, so it is
   awaited and caught, then the legacy textarea + execCommand path is tried.
   Resolves to whether the text really reached the clipboard. */
async function copyText(text) {
  if (navigator.clipboard && navigator.clipboard.writeText) {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch (e) { /* denied, or the document lost focus: try the legacy path */ }
  }
  return legacyCopy(text);
}

function legacyCopy(text) {
  const ta = document.createElement("textarea");
  ta.value = text;
  ta.setAttribute("readonly", "");
  ta.style.cssText = "position:fixed;top:0;left:0;opacity:0;pointer-events:none";
  const prev = document.activeElement;
  document.body.appendChild(ta);
  ta.select();
  let ok = false;
  try { ok = document.execCommand("copy"); } catch (e) { ok = false; }
  ta.remove();
  if (prev && prev.focus) prev.focus();
  return ok;
}

function selectContents(el) {
  if (!el) return;
  const range = document.createRange();
  range.selectNodeContents(el);
  const sel = window.getSelection();
  sel.removeAllRanges();
  sel.addRange(range);
}

/* --- Copyable ---
   `display` shows a shortened form while the button still copies the full
   `text`. `onCopy(message, ok)` gets the translated outcome. After a failure
   the field shows the full text, selected, and says to press Ctrl+C. --- */
function Copyable({ text, display, t, onCopy, className }) {
  const [state, setState] = useState("idle"); // idle | copied | failed
  const codeRef = useRef(null);
  useEffect(() => { setState("idle"); }, [text]);
  useEffect(() => {
    if (state === "failed") selectContents(codeRef.current);
    if (state !== "copied") return;
    const h = setTimeout(() => setState("idle"), 1500);
    return () => clearTimeout(h);
  }, [state]);

  async function copy() {
    const ok = await copyText(text);
    setState(ok ? "copied" : "failed");
    if (onCopy) onCopy(ok ? t.copied : t.copy_failed, ok);
  }

  const failed = state === "failed";
  return (
    <span className={`copyfield${className ? ` ${className}` : ""}`} data-copy-state={state}>
      <code ref={codeRef} title={display && !failed ? text : undefined}>{failed ? text : (display || text)}</code>
      <button type="button" onClick={copy} title={failed ? t.copy_failed : t.copy} aria-label={t.copy}>
        <Icon name={state === "copied" ? "check" : "copy"} size={13} />
      </button>
      {failed && <span className="copyfield-note" role="status">{t.copy_failed}</span>}
    </span>
  );
}

/* --- Error boundary (#141) --- */
class ErrorBoundary extends Component {
  constructor(props) {
    super(props);
    this.state = { error: null };
  }

  static getDerivedStateFromError(error) {
    return { error };
  }

  componentDidUpdate(prev) {
    if (this.state.error && prev.resetKey !== this.props.resetKey) this.setState({ error: null });
  }

  render() {
    const { error } = this.state;
    if (!error) return this.props.children;
    const t = typeof this.props.t === "function" ? this.props.t() : this.props.t;
    return (
      <div className="card pad" role="alert" data-testid="error-boundary" style={{ maxWidth: 560, margin: "40px auto" }}>
        <h3 style={{ margin: "0 0 6px", fontSize: 16, color: "var(--danger)" }}>{t.err_h}</h3>
        <p className="hint" style={{ marginTop: 0 }}>{t.err_p}</p>
        <pre style={{ whiteSpace: "pre-wrap", wordBreak: "break-word", fontSize: 12.5, fontFamily: "var(--font-mono)", color: "var(--text-2)" }}>
          {String((error && error.message) || error)}
        </pre>
        <Btn variant="primary" onClick={() => window.location.reload()}>{t.err_reload}</Btn>
      </div>
    );
  }
}

/* --- Toast ---
   Four kinds, each with its own glyph and colour, so a failure never wears a
   check mark. A success fades on its own; an error stays until closed — a
   message that explains why a save failed is not read in two seconds. --- */
const TOAST_ICON = { ok: "check", error: "alert", warn: "alert", info: "info" };
function Toast({ msg, show, kind = "ok", onClose, t }) {
  return (
    <div className={`toast ${kind} ${show ? "show" : ""}`} role={kind === "error" ? "alert" : "status"} aria-live={kind === "error" ? "assertive" : "polite"}>
      <Icon name={TOAST_ICON[kind] || "info"} size={15} className={kind === "ok" ? "ok" : kind === "error" ? "bad" : "mid"} />
      <span className="toast-msg">{msg}</span>
      {kind !== "ok" && onClose && (
        <button type="button" className="toast-close" onClick={onClose} aria-label={(t && t.close) || "close"}>
          <Icon name="x" size={13} />
        </button>
      )}
    </div>
  );
}

/* --- Banner: a notice with a tone, inside the flow of the page.
   info (a fact), warn (a risk), danger (a failure), ok (a confirmation).
   `action` sits on the right; `title` is optional and bold. --- */
const BANNER_ICON = { info: "info", warn: "alert", danger: "alert", ok: "check" };
function Banner({ tone = "info", icon, title, children, action, testid, style, className, role }) {
  return (
    <div className={`banner ${tone}${className ? ` ${className}` : ""}`} data-testid={testid} style={style}
      role={role || (tone === "danger" ? "alert" : "status")}>
      <span className="banner-ic"><Icon name={icon || BANNER_ICON[tone]} size={17} /></span>
      <div className="banner-body">
        {title && <div className="banner-title">{title}</div>}
        {children && <div className="banner-text">{children}</div>}
      </div>
      {action && <div className="banner-action">{action}</div>}
    </div>
  );
}

/* --- Dialog: the one modal. Role, label, Esc, focus moved in on open and
   returned on close, Tab kept inside. Confirm and the upgrade dialog are both
   built on it, so keyboard behaviour cannot differ between them. --- */
function Dialog({ open, onClose, title, label, width = 440, children, testid, busy }) {
  const ref = useRef(null);
  const prevFocus = useRef(null);
  const titleId = useRef(`dlg-${Math.random().toString(36).slice(2, 8)}`).current;

  useEffect(() => {
    if (!open) return undefined;
    prevFocus.current = document.activeElement;
    const el = ref.current;
    const focusables = () => Array.from(el ? el.querySelectorAll('input, button, select, textarea, [tabindex]:not([tabindex="-1"]), a[href]') : [])
      .filter(x => !x.disabled && x.offsetParent !== null);
    const first = focusables().find(x => x.hasAttribute("autofocus")) || focusables()[0];
    if (first) first.focus();
    const onKey = (e) => {
      if (e.key === "Escape" && !busy) { e.stopPropagation(); onClose && onClose(); }
      if (e.key === "Tab") {
        const f = focusables();
        if (f.length === 0) return;
        const i = f.indexOf(document.activeElement);
        if (e.shiftKey && (i <= 0)) { e.preventDefault(); f[f.length - 1].focus(); }
        else if (!e.shiftKey && i === f.length - 1) { e.preventDefault(); f[0].focus(); }
      }
    };
    document.addEventListener("keydown", onKey);
    document.body.style.overflow = "hidden";
    return () => {
      document.removeEventListener("keydown", onKey);
      document.body.style.overflow = "";
      if (prevFocus.current && prevFocus.current.focus) prevFocus.current.focus();
    };
  }, [open, busy, onClose]);

  if (!open) return null;
  return (
    <div className="dialog-backdrop" onMouseDown={(e) => { if (e.target === e.currentTarget && !busy && onClose) onClose(); }}>
      <div className="card pad dialog" role="dialog" aria-modal="true"
        aria-labelledby={title ? titleId : undefined} aria-label={!title ? label : undefined}
        style={{ maxWidth: width }} ref={ref} data-testid={testid}>
        {title && <h3 id={titleId} className="dialog-title">{title}</h3>}
        {children}
      </div>
    </div>
  );
}

/* --- Confirm modal ---
   `requireText` turns this into a type-to-confirm dialog: the action stays
   disabled until the user types that exact string. For an irreversible delete
   a single click is too cheap — the point is to make the user name the thing
   they are destroying, which also catches "wrong deployment open in the tab".
   The placeholder is deliberately NOT the expected text: a grey copy of the
   name read as already typed, and the disabled button then read as broken. --- */
function Confirm({ open, title, body, confirmLabel, cancelLabel, onConfirm, onCancel, confirmTestid, cancelTestid, icon = "trash", variant = "danger", requireText, requireLabel, requirePlaceholder, children, busy }) {
  const [typed, setTyped] = useState("");
  useEffect(() => { if (!open) setTyped(""); }, [open]);
  const armed = !requireText || typed.trim() === requireText;
  const tone = variant === "danger" ? "var(--danger)" : "var(--accent)";
  return (
    <Dialog open={open} onClose={onCancel} label={title} width={440} busy={busy}>
      <div style={{ display: "flex", gap: 12, marginBottom: 12 }}>
        <span style={{ color: tone, flexShrink: 0, marginTop: 2 }}><Icon name={icon} size={20} /></span>
        <div style={{ minWidth: 0 }}>
          <h3 className="dialog-title" style={{ margin: "0 0 6px" }}>{title}</h3>
          {body && <p style={{ margin: 0, color: "var(--text-2)", fontSize: 14 }}>{body}</p>}
        </div>
      </div>
      {children}
      {requireText && (
        <div style={{ marginTop: 4 }}>
          {requireLabel && (
            <label htmlFor="confirm-text" style={{ display: "block", fontSize: 13, color: "var(--text-2)", marginBottom: 6 }}>
              {requireLabel}
            </label>
          )}
          <input
            id="confirm-text"
            className="input mono"
            value={typed}
            autoFocus
            autoComplete="off"
            spellCheck="false"
            placeholder={requirePlaceholder || ""}
            onChange={e => setTyped(e.target.value)}
            onKeyDown={e => { if (e.key === "Enter" && armed) onConfirm(); }}
            data-testid="confirm-text"
          />
        </div>
      )}
      <div className="dialog-actions">
        <Btn variant="outline" onClick={onCancel} data-testid={cancelTestid} disabled={busy}>{cancelLabel}</Btn>
        <Btn variant={variant === "danger" ? "danger-solid" : variant} icon={icon} disabled={!armed || busy} onClick={onConfirm} data-testid={confirmTestid}>{confirmLabel}</Btn>
      </div>
    </Dialog>
  );
}

/* --- Tabs: a tablist with arrow-key movement. The container scrolls on a
   narrow screen and fades at the edge so the hidden tabs announce
   themselves; each tab carries a stable test id. --- */
function Tabs({ items, value, onChange, label, testidPrefix = "tab", className }) {
  const ref = useRef(null);
  function onKey(e, i) {
    if (e.key !== "ArrowRight" && e.key !== "ArrowLeft" && e.key !== "Home" && e.key !== "End") return;
    e.preventDefault();
    const n = items.length;
    const j = e.key === "Home" ? 0 : e.key === "End" ? n - 1 : (i + (e.key === "ArrowRight" ? 1 : -1) + n) % n;
    onChange(items[j].key);
    const btn = ref.current && ref.current.querySelectorAll('[role="tab"]')[j];
    if (btn) btn.focus();
  }
  return (
    <div className={`tabs-wrap${className ? ` ${className}` : ""}`}>
      <nav className="tabs" role="tablist" aria-label={label} ref={ref}>
        {items.map((it, i) => (
          <button key={it.key} role="tab" type="button"
            aria-selected={value === it.key}
            tabIndex={value === it.key ? 0 : -1}
            data-testid={`${testidPrefix}-${it.key}`}
            onClick={() => onChange(it.key)}
            onKeyDown={(e) => onKey(e, i)}>
            {it.icon && <Icon name={it.icon} size={14} />}
            {it.label}
            {it.count != null && <span className="tab-count">{it.count}</span>}
          </button>
        ))}
      </nav>
    </div>
  );
}

/* --- Menu: a small dropdown of actions. Click-outside and Esc close it; the
   trigger is a real button with aria-expanded. --- */
function Menu({ label, icon = "dots", items, align = "right", testid, variant = "outline", renderTrigger, className }) {
  const [open, setOpen] = useState(false);
  const ref = useRef(null);
  useEffect(() => {
    if (!open) return undefined;
    const onDoc = (e) => { if (ref.current && !ref.current.contains(e.target)) setOpen(false); };
    const onKey = (e) => { if (e.key === "Escape") setOpen(false); };
    document.addEventListener("mousedown", onDoc);
    document.addEventListener("keydown", onKey);
    return () => { document.removeEventListener("mousedown", onDoc); document.removeEventListener("keydown", onKey); };
  }, [open]);
  const toggle = () => setOpen(o => !o);
  return (
    <div className={`menu${className ? ` ${className}` : ""}`} ref={ref}>
      {renderTrigger
        ? renderTrigger({ open, toggle, "aria-haspopup": "menu", "aria-expanded": open })
        : (
          <Btn variant={variant} icon={icon} aria-haspopup="menu" aria-expanded={open} aria-label={label} title={label}
            onClick={toggle} data-testid={testid} className={items.length === 0 ? "hidden" : ""}>
            {typeof label === "string" && icon !== "dots" ? label : null}
          </Btn>
        )}
      {open && (
        <div className={`menu-list ${align}`} role="menu">
          {items.map((it, i) => it.sep
            ? <div className="menu-sep" key={i} role="separator" />
            : (
              <button key={i} role="menuitem" type="button" className={`menu-item${it.danger ? " danger" : ""}`}
                disabled={it.disabled} data-testid={it.testid} title={it.title}
                onClick={() => { setOpen(false); it.onClick && it.onClick(); }}>
                {it.icon && <Icon name={it.icon} size={14} />}
                <span>{it.label}</span>
                {it.right && <span className="menu-right">{it.right}</span>}
              </button>
            ))}
        </div>
      )}
    </div>
  );
}

/* --- Page header: title, status, one line of facts, up to three actions. The
   same shape on every screen, so the eye learns it once. --- */
function PageHeader({ title, mono, pill, meta, actions, children, lead }) {
  return (
    <div className="page-head">
      <div className="ph-main">
        <h1 className={`page-title${mono ? " mono" : ""}`}>{title}{pill && <span className="ph-pill">{pill}</span>}</h1>
        {meta && <div className="ph-meta">{meta}</div>}
        {lead && <p className="lead">{lead}</p>}
      </div>
      {actions && <div className="ph-actions">{actions}</div>}
      {children}
    </div>
  );
}

/* --- StatusPill: a health as a word AND a colour. The map from the cluster's
   own vocabulary (green / yellow / red) to the user's lives here and nowhere
   else, so "green" never leaks onto a screen. `busy` names the activity
   instead of the colour, which is misleading mid-roll. --- */
function healthTone(health) { return ["green", "yellow", "red"].includes(health) ? health : "unknown"; }
function StatusPill({ health, busy, t, size, testid }) {
  const tone = busy ? "busy" : healthTone(health);
  const word = busy ? busy : (t[`health_${tone}`] || health || "—");
  return (
    <span className={`pill ${tone}${size === "sm" ? " sm" : ""}`} data-testid={testid} data-health={health}>
      <span className="dot" />{word}
    </span>
  );
}

/* --- Empty state: an illustration, one sentence, one action. --- */
function EmptyState({ art, title, body, action, children, testid }) {
  return (
    <div className="empty" data-testid={testid}>
      {art}
      <h2>{title}</h2>
      {body && <p>{body}</p>}
      {action}
      {children}
    </div>
  );
}

export {
  Icon, Logo, InfoTip, Btn, Field, FactField, FormSection, Metric, MiniMeter, StatTile, Gauge,
  copyText, Copyable, ErrorBoundary, Toast, Banner, Dialog, Confirm, Tabs, Menu, PageHeader,
  StatusPill, healthTone, EmptyState,
  locale, fmtInt, fmtDec, fmtShort, fmtBytes, fmtQty, fmtGib,
};
