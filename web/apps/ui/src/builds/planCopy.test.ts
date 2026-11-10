import { describe, expect, it } from "vitest";
import { DRAFT_OPEN, HISTORY_EMPTY, NO_DRAFT, PLAN_HELP } from "./planCopy";

// Every source file of the replacement UI, tests excluded, as text.
const sources = import.meta.glob<string>(["../**/*.{ts,tsx}", "!../**/*.test.{ts,tsx}"], {
  query: "?raw",
  import: "default",
  eager: true,
});

// The old app's guide says this; it is not how the replacement UI's drafts work. This scans
// the UI's own source only: text the core sends, such as a workflow action, is not covered.
const STALE = [/no separate acceptance/i, /publishing step/i, /changes save automatically/i];

describe("GRE-1140 Plan copy separates draft autosave from Accept", () => {
  it("says drafts save on their own, Accept applies them, and Accept snapshots first", () => {
    const help = PLAN_HELP.join(" ");
    expect(help).toMatch(/save to a draft as you make them/);
    expect(help).toMatch(/a checkbox saves at once/);
    expect(help).toMatch(
      /a typed field, such as a name or a quantity, saves when you leave it or press Enter/,
    );
    expect(help).toMatch(/does not change until you accept the draft/);
    expect(help).toMatch(
      /Accept first saves a snapshot of the accepted Plan it replaces, when it can/,
    );
    expect(NO_DRAFT).toMatch(/the Build changes only when you accept/);
    expect(DRAFT_OPEN).toMatch(/the Build changes only when you accept/);
    expect(HISTORY_EMPTY).toMatch(/Restore as draft can bring a snapshot back/);
  });

  it("finds no UI source string that says changes apply without Accept", () => {
    const files = Object.entries(sources);
    expect(files.length).toBeGreaterThan(20);
    const hits = files.flatMap(([file, text]) =>
      STALE.filter((pattern) => pattern.test(text)).map((pattern) => `${file}: ${pattern}`),
    );
    expect(hits).toEqual([]);
  });
});
