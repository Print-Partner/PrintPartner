// Runs every case in fixtures/drivers/bambu/driver/prints.json through the
// Node adapter's statusFromBambuPrint and checks the results against
// node-oracle-status.json, which the Rust driver test
// (rust/crates/pp-drivers/tests/ticket_t_23_bambu.rs) also replays.
// UPDATE_BAMBU_ORACLE=1 rewrites the oracle file instead of checking it.
import { readFileSync, writeFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { statusFromBambuPrint } from "./bambu.js";

const here = new URL("../../../../../../fixtures/drivers/bambu/driver/", import.meta.url);

type Case = { file?: string; print?: Record<string, unknown> };

describe("Bambu driver oracle", () => {
  it("statuses match node-oracle-status.json", () => {
    const cases = JSON.parse(readFileSync(new URL("prints.json", here), "utf8")) as Record<
      string,
      Case
    >;
    const out: Record<string, unknown> = {};
    for (const [name, { file, print }] of Object.entries(cases)) {
      const report = file ? JSON.parse(readFileSync(new URL(file, here), "utf8")) : { print };
      out[name] = statusFromBambuPrint(report.print);
    }
    const text = `${JSON.stringify(out, null, 2)}\n`;
    const oracle = new URL("node-oracle-status.json", here);
    if (process.env.UPDATE_BAMBU_ORACLE === "1") {
      writeFileSync(oracle, text);
      return;
    }
    expect(JSON.parse(text)).toEqual(JSON.parse(readFileSync(oracle, "utf8")));
  });
});
