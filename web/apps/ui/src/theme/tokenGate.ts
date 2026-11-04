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
  "status-positive",
  "status-attention",
  "status-danger",
  "status-active",
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
  ["interaction-on-selection", "interaction-selection"],
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
    if (!rule.atRules.every((a) => media.includes(a))) return;
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

const SUPPORTED_SELECTOR = /^:root(?:\[[\w-]+(?:="[^"]*")?\])*$/;
const SUPPORTED_VALUES: readonly RegExp[] = [
  /^#(?:[0-9a-f]{3}|[0-9a-f]{6})$/i,
  /^var\(--[\w-]+\)$/,
  /^\d+(?:\.\d+)?(?:px|%|ms)?$/,
  /^cubic-bezier\(\s*[\d.]+\s*,\s*[\d.]+\s*,\s*[\d.]+\s*,\s*[\d.]+\s*\)$/,
  /^tabular-nums$/,
  // A font stack: names or quoted names, ending in a generic family.
  /^(?:(?:"[^"]+"|[\w-]+)\s*,\s*)+(?:sans-serif|serif|monospace|system-ui|ui-monospace)$/,
];

/**
 * The gate models a small subset of CSS: :root rules with double-quoted or bare
 * attribute selectors, at top level or directly inside the forced-colors or
 * reduced-motion query, without !important, holding hex colours, var() references
 * and a few non-colour forms. Anything else that sets a custom property fails the
 * gate instead of being skipped, because the gate could not tell what it does.
 */
function checkSupported(rules: readonly CssRule[], problems: string[]) {
  for (const rule of rules) {
    const custom = rule.body
      .split(";")
      .map((part) => [
        part.slice(0, part.indexOf(":")).trim(),
        part.slice(part.indexOf(":") + 1).trim(),
      ])
      .filter(([name]) => name.startsWith("--"));
    if (custom.length === 0) continue;
    const where = [...rule.atRules, rule.selector].join(" ");
    const [context, ...nested] = rule.atRules;
    const forced = context === FORCED_COLORS;
    if (nested.length > 0 || (context !== undefined && !forced && context !== REDUCED_MOTION)) {
      problems.push(
        `unsupported: ${where}: tokens may sit only at top level or directly in ${FORCED_COLORS} or ${REDUCED_MOTION}`,
      );
    }
    for (const part of rule.selector.split(",").map((p) => p.trim())) {
      if (!SUPPORTED_SELECTOR.test(part)) {
        problems.push(
          `unsupported: selector ${part}: use :root, :root[attr] or :root[attr="value"]`,
        );
      }
    }
    for (const [name, value] of custom) {
      if (/!\s*important/i.test(value)) {
        problems.push(
          `unsupported: ${where}: ${name} uses !important, which the gate does not model`,
        );
      } else if (
        !SUPPORTED_VALUES.some((re) => re.test(value)) &&
        !(forced && SYSTEM_KEYWORDS.includes(value.toLowerCase()))
      ) {
        problems.push(
          `unsupported: ${where}: ${name}: ${value} is not a hex colour, var(--token)${forced ? ", system colour" : ""} or known non-colour value`,
        );
      }
    }
  }
}

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

/**
 * Forced colors collapse the surfaces to one system colour, so a selection fill
 * that maps to the same colour makes selected text look unselected.
 */
function checkSelectionVisible(
  label: string,
  forced: Map<string, string>,
  palette: Readonly<Record<string, string>>,
  problems: string[],
) {
  const selection = resolve(forced, "interaction-selection", palette);
  for (const surface of SURFACES) {
    const fill = resolve(forced, surface, palette);
    if (selection && fill && selection.every((v, i) => v === fill[i])) {
      problems.push(
        `${label}: --interaction-selection is the same colour as --${surface}, so selected text looks unselected`,
      );
    }
  }
}

export function checkTokens(css: string): GateResult {
  const rules = parseRules(css);
  const problems: string[] = [];
  const tables: GateResult["tables"] = [];
  const darkForced = effectiveTokens(rules, THEMES[0].attrs, [FORCED_COLORS]);
  checkSupported(rules, problems);

  for (const theme of THEMES) {
    const tokens = effectiveTokens(rules, theme.attrs);
    tables.push({ label: theme.name, rows: measure(tokens, theme.textMin, theme.name, problems) });
    const colours = colourTokens(tokens);
    for (const name of colours) {
      if (isCyan(resolve(tokens, name)!) && !name.startsWith(INTERACTION_PREFIX)) {
        problems.push(`${theme.name}: --${name} is cyan; cyan is for interaction tokens only`);
      }
    }
    // Each accessibility mode alone and both together, as a user can have both on.
    for (const media of [[FORCED_COLORS], [FORCED_COLORS, REDUCED_MOTION]]) {
      const label = `${media.length > 1 ? "forced colors with reduced motion" : "forced colors"} (${theme.name})`;
      const forced = effectiveTokens(rules, theme.attrs, media);
      checkForced(label, colours, forced, darkForced, problems);
    }
    for (const media of [[REDUCED_MOTION], [REDUCED_MOTION, FORCED_COLORS]]) {
      const label = `${media.length > 1 ? "reduced motion with forced colors" : "reduced motion"} (${theme.name})`;
      checkReducedMotion(label, effectiveTokens(rules, theme.attrs, media), problems);
    }
  }

  for (const [label, palette] of Object.entries(FORCED_PALETTES)) {
    tables.push({ label, rows: measure(darkForced, 4.5, label, problems, palette) });
    checkSelectionVisible(label, darkForced, palette, problems);
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
