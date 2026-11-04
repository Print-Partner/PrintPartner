// CSS custom-property parsing and WCAG contrast arithmetic, shared by the replacement UI
// gate (tokenGate.ts) and the old app's gate (web/scripts/design-system-contrast.mjs).
// Node loads this file directly with type stripping, so it has no imports and uses only
// erasable TypeScript syntax. It lives in apps/ui because the package's import boundary
// stops tokenGate.ts from importing anything outside it.

export type Rgb = readonly [number, number, number];

export interface CssRule {
  /** The @media prelude the rule sits in, or null. Other at-rules are transparent. */
  media: string | null;
  selector: string;
  body: string;
}

export function parseRules(css: string): CssRule[] {
  const out: CssRule[] = [];
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
      else out.push({ media, selector: prelude, body });
      i = close + 1;
    }
  };
  walk(css.replace(/\/\*[\s\S]*?\*\//g, ""), null);
  return out;
}

export function declarations(body: string): Map<string, string> {
  const found = new Map<string, string>();
  for (const part of body.split(";")) {
    const colon = part.indexOf(":");
    if (colon >= 0) found.set(part.slice(0, colon).trim(), part.slice(colon + 1).trim());
  }
  return found;
}

function luminance(rgb: Rgb): number {
  const [r, g, b] = rgb.map((v) => (v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4));
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

/** WCAG 2 contrast ratio of two sRGB colours with channels in 0..1. */
export function contrast(a: Rgb, b: Rgb): number {
  const [x, y] = [luminance(a), luminance(b)];
  return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05);
}
