// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
/* ============================================================
   Shared UI primitives + simple line icons
   ============================================================ */
import { Component, useState, useEffect, useRef } from "react";

/* --- Icons: simple stroke line icons --- */
function Icon({ name, size = 16, className, style }) {
  const s = { width: size, height: size, ...style };
  const common = {
    viewBox: "0 0 24 24", fill: "none", stroke: "currentColor",
    strokeWidth: 2, strokeLinecap: "round", strokeLinejoin: "round",
    style: s, className,
  };
  const P = {
    search: <><circle cx="11" cy="11" r="7" /><path d="M21 21l-4.3-4.3" /></>,
    plus: <><path d="M12 5v14M5 12h14" /></>,
    gear: <><circle cx="12" cy="12" r="3" /><path d="M19.4 15a1.6 1.6 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.6 1.6 0 0 0-1.8-.3 1.6 1.6 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.6 1.6 0 0 0-1-1.5 1.6 1.6 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.6 1.6 0 0 0 .3-1.8 1.6 1.6 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.6 1.6 0 0 0 1.5-1 1.6 1.6 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.6 1.6 0 0 0 1.8.3H9a1.6 1.6 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.6 1.6 0 0 0 1 1.5 1.6 1.6 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.6 1.6 0 0 0-.3 1.8V9a1.6 1.6 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.6 1.6 0 0 0-1.5 1z" /></>,
    chevR: <path d="M9 18l6-6-6-6" />,
    chevL: <path d="M15 18l-6-6 6-6" />,
    chevD: <path d="M6 9l6 6 6-6" />,
    check: <path d="M20 6L9 17l-5-5" />,
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
    spark: <path d="M12 2l2 7 7 2-7 2-2 7-2-7-7-2 7-2 2-7z" />,
    clock: <><circle cx="12" cy="12" r="9" /><path d="M12 7v5l3 2" /></>,
    db: <><ellipse cx="12" cy="5" rx="8" ry="3" /><path d="M4 5v6c0 1.7 3.6 3 8 3s8-1.3 8-3V5M4 11v6c0 1.7 3.6 3 8 3s8-1.3 8-3v-6" /></>,
  };
  return <svg {...common}>{P[name] || null}</svg>;
}

/* --- Brand logo: simple cluster motif (circles + lines, allowed) --- */
function Logo({ size = 22 }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6">
      <circle cx="12" cy="5" r="2.4" fill="currentColor" stroke="none" />
      <circle cx="5" cy="17" r="2.4" fill="currentColor" stroke="none" />
      <circle cx="19" cy="17" r="2.4" fill="currentColor" stroke="none" />
      <path d="M12 7.4L6 14.8M12 7.4l6 7.4M7 17h10" opacity="0.55" />
    </svg>
  );
}

/* --- Info tooltip dot --- */
function InfoTip({ text }) {
  return (
    <span className="info-dot" tabIndex={0}>
      i
      <span className="tip">{text}</span>
    </span>
  );
}

/* --- Button --- */
function Btn({ variant = "ghost", size, icon, iconR, children, ...rest }) {
  const cls = `btn btn-${variant}${size === "lg" ? " btn-lg" : ""}`;
  return (
    <button className={cls} {...rest}>
      {icon && <Icon name={icon} size={15} />}
      {children}
      {iconR && <Icon name={iconR} size={15} />}
    </button>
  );
}

/* --- Field wrapper ---
   `htmlFor` + `hintId` associate the label and the hint with the control, so
   screen readers announce both. Both are optional: callers that predate them
   render exactly as before. `error` renders under the field — a toast is a
   duplicate of an error, never the only signal. --- */
function Field({ label, tip, hint, htmlFor, hintId, error, children }) {
  return (
    <div className="field">
      {label && (
        <label htmlFor={htmlFor}>
          {label}
          {tip && <InfoTip text={tip} />}
        </label>
      )}
      {hint && <div className="hint" id={hintId}>{hint}</div>}
      {children}
      {error && (
        <div style={{ color: "var(--danger)", fontSize: 12.5, marginTop: 6, fontFamily: "var(--font-mono)" }}>
          {error}
        </div>
      )}
    </div>
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

/* --- MiniMeter: a Metric that fits on one line, for comparing the same value
   across rows (deployment nodes, cluster nodes). Same thresholds as .bar, so a
   row and its bar never disagree about what "hot" means. --- */
function MiniMeter({ label, value, pct }) {
  const p = Math.max(0, Math.min(100, pct || 0));
  const tone = p >= 90 ? "danger" : p >= 75 ? "warn" : "";
  return (
    <div className="mini">
      <div className="mh"><span>{label}</span><b>{value}</b></div>
      <div className={`bar ${tone}`}><span style={{ width: `${Math.max(2, p)}%` }} /></div>
    </div>
  );
}

/* --- StatTile: one number, one label, one qualifier. The qualifier carries the
   judgement ("recebendo", "degradado"), so the number is never interpreted on
   its own. --- */
function StatTile({ label, value, sub, tone }) {
  return (
    <div className="stat-tile">
      <div className="lbl">{label}</div>
      <div className="num" title={String(value)}>{value}</div>
      {sub && <div className={`sub ${tone || ""}`}>{sub}</div>}
    </div>
  );
}

/* --- Gauge: dependency-free SVG ring (same spirit as the Spark line).
   `pct` 0–100 drives the arc; tone matches the .bar thresholds. `sub` is the
   small caption under the percentage (e.g. "12 / 32 GiB"). --- */
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
   Resolves to whether the text really reached the clipboard: the UI says
   "copied" only on true, never on an attempt. */
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

// Select an element's text so a failed copy still leaves the user one Ctrl+C
// away from it.
function selectContents(el) {
  if (!el) return;
  const range = document.createRange();
  range.selectNodeContents(el);
  const sel = window.getSelection();
  sel.removeAllRanges();
  sel.addRange(range);
}

/* --- Copyable ---
   `display` shows a shortened form (a digest, a commit) while the button still
   copies the full `text`. `onCopy(message, ok)` gets the translated outcome,
   so a caller can hand it straight to a toast. After a failure the field shows
   the full text, selected, and says to press Ctrl+C. */
function Copyable({ text, display, t, onCopy }) {
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
    <span className="copyfield" data-copy-state={state}>
      <code ref={codeRef} title={display && !failed ? text : undefined}>{failed ? text : (display || text)}</code>
      <button type="button" onClick={copy} title={failed ? t.copy_failed : t.copy} aria-label={t.copy}>
        <Icon name={state === "copied" ? "check" : "copy"} size={13} />
      </button>
      {failed && <span className="copyfield-note" role="status">{t.copy_failed}</span>}
    </span>
  );
}

/* --- Error boundary (#141) ---
   One render exception used to blank the whole app. The app shell and each
   view sit inside one of these: a crash replaces only what it wrapped with a
   fallback that says so and offers a reload. `resetKey` clears the error when
   it changes (the user navigated away), so a broken view does not stick.
   `t` is the string table, or a function returning it: the top-level boundary
   sits outside App's language state and reads the choice when it renders. */
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
        <h3 style={{ margin: "0 0 6px", fontFamily: "var(--font-mono)", fontSize: 16, color: "var(--danger)" }}>{t.err_h}</h3>
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
   `ok={false}` marks a failure, so "couldn't copy" never wears a check mark. */
function Toast({ msg, show, ok = true }) {
  return (
    <div className={`toast ${show ? "show" : ""}`}>
      <Icon name={ok ? "check" : "bolt"} size={15} className={ok ? "ok" : "bad"} />
      {msg}
    </div>
  );
}

/* --- Confirm modal --- */
function Confirm({ open, title, body, confirmLabel, cancelLabel, onConfirm, onCancel, confirmTestid, cancelTestid, icon = "trash", variant = "danger", requireText, requireLabel, children }) {
  // `requireText` turns this into a type-to-confirm dialog: the action stays
  // disabled until the user types that exact string. For an irreversible delete
  // a single click is too cheap — the point is to make the user name the thing
  // they are destroying, which also catches "wrong cluster open in the tab".
  const [typed, setTyped] = useState("");
  useEffect(() => { if (!open) setTyped(""); }, [open]);
  if (!open) return null;
  // Destructive by default (delete flows); `icon`/`variant` let a non-destructive
  // confirmation — e.g. applying a config change — avoid showing a trash can.
  const tone = variant === "danger" ? "var(--danger)" : "var(--accent)";
  const armed = !requireText || typed.trim() === requireText;
  return (
    <div style={{
      position: "fixed", inset: 0, zIndex: 300,
      background: "rgb(0 0 0 / 0.55)", backdropFilter: "blur(3px)",
      display: "grid", placeItems: "center", padding: 20,
    }} onClick={onCancel}>
      <div className="card pad" style={{ maxWidth: 420, width: "100%" }} onClick={e => e.stopPropagation()}>
        <div style={{ display: "flex", gap: 12, marginBottom: 12 }}>
          <span style={{ color: tone, flexShrink: 0, marginTop: 2 }}><Icon name={icon} size={20} /></span>
          <div>
            <h3 style={{ margin: "0 0 6px", fontFamily: "var(--font-mono)", fontSize: 16 }}>{title}</h3>
            <p style={{ margin: 0, color: "var(--text-2)", fontSize: 14 }}>{body}</p>
          </div>
        </div>
        {/* Anything the decision needs in front of the user: an option that
            changes what the action does, or a value it just produced. */}
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
              className="input"
              value={typed}
              autoFocus
              autoComplete="off"
              spellCheck="false"
              placeholder={requireText}
              onChange={e => setTyped(e.target.value)}
              onKeyDown={e => { if (e.key === "Enter" && armed) onConfirm(); }}
              data-testid="confirm-text"
              style={{ fontFamily: "var(--font-mono)" }}
            />
          </div>
        )}
        <div style={{ display: "flex", gap: 10, justifyContent: "flex-end", marginTop: 14 }}>
          <Btn variant="outline" onClick={onCancel} data-testid={cancelTestid}>{cancelLabel}</Btn>
          <Btn variant={variant} icon={icon} disabled={!armed} onClick={onConfirm} data-testid={confirmTestid}>{confirmLabel}</Btn>
        </div>
      </div>
    </div>
  );
}

export { Icon, Logo, InfoTip, Btn, Field, Metric, MiniMeter, StatTile, Gauge, copyText, Copyable, ErrorBoundary, Toast, Confirm };
