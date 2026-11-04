// @vitest-environment node
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import {
  FORCED_COLORS,
  ROOT,
  THEMES,
  checkTokens,
  colourTokens,
  isCyan,
  parseHex,
  parseRules,
  pick,
  resolve,
  themeTokens,
} from "../src/theme/tokenGate.ts";

// Read from disk: Vitest serves CSS imports, ?raw included, as empty strings.
const css = readFileSync(new URL("../src/theme/tokens.css", import.meta.url), "utf8");

const rules = parseRules(css);
const root = pick(rules, ROOT);
const px = (name: string, tokens = root) => {
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

describe("TOK-01 token coverage", () => {
  it("defines every colour role in each theme", () => {
    for (const theme of THEMES) {
      const names = colourTokens(themeTokens(rules, theme));
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
    const compact = new Map([...root, ...pick(rules, ':root[data-density="compact"]')]);
    expect(px("row-height", compact)).toBe(32);
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
      expect(isCyan(resolve(themeTokens(rules, theme), "interaction-accent")!), theme.name).toBe(
        true,
      );
    }
    expect(isCyan(parseHex("#16191d")!)).toBe(false);
    expect(isCyan(parseHex("#2f5bc8")!)).toBe(false);
  });

  it("never resolves a status or filament token to cyan", () => {
    for (const theme of THEMES) {
      const tokens = themeTokens(rules, theme);
      const guarded = colourTokens(tokens).filter((n) => /^(status|filament)-/.test(n));
      expect(guarded.length).toBeGreaterThan(0);
      for (const name of guarded)
        expect(isCyan(resolve(tokens, name)!), `${theme.name} --${name}`).toBe(false);
    }
  });

  it("fails a status token that points at an interaction token", () => {
    const planted = `${css}\n:root[data-theme="light"] { --status-info: var(--interaction-accent); }`;
    expect(checkTokens(planted).problems).toContain(
      "light: --status-info is cyan; cyan is for interaction tokens only",
    );
  });

  it("fails a status token mapped to a forced-colors highlight", () => {
    const planted = `${css}\n${FORCED_COLORS} { :root { --status-success: Highlight; } }`;
    expect(checkTokens(planted).problems).toContain(
      "forced colors: --status-success uses Highlight; highlight colours are for interaction tokens only",
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
      "forced colors",
      `${FORCED_COLORS} { :root { --text-muted: Canvas; } }`,
      "forced colors, dark palette: --text-muted on --surface-sunken",
    ],
  ])("fails a planted low-contrast pair (%s)", (_label, plant, expected) => {
    const { problems } = checkTokens(`${css}\n${plant}`);
    expect(problems.some((p) => p.startsWith(expected))).toBe(true);
  });

  it("fails a planted pair through web/scripts/design-system-contrast.mjs --check", () => {
    const script = fileURLToPath(
      new URL("../../../scripts/design-system-contrast.mjs", import.meta.url),
    );
    const scratch = mkdtempSync(join(tmpdir(), "ui-tokens-"));
    try {
      const planted = join(scratch, "tokens.css");
      writeFileSync(planted, `${css}\n:root[data-theme="light"] { --text-muted: #9aa1a8; }`);
      let stderr = "";
      try {
        execFileSync(process.execPath, [script, "--check", "--ui-css", planted], { stdio: "pipe" });
      } catch (error) {
        stderr = String((error as { stderr?: Buffer }).stderr);
      }
      expect(stderr).toMatch(
        /apps\/ui: light: --text-muted on --surface-sunken is [\d.]+:1, below 4\.5:1/,
      );
    } finally {
      rmSync(scratch, { recursive: true, force: true });
    }
  });

  it("fails a colour token with no forced-colors mapping", () => {
    const { problems } = checkTokens(`${css}\n:root { --status-paused: #c29bf0; }`);
    expect(problems).toContain("forced colors: --status-paused is not mapped to a system colour");
  });

  it("fails a duration that reduced motion leaves running", () => {
    const { problems } = checkTokens(`${css}\n:root { --motion-duration-extra: 180ms; }`);
    expect(problems.some((p) => p.startsWith("reduced motion: --motion-duration-extra"))).toBe(
      true,
    );
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
