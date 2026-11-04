// @vitest-environment node
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { parseRules } from "../src/theme/cssTokens.ts";
import {
  FORCED_COLORS,
  THEMES,
  checkTokens,
  colourTokens,
  effectiveTokens,
  isCyan,
  parseHex,
  resolve,
} from "../src/theme/tokenGate.ts";

// Read from disk: Vitest serves CSS imports, ?raw included, as empty strings.
const css = readFileSync(new URL("../src/theme/tokens.css", import.meta.url), "utf8");

const rules = parseRules(css);
const root = effectiveTokens(rules, {});
const themeTokens = (theme: (typeof THEMES)[number]) => effectiveTokens(rules, theme.attrs);
const px = (name: string, tokens = root): number => {
  const value = tokens.get(`--${name}`) ?? "";
  const ref = /^var\(--([\w-]+)\)$/.exec(value);
  if (ref) return px(ref[1], tokens);
  expect(value, `--${name}`).toMatch(/^\d+px$/);
  return Number.parseInt(value, 10);
};
const ms = (name: string) => {
  const value = root.get(`--${name}`) ?? "";
  expect(value, `--${name}`).toMatch(/^\d+ms$/);
  return Number.parseInt(value, 10);
};
const problemsWith = (plant: string) => checkTokens(`${css}\n${plant}`).problems;
const FORCED_ALL = `${FORCED_COLORS} { :root, :root[data-theme]`;

describe("TOK-01 token coverage", () => {
  it("defines every colour role in each theme", () => {
    for (const theme of THEMES) {
      const names = colourTokens(themeTokens(theme));
      for (const role of ["surface-", "text", "line", "interaction-", "status-", "filament-"]) {
        expect(
          names.some((n) => n.startsWith(role)),
          `${theme.name} ${role}`,
        ).toBe(true);
      }
    }
  });

  it("defines IBM Plex type with weights 400, 500, 600 and tabular figures", () => {
    expect(root.get("--font-sans")).toMatch(/^"IBM Plex Sans"/);
    expect(root.get("--font-mono")).toMatch(/^"IBM Plex Mono"/);
    expect(["regular", "medium", "semibold"].map((w) => root.get(`--font-weight-${w}`))).toEqual([
      "400",
      "500",
      "600",
    ]);
    expect(root.get("--font-numeric")).toBe("tabular-nums");
  });

  it("has 40 px comfortable and 32 px compact rows and 44 to 48 px touch targets", () => {
    expect(px("row-comfortable")).toBe(40);
    expect(px("row-compact")).toBe(32);
    expect(px("row-height")).toBe(40);
    expect(px("row-height", effectiveTokens(rules, { "data-density": "compact" }))).toBe(32);
    expect(px("touch-target-min")).toBe(44);
    expect(px("touch-target")).toBeGreaterThanOrEqual(44);
    expect(px("touch-target")).toBeLessThanOrEqual(48);
  });

  it("keeps motion between 120 and 200 ms with an ease-out curve", () => {
    const durations = [...root.keys()].filter((k) => k.startsWith("--motion-duration-"));
    expect(durations.length).toBeGreaterThan(0);
    for (const key of durations) {
      expect(ms(key.slice(2))).toBeGreaterThanOrEqual(120);
      expect(ms(key.slice(2))).toBeLessThanOrEqual(200);
    }
    // Ease-out starts at full speed: the first control point sits on the time axis origin.
    expect(root.get("--motion-ease-out")).toMatch(/^cubic-bezier\(0, 0, [\d.]+, 1\)$/);
  });
});

describe("TOK-02 cyan is interaction only", () => {
  it("detects cyan, so the check below can fail", () => {
    for (const theme of THEMES) {
      expect(isCyan(resolve(themeTokens(theme), "interaction-accent")!), theme.name).toBe(true);
    }
    expect(isCyan(parseHex("#16191d")!)).toBe(false);
    expect(isCyan(parseHex("#2f5bc8")!)).toBe(false);
  });

  it("never resolves a status or filament token to cyan", () => {
    for (const theme of THEMES) {
      const tokens = themeTokens(theme);
      const guarded = colourTokens(tokens).filter((n) => /^(status|filament)-/.test(n));
      expect(guarded.length).toBeGreaterThan(0);
      for (const name of guarded) {
        expect(isCyan(resolve(tokens, name)!), `${theme.name} --${name}`).toBe(false);
      }
    }
  });

  it("fails a status token that points at an interaction token", () => {
    expect(
      problemsWith(':root[data-theme="light"] { --status-info: var(--interaction-accent); }'),
    ).toContain("light: --status-info is cyan; cyan is for interaction tokens only");
  });

  it("fails a status token mapped to a forced-colors highlight", () => {
    expect(problemsWith(`${FORCED_ALL} { --status-success: Highlight; } }`)).toContain(
      "forced colors (dark): --status-success uses Highlight; highlight colours are for interaction tokens only",
    );
  });

  it("follows a forced-colors var() alias to the highlight keyword it ends at", () => {
    expect(
      problemsWith(`${FORCED_ALL} { --status-success: var(--interaction-text); } }`),
    ).toContain(
      "forced colors (dark): --status-success uses LinkText; highlight colours are for interaction tokens only",
    );
  });
});

describe("TOK-03 contrast gate", () => {
  it("passes the shipped tokens in all three themes and forced colors", () => {
    const result = checkTokens(css);
    expect(result.problems).toEqual([]);
    expect(result.tables.map((t) => t.label)).toEqual([
      "dark",
      "light",
      "shop-floor",
      "forced colors, dark palette",
      "forced colors, light palette",
    ]);
  });

  it.each([
    ["dark", ":root { --text-muted: #3a3f45; }", "dark: --text-muted on --surface-sunken"],
    [
      "light",
      ':root[data-theme="light"] { --status-warning: #d0a040; }',
      "light: --status-warning on --surface-sunken",
    ],
    [
      "shop floor at 7:1",
      ':root[data-theme="shop-floor"] { --text-muted: #8a939c; }',
      "shop-floor: --text-muted on --surface-sunken",
    ],
    [
      "control edge",
      ":root { --line-strong: #2c323a; }",
      "dark: --line-strong on --surface-sunken",
    ],
    [
      "accent hover fill",
      ':root[data-theme="light"] { --interaction-accent-hover: #ffffff; }',
      "light: --interaction-on-accent on --interaction-accent-hover",
    ],
    [
      "forced colors",
      `${FORCED_ALL} { --text-muted: Canvas; } }`,
      "forced colors, dark palette: --text-muted on --surface-sunken",
    ],
  ])("fails a planted low-contrast pair (%s)", (_label, plant, expected) => {
    expect(problemsWith(plant).some((p) => p.startsWith(expected))).toBe(true);
  });

  it("runs from web/scripts/design-system-contrast.mjs --check", () => {
    const script = fileURLToPath(
      new URL("../../../scripts/design-system-contrast.mjs", import.meta.url),
    );
    const output = execFileSync(process.execPath, [script, "--check"], { encoding: "utf8" });
    expect(output).toContain("apps/ui tokens: dark, light, shop floor and forced colors pass");
  });

  it("fails a colour token with no forced-colors mapping", () => {
    expect(problemsWith(":root { --status-paused: #c29bf0; }")).toContain(
      "forced colors (dark): --status-paused is #c29bf0, not a system colour",
    );
  });

  it("fails a forced-colors alias that resolves to nothing", () => {
    expect(
      problemsWith(`${FORCED_ALL} { --filament-swatch-empty: var(--missing-token); } }`),
    ).toContain("forced colors (light): --filament-swatch-empty does not resolve to a value");
  });

  it("fails a theme colour that outranks the forced-colors block", () => {
    expect(problemsWith(':root[data-theme="light"] { --text: #000000; }')).toContain(
      "forced colors (light): --text is #000000, not a system colour",
    );
  });

  it("fails forced colors that change with the theme", () => {
    expect(
      problemsWith(`${FORCED_COLORS} { :root[data-theme="light"] { --text-muted: GrayText; } }`),
    ).toContain(
      "forced colors (light): --text-muted is GrayText but CanvasText in dark; forced colors must not depend on the theme",
    );
  });

  it.each([
    [
      "root",
      ":root { --motion-duration-extra: 180ms; }",
      "reduced motion (dark): --motion-duration-extra",
    ],
    [
      "theme override",
      ':root[data-theme="light"] { --motion-duration-base: 160ms; }',
      "reduced motion (light): --motion-duration-base is 160ms",
    ],
    [
      "duration only in a theme",
      ':root[data-theme="shop-floor"] { --motion-duration-pulse: 140ms; }',
      "reduced motion (shop-floor): --motion-duration-pulse",
    ],
  ])("fails a duration that reduced motion leaves running (%s)", (_label, plant, expected) => {
    expect(problemsWith(plant).some((p) => p.startsWith(expected))).toBe(true);
  });
});

describe("TOK-05 filament swatches", () => {
  it("are round chips with a visible border, unlike status pills", () => {
    expect(root.get("--filament-swatch-radius")).toBe("50%");
    expect(px("filament-swatch-border-width")).toBeGreaterThan(0);
    expect(px("filament-swatch-size")).toBeGreaterThanOrEqual(12);
    expect(px("status-pill-radius")).toBeLessThan(px("status-pill-height") / 2);
    // The border reaches 3:1 on every surface; the gate measures it in each theme.
    expect(
      checkTokens(css).tables[0].rows.filter((r) => r.ink === "filament-swatch-border"),
    ).toHaveLength(4);
  });
});
