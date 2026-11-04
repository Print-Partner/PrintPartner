// Contrast and colour-role gate for the replacement UI tokens in tokens.css.
// web/scripts/design-system-contrast.mjs loads this file directly with Node's type
// stripping, so it uses only erasable TypeScript syntax and imports with a .ts suffix.
import { contrast, parseRules, declarations, type CssRule, type Rgb } from "./cssTokens.ts";

interface Theme {
  name: string;
  /** Root attributes that select the theme. Dark is the default and sets none. */
  attrs: Readonly<Record<string, string>>;
  /** Minimum ratio for text inks. Shop floor is read from across a room, so 7:1. */
  textMin: number;
}

export const FORCED_COLORS = "@media (forced-colors: active)";
const REDUCED_MOTION = "@media (prefers-reduced-motion: reduce)";

export const THEMES: readonly Theme[] = [
  { name: "dark", attrs: {}, textMin: 4.5 },
  { name: "light", attrs: { "data-theme": "light" }, textMin: 4.5 },
  { name: "shop-floor", attrs: { "data-theme": "shop-floor" }, textMin: 7 },
];

const SURFACES = ["surface-sunken", "surface-base", "surface-raised", "surface-overlay"];
/** Inks that carry words. Each must reach the theme's text minimum on every surface. */
const TEXT_INKS = [
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
const UI_INKS = [
  "line-strong",
  "interaction-accent",
  "interaction-focus-ring",
  "filament-swatch-border",
];
/** Ink on a filled background: [ink, fill]. */
const FILLED_PAIRS: readonly (readonly [string, string])[] = [
  ["interaction-on-accent", "interaction-accent"],
  ["interaction-on-accent", "interaction-accent-hover"],
  ["text", "interaction-selection"],
];

/** Only these tokens may be cyan, and only they may use forced-colors highlight keywords. */
const INTERACTION_PREFIX = "interaction-";
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
const FORCED_PALETTES: Readonly<Record<string, Readonly<Record<string, string>>>> = {
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

/**
 * Specificity of a `:root` selector with attribute selectors, as the number of
 * class-level parts, when it matches these root attributes; otherwise null.
 */
function rootMatch(selector: string, attrs: Readonly<Record<string, string>>): number | null {
  const m = /^:root((?:\[[\w-]+(?:="[^"]*")?\])*)$/.exec(selector.trim());
  if (!m) return null;
  let parts = 1;
  for (const [, name, value] of m[1].matchAll(/\[([\w-]+)(?:="([^"]*)")?\]/g)) {
    if (!(name in attrs) || (value !== undefined && attrs[name] !== value)) return null;
    parts++;
  }
  return parts;
}

/**
 * Custom properties in effect on the root element with these attributes, while the
 * given media queries match. Later rules win among equal specificity, as in CSS.
 */
export function effectiveTokens(
  rules: readonly CssRule[],
  attrs: Readonly<Record<string, string>>,
  media: readonly string[] = [],
): Map<string, string> {
  const applying: { specificity: number; index: number; rule: CssRule }[] = [];
  rules.forEach((rule, index) => {
    if (rule.media !== null && !media.includes(rule.media)) return;
    const matches = rule.selector.split(",").map((s) => rootMatch(s, attrs));
    const specificity = Math.max(-1, ...matches.map((m) => m ?? -1));
    if (specificity >= 0) applying.push({ specificity, index, rule });
  });
  applying.sort((a, b) => a.specificity - b.specificity || a.index - b.index);
  const merged = new Map<string, string>();
  for (const { rule } of applying) {
    for (const [k, v] of declarations(rule.body)) if (k.startsWith("--")) merged.set(k, v);
  }
  return merged;
}

export function parseHex(value: string): Rgb | null {
  const m = /^#([0-9a-f]{3}|[0-9a-f]{6})$/i.exec(value.trim());
  if (!m) return null;
  const hex = m[1].length === 3 ? [...m[1]].map((c) => c + c).join("") : m[1];
  return [0, 2, 4].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255) as unknown as Rgb;
}

/** Follows var() references to the final value; null for a missing token or a cycle. */
function terminal(tokens: Map<string, string>, name: string): string | null {
  const seen = new Set([name]);
  let value = tokens.get(`--${name}`);
  while (value !== undefined) {
    const ref = /^var\(--([\w-]+)\)$/.exec(value);
    if (!ref) break;
    if (seen.has(ref[1])) return null;
    seen.add(ref[1]);
    value = tokens.get(`--${ref[1]}`);
  }
  return value ?? null;
}

/** The token's colour: a hex value, or a system keyword looked up in a forced palette. */
export function resolve(
  tokens: Map<string, string>,
  name: string,
  palette?: Readonly<Record<string, string>>,
): Rgb | null {
  const value = terminal(tokens, name);
  if (value === null) return null;
  if (!palette) return parseHex(value);
  const system = palette[value.toLowerCase()];
  return system ? parseHex(system) : null;
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
  return [...tokens.keys()].map((k) => k.slice(2)).filter((name) => resolve(tokens, name) !== null);
}

interface Measured {
  ink: string;
  on: string;
  ratio: number;
  min: number;
}

interface GateResult {
  problems: string[];
  /** Per theme or forced palette: every measured pair. */
  tables: { label: string; rows: Measured[] }[];
}

function measure(
  tokens: Map<string, string>,
  textMin: number,
  label: string,
  problems: string[],
  palette?: Readonly<Record<string, string>>,
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

const SYSTEM_KEYWORDS = Object.keys(Object.values(FORCED_PALETTES)[0]);

/**
 * Forced colors: the browser swaps authored colours for system ones. Every colour
 * token, after following var(), must be a system colour so nothing authored leaks
 * through, only interaction tokens may borrow the highlight family, and the result
 * must match the dark theme's.
 */
function checkForced(
  label: string,
  colours: readonly string[],
  forced: Map<string, string>,
  dark: Map<string, string>,
  problems: string[],
) {
  for (const name of colours) {
    const value = terminal(forced, name);
    const keyword = value?.toLowerCase() ?? "";
    const darkValue = terminal(dark, name);
    if (value === null) problems.push(`${label}: --${name} does not resolve to a value`);
    else if (!SYSTEM_KEYWORDS.includes(keyword)) {
      problems.push(`${label}: --${name} is ${value}, not a system colour`);
    } else if (HIGHLIGHT_KEYWORDS.has(keyword) && !name.startsWith(INTERACTION_PREFIX)) {
      problems.push(
        `${label}: --${name} uses ${value}; highlight colours are for interaction tokens only`,
      );
    } else if (keyword !== darkValue?.toLowerCase()) {
      problems.push(
        `${label}: --${name} is ${value} but ${darkValue} in dark; forced colors must not depend on the theme`,
      );
    }
  }
}

/** Reduced motion: every duration in effect drops to 0ms. */
function checkReducedMotion(label: string, reduced: Map<string, string>, problems: string[]) {
  for (const [key, value] of reduced) {
    if (key.startsWith("--motion-duration-") && value !== "0ms") {
      problems.push(`${label}: ${key} is ${value}, must be 0ms`);
    }
  }
}

export function checkTokens(css: string): GateResult {
  const rules = parseRules(css);
  const problems: string[] = [];
  const tables: GateResult["tables"] = [];
  const darkForced = effectiveTokens(rules, THEMES[0].attrs, [FORCED_COLORS]);

  for (const theme of THEMES) {
    const tokens = effectiveTokens(rules, theme.attrs);
    tables.push({ label: theme.name, rows: measure(tokens, theme.textMin, theme.name, problems) });
    const colours = colourTokens(tokens);
    for (const name of colours) {
      if (isCyan(resolve(tokens, name)!) && !name.startsWith(INTERACTION_PREFIX)) {
        problems.push(`${theme.name}: --${name} is cyan; cyan is for interaction tokens only`);
      }
    }
    const forced = effectiveTokens(rules, theme.attrs, [FORCED_COLORS]);
    checkForced(`forced colors (${theme.name})`, colours, forced, darkForced, problems);
    const reduced = effectiveTokens(rules, theme.attrs, [REDUCED_MOTION]);
    checkReducedMotion(`reduced motion (${theme.name})`, reduced, problems);
  }

  for (const [label, palette] of Object.entries(FORCED_PALETTES)) {
    tables.push({ label, rows: measure(darkForced, 4.5, label, problems, palette) });
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
