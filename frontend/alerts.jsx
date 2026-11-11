// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
/* ============================================================
   alerts.jsx — the alert tray: the notices that STAY on screen, as popups
   in the top-right corner, each one mutable.

   A `Banner` sits in the flow of a page and is read once, where it is
   relevant (a degraded catalog on the Data tab, the Mesh's state on the Mesh
   tab). An *alert* is different: it is true for days (the operator drifted,
   the host cluster keeps one copy of every volume, a provisioning schedule
   is spent) and it is true on every tab. Repeated in the flow it pushes the
   content down and stops being read. So it is a `Popup`: rendered into the
   tray through a portal from wherever the fact is known, stacked in the
   corner, and the operator can **mute** it.

   Muting is a UI preference (like the dismissed checklist), kept in
   localStorage under one key, per alert id. The id carries what the alert
   is about — `operator-drift:<running image>`, `single-copy:<deployment>` —
   so the same fact stays muted and a NEW fact (a different image, another
   deployment) alerts again. Muted alerts that are still true are counted on
   a bell in the tray, from which they are reactivated; a muted alert that
   is no longer true simply has nothing to show.

   Without a tray in the DOM (the bootstrap screen, a test that mounts one
   view) a `Popup` renders in place as a `Banner`, so the fact is never lost.
   ============================================================ */
import { useEffect, useState, useSyncExternalStore } from "react";
import { createPortal } from "react-dom";
import { Banner, Btn, Icon } from "./ui.jsx";

const KEY = "velox-muted-alerts";
const TRAY_ID = "alert-tray";

function readMuted() {
  try {
    const v = JSON.parse(localStorage.getItem(KEY) || "{}");
    return v && typeof v === "object" ? v : {};
  } catch { return {}; }
}
function writeMuted(m) {
  try { localStorage.setItem(KEY, JSON.stringify(m)); } catch { /* storage blocked: muting lasts the session */ }
}

/* One module-level store: which ids are muted, which popups are mounted
   (so the bell can list the muted ones that are still true). Components
   subscribe with useSyncExternalStore and re-render on a version bump. */
let muted = readMuted();
const mounted = new Map(); // id -> { title, tone }
let version = 0;
const listeners = new Set();
function emit() { version += 1; listeners.forEach(l => l()); }
function subscribe(l) { listeners.add(l); return () => listeners.delete(l); }
function useAlertStore() { return useSyncExternalStore(subscribe, () => version, () => version); }

function mute(id) { muted = { ...muted, [id]: Date.now() }; writeMuted(muted); emit(); }
function unmute(id) { const { [id]: _, ...rest } = muted; muted = rest; writeMuted(muted); emit(); }
function unmuteAll() { muted = {}; writeMuted(muted); emit(); }
function isMuted(id) { return Object.prototype.hasOwnProperty.call(muted, id); }

const ICON = { info: "info", warn: "alert", danger: "alert", ok: "check" };

/* An alert. `id` is stable and names the fact (see the header). Everything
   else is the Banner's vocabulary, so swapping one for the other is a
   one-word change where the fact is known. */
function Popup({ id, tone = "warn", icon, title, children, action, testid, t }) {
  useAlertStore();
  useEffect(() => {
    mounted.set(id, { title, tone });
    emit();
    return () => { mounted.delete(id); emit(); };
  }, [id, title, tone]);
  const host = typeof document !== "undefined" ? document.getElementById(TRAY_ID) : null;
  if (!host) {
    return <Banner tone={tone} icon={icon} title={title} action={action} testid={testid} style={{ marginBottom: 16 }}>{children}</Banner>;
  }
  if (isMuted(id)) return null;
  return createPortal(
    <div className={`popup ${tone}`} data-testid={testid} data-alert-id={id} role={tone === "danger" ? "alert" : "status"}>
      <span className="popup-ic"><Icon name={icon || ICON[tone]} size={16} /></span>
      <div className="popup-body">
        {title && <div className="popup-title">{title}</div>}
        {children && <div className="popup-text">{children}</div>}
        {action && <div className="popup-action">{action}</div>}
      </div>
      <button type="button" className="popup-mute" onClick={() => mute(id)} data-testid={`mute-${testid || id}`}
        title={t ? t.alert_mute : "mute"} aria-label={t ? t.alert_mute : "mute"}>
        <Icon name="bellOff" size={14} />
      </button>
    </div>,
    host,
  );
}

/* The tray: the popups' host, the bell for the muted ones, and whatever the
   shell puts after them (the toast), in one column in the corner. */
function AlertTray({ t, children }) {
  useAlertStore();
  const [open, setOpen] = useState(false);
  const mutedHere = [...mounted.entries()].filter(([id]) => isMuted(id));
  useEffect(() => { if (mutedHere.length === 0) setOpen(false); }, [mutedHere.length]);
  return (
    <div className="alert-tray" data-testid="alert-tray">
      <div id={TRAY_ID} className="alert-stack" />
      {mutedHere.length > 0 && (
        <div className="alert-bell-wrap">
          <button type="button" className={`alert-bell${open ? " on" : ""}`} onClick={() => setOpen(o => !o)}
            data-testid="alert-bell" aria-expanded={open} title={t.alert_muted_n.replace("{n}", mutedHere.length)}>
            <Icon name="bell" size={14} />
            <span className="n" data-testid="alert-bell-count">{mutedHere.length}</span>
          </button>
          {open && (
            <div className="popup muted-list" data-testid="alert-muted-list">
              <div className="popup-body">
                <div className="popup-title">{t.alert_muted_h}</div>
                {mutedHere.map(([id, m]) => (
                  <div key={id} className="muted-row" data-testid={`muted-${id}`}>
                    <span className="popup-text">{m.title || id}</span>
                    <Btn variant="ghost" size="sm" onClick={() => unmute(id)} data-testid={`unmute-${id}`}>{t.alert_unmute}</Btn>
                  </div>
                ))}
                {mutedHere.length > 1 && (
                  <div className="popup-action">
                    <Btn variant="outline" size="sm" icon="bell" onClick={unmuteAll} data-testid="alert-unmute-all">{t.alert_unmute_all}</Btn>
                  </div>
                )}
              </div>
            </div>
          )}
        </div>
      )}
      {children}
    </div>
  );
}

export { AlertTray, Popup };
