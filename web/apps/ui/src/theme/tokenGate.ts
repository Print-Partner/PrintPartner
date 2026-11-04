// Contrast and colour-role gate for the replacement UI tokens in tokens.css.
// web/scripts/design-system-contrast.mjs loads this file directly with Node's type
// stripping, so it has no imports and uses only erasable TypeScript syntax.

export type Rgb = readonly [number, number, number];

export interface Rule {
  media: string | null;
  selector: string;
  decls: Map<string, string>;
}

export interface Theme {
  name: string;
  /** Rules merged in order; later rules win. */
  selectors: readonly string[];
  /** Minimum ratio for text inks. Shop floor is read from across a room, so 7:1. */
  textMin: number;
}

export const ROOT = ":root";
export const FORCED_COLORS = "@media (forced-colors: active)";
export const REDUCED_MOTION = "@media (prefers-reduced-motion: reduce)";

export const THEMES: readonly Theme[] = [
  { name: "dark", selectors: [ROOT], textMin: 4.5 },
  { name: "light", selectors: [ROOT, ':root[data-theme="light"]'], textMin: 4.5 },
  { name: "shop-floor", selectors: [ROOT, ':root[data-theme="shop-floor"]'], textMin: 7 },
];

export const SURFACES = ["surface-sunken", "surface-base", "surface-raised", "surface-overlay"];
/** Inks that carry words. Each must reach the theme's text minimum on every surface. */
export const TEXT_INKS = [
  "text",
  "text-muted",
  "interaction-text",
  "status-success",
  "status-warning",
  "status-danger",
  "status-info",
  "status-neutral",
];
/** Control edges and indicators. WCAG 1.4.11 asks for 3:1 on every surface. */
export const UI_INKS = [
  "line-strong",
  "interaction-accent",
  "interaction-focus-ring",
  "filament-swatch-border",
];
/** Ink on a filled background: [ink, fill]. */
export const FILLED_PAIRS: readonly (readonly [string, string])[] = [
  ["interaction-on-accent", "interaction-accent"],
  ["text", "interaction-selection"],
];

/** Only these tokens may be cyan, and only they may use forced-colors highlight keywords. */
export const INTERACTION_PREFIX = "interaction-";
const HIGHLIGHT_KEYWORDS = new Set([
  "highlight",
  "highlighttext",
  "linktext",
  "visitedtext",
  "activetext",
  "accentcolor",
  "accentcolortext",
  "selecteditem",
  "selecteditemtext",
]);

/**
 * Two common forced-colors palettes (Windows contrast themes "Night sky" and
 * "Desert"). The browser picks the real colours; the gate checks that the
 * keyword pairs the tokens use hold up in both.
 */
export const FORCED_PALETTES: Readonly<Record<string, Readonly<Record<string, string>>>> = {
  "forced colors, dark palette": {
    canvas: "#000000",
    canvastext: "#ffffff",
    linktext: "#ffff00",
    graytext: "#3ff23f",
    highlight: "#1aebff",
    highlighttext: "#000000",
    buttonface: "#000000",
    buttontext: "#ffffff",
    buttonborder: "#ffffff",
    field: "#000000",
    fieldtext: "#ffffff",
  },
  "forced colors, light palette": {
    canvas: "#fffaef",
    canvastext: "#3d3d3d",
    linktext: "#1c5e75",
    graytext: "#676767",
    highlight: "#903909",
    highlighttext: "#fff5e3",
    buttonface: "#fffaef",
    buttontext: "#202020",
    buttonborder: "#202020",
    field: "#fffaef",
    fieldtext: "#202020",
  },
};

/** Leaf rules with the @media they sit in. Other at-rules are transparent. */
export function parseRules(css: string): Rule[] {
  const source = css.replace(/\/\*[\s\S]*?\*\//g, "");
  const out: Rule[] = [];
  const walk = (text: string, media: string | null) => {
    let i = 0;
    while (i < text.length) {
      const open = text.indexOf("{", i);
      if (open < 0) break;
      let prelude = text.slice(i, open);
      prelude = prelude
        .slice(prelude.lastIndexOf(";") + 1)
        .replace(/\s+/g, " ")
        .trim();
      let depth = 0;
      let close = open;
      for (; close < text.length; close++) {
        if (text[close] === "{") depth++;
        else if (text[close] === "}" && --depth === 0) break;
      }
      const body = text.slice(open + 1, close);
      if (prelude.startsWith("@")) walk(body, prelude.startsWith("@media") ? prelude : media);
      else {
        const decls = new Map<string, string>();
        for (const part of body.split(";")) {
          const colon = part.indexOf(":");
          if (colon >= 0) decls.set(part.slice(0, colon).trim(), part.slice(colon + 1).trim());
        }
        out.push({ media, selector: prelude, decls });
      }
      i = close + 1;
    }
  };
  walk(source, null);
  return out;
}

export function pick(
  rules: readonly Rule[],
  selector: string,
  media: string | null = null,
): Map<string, string> {
  const merged = new Map<string, string>();
  for (const rule of rules) {
    if (rule.selector === selector && rule.media === media) {
      for (const [k, v] of rule.decls) merged.set(k, v);
    }
  }
  return merged;
}

export function themeTokens(rules: readonly Rule[], theme: Theme): Map<string, string> {
  const merged = new Map<string, string>();
  for (const selector of theme.selectors) {
    for (const [k, v] of pick(rules, selector)) merged.set(k, v);
  }
  return merged;
}

export function parseHex(value: string): Rgb | null {
  const m = /^#([0-9a-f]{3}|[0-9a-f]{6})$/i.exec(value.trim());
  if (!m) return null;
  const hex = m[1].length === 3 ? [...m[1]].map((c) => c + c).join("") : m[1];
  return [0, 2, 4].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255) as unknown as Rgb;
}

/** Follows var() references to a hex colour, or to a system keyword in forced colors. */
export function resolve(
  tokens: Map<string, string>,
  name: string,
  palette?: Record<string, string>,
): Rgb | null {
  const seen = new Set<string>();
  let value = tokens.get(`--${name}`);
  while (value !== undefined) {
    const ref = /^var\(--([\w-]+)\)$/.exec(value);
    if (!ref) break;
    if (seen.has(ref[1])) return null;
    seen.add(ref[1]);
    value = tokens.get(`--${ref[1]}`);
  }
  if (value === undefined) return null;
  if (palette) {
    const system = palette[value.toLowerCase()];
    return system ? parseHex(system) : null;
  }
  return parseHex(value);
}

function luminance(rgb: Rgb): number {
  const [r, g, b] = rgb.map((v) => (v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4));
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

export function contrast(a: Rgb, b: Rgb): number {
  const [x, y] = [luminance(a), luminance(b)];
  return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05);
}

/** Cyan: hue 165 to 200 degrees with HSL saturation of at least 30%. Graphite stays far below that. */
export function isCyan([r, g, b]: Rgb): boolean {
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const delta = max - min;
  if (delta === 0) return false;
  const l = (max + min) / 2;
  const s = delta / (1 - Math.abs(2 * l - 1));
  let h: number;
  if (max === r) h = 60 * (((g - b) / delta) % 6);
  else if (max === g) h = 60 * ((b - r) / delta + 2);
  else h = 60 * ((r - g) / delta + 4);
  if (h < 0) h += 360;
  return s >= 0.3 && h >= 165 && h <= 200;
}

/** Custom properties whose value resolves to a colour in this token set. */
export function colourTokens(tokens: Map<string, string>): string[] {
  return [...tokens.keys()]
    .filter((k) => k.startsWith("--") && resolve(tokens, k.slice(2)) !== null)
    .map((k) => k.slice(2));
}

export interface Measured {
  ink: string;
  on: string;
  ratio: number;
  min: number;
}

export interface GateResult {
  problems: string[];
  /** Per theme or forced palette: every measured pair. */
  tables: { label: string; rows: Measured[] }[];
}

function measure(
  tokens: Map<string, string>,
  textMin: number,
  label: string,
  problems: string[],
  palette?: Record<string, string>,
): Measured[] {
  const rows: Measured[] = [];
  const check = (ink: string, on: string, min: number) => {
    const a = resolve(tokens, ink, palette);
    const b = resolve(tokens, on, palette);
    if (!a || !b) {
      problems.push(`${label}: cannot resolve --${a ? on : ink} to a colour`);
      return;
    }
    const ratio = contrast(a, b);
    rows.push({ ink, on, ratio, min });
    if (ratio < min)
      problems.push(`${label}: --${ink} on --${on} is ${ratio.toFixed(2)}:1, below ${min}:1`);
  };
  for (const ink of TEXT_INKS) for (const s of SURFACES) check(ink, s, textMin);
  for (const ink of UI_INKS) for (const s of SURFACES) check(ink, s, 3);
  for (const [ink, fill] of FILLED_PAIRS) check(ink, fill, textMin);
  return rows;
}

export function checkTokens(css: string): GateResult {
  const rules = parseRules(css);
  const problems: string[] = [];
  const tables: GateResult["tables"] = [];
  const allColours = new Set<string>();

  for (const theme of THEMES) {
    const tokens = themeTokens(rules, theme);
    tables.push({ label: theme.name, rows: measure(tokens, theme.textMin, theme.name, problems) });
    for (const name of colourTokens(tokens)) {
      allColours.add(name);
      const rgb = resolve(tokens, name);
      if (rgb && isCyan(rgb) && !name.startsWith(INTERACTION_PREFIX)) {
        problems.push(`${theme.name}: --${name} is cyan; cyan is for interaction tokens only`);
      }
    }
  }

  // Forced colors: the browser swaps authored colours for system ones. Every colour
  // token must name a system colour so nothing authored leaks through, and only
  // interaction tokens may borrow the highlight family.
  const forced = pick(rules, ROOT, FORCED_COLORS);
  const palettes = Object.entries(FORCED_PALETTES);
  for (const name of allColours) {
    const value = forced.get(`--${name}`);
    if (value === undefined) {
      problems.push(`forced colors: --${name} is not mapped to a system colour`);
      continue;
    }
    const keyword = /^var\(/.test(value) ? null : value.toLowerCase();
    if (keyword !== null && !(keyword in palettes[0][1])) {
      problems.push(`forced colors: --${name} is ${value}, not a known system colour`);
    }
    if (
      keyword !== null &&
      HIGHLIGHT_KEYWORDS.has(keyword) &&
      !name.startsWith(INTERACTION_PREFIX)
    ) {
      problems.push(
        `forced colors: --${name} uses ${value}; highlight colours are for interaction tokens only`,
      );
    }
  }
  for (const [label, palette] of palettes) {
    tables.push({ label, rows: measure(forced, 4.5, label, problems, palette) });
  }

  // Reduced motion: every duration token drops to 0ms.
  const reduced = pick(rules, ROOT, REDUCED_MOTION);
  for (const [key, value] of pick(rules, ROOT)) {
    if (key.startsWith("--motion-duration-") && reduced.get(key) !== "0ms") {
      problems.push(`reduced motion: ${key} is ${value}, must be 0ms under ${REDUCED_MOTION}`);
    }
  }

  return { problems, tables };
}

/** Markdown contrast tables for docs/product/ui-tokens.md. */
export function renderTables(result: GateResult): string {
  const out: string[] = [];
  for (const { label, rows } of result.tables) {
    out.push(`${label[0].toUpperCase()}${label.slice(1)}:`, "");
    out.push(
      "| Ink | sunken | base | raised | overlay | min |",
      "| --- | --- | --- | --- | --- | --- |",
    );
    const byInk = new Map<string, Measured[]>();
    for (const row of rows) byInk.set(row.ink, [...(byInk.get(row.ink) ?? []), row]);
    for (const [ink, measured] of byInk) {
      const onSurface = SURFACES.map((s) => measured.find((m) => m.on === s));
      if (onSurface.every(Boolean)) {
        out.push(
          `| ${ink} | ${onSurface.map((m) => m!.ratio.toFixed(2)).join(" | ")} | ${measured[0].min} |`,
        );
      }
      for (const m of measured.filter((r) => !SURFACES.includes(r.on))) {
        out.push(`| ${ink} on ${m.on} | ${m.ratio.toFixed(2)} | | | | ${m.min} |`);
      }
    }
    out.push("");
  }
  return out.join("\n").trimEnd();
}
