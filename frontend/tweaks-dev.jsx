// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
/* ============================================================
   Dev-only design tweaks (accent, density, font) — loaded lazily by app.jsx
   when `import.meta.env.DEV`, so the prototyping panel and its postMessage
   channel to an editing host never reach the production bundle.
   ============================================================ */
import { useEffect } from "react";
import { useTweaks, TweaksPanel, TweakSection, TweakColor, TweakRadio, TweakSelect } from "./tweaks-panel.jsx";

const TWEAK_DEFAULTS = /*EDITMODE-BEGIN*/{
  "accent": "#10b981",
  "density": "regular",
  "uiFont": "IBM Plex"
}/*EDITMODE-END*/;

const ACCENT_HUE = {
  "#10b981": 162, // green
  "#3b82f6": 254, // blue
  "#8b5cf6": 292, // violet
  "#06b6d4": 220, // cyan
  "#f97316": 55,  // orange
};

const FONT_STACKS = {
  "IBM Plex": { sans: '"IBM Plex Sans", system-ui, sans-serif', mono: '"IBM Plex Mono", ui-monospace, monospace' },
  "Geist": { sans: '"Space Grotesk", system-ui, sans-serif', mono: '"JetBrains Mono", ui-monospace, monospace' },
  "Mono only": { sans: '"JetBrains Mono", ui-monospace, monospace', mono: '"JetBrains Mono", ui-monospace, monospace' },
};

export default function DevTweaks({ lang }) {
  const [t, setTweak] = useTweaks(TWEAK_DEFAULTS);
  useEffect(() => {
    const r = document.documentElement;
    r.style.setProperty("--accent-h", ACCENT_HUE[t.accent] ?? 162);
    r.style.setProperty("--density", t.density === "compact" ? 0.8 : t.density === "comfy" ? 1.2 : 1);
    const f = FONT_STACKS[t.uiFont] || FONT_STACKS["IBM Plex"];
    r.style.setProperty("--font-sans", f.sans);
    r.style.setProperty("--font-mono", f.mono);
  }, [t.accent, t.density, t.uiFont]);
  const L = (pt, en, es) => (lang === "pt" ? pt : lang === "es" ? es : en);
  return (
    <TweaksPanel title="Tweaks">
      <TweakSection label={L("Aparência", "Appearance", "Apariencia")} />
      <TweakColor label={L("Cor de destaque", "Accent", "Color de acento")} value={t.accent}
        options={["#10b981", "#3b82f6", "#8b5cf6", "#06b6d4", "#f97316"]}
        onChange={v => setTweak("accent", v)} />
      <TweakRadio label={L("Densidade", "Density", "Densidad")} value={t.density}
        options={["compact", "regular", "comfy"]}
        onChange={v => setTweak("density", v)} />
      <TweakSelect label={L("Fonte", "Font", "Fuente")} value={t.uiFont}
        options={["IBM Plex", "Geist", "Mono only"]}
        onChange={v => setTweak("uiFont", v)} />
    </TweaksPanel>
  );
}
