import { describe, expect, it } from "vitest";
import { latestSyncedAt, processCompatibleWithMachine } from "./printer-profile-assignments.js";

describe("latestSyncedAt", () => {
  it("returns null when all missing", () => {
    expect(latestSyncedAt([null, undefined])).toBeNull();
  });
  it("returns the max ISO timestamp among assigned profiles", () => {
    expect(latestSyncedAt(["2024-08-17T17:11:00.000Z", null, "2024-08-18T12:00:00.000Z"])).toBe(
      "2024-08-18T12:00:00.000Z",
    );
  });
});

describe("processCompatibleWithMachine", () => {
  it("matches when machine name is listed (case-insensitive)", () => {
    expect(processCompatibleWithMachine(JSON.stringify(["Voron 350", "Other"]), "voron 350")).toBe(
      true,
    );
  });
  it("matches bidirectional substring like existing name heuristics", () => {
    expect(processCompatibleWithMachine(JSON.stringify(["Voron 350 0.4"]), "Voron 350")).toBe(true);
  });
  it("returns true when compatible list is empty/null (treat as unrestricted)", () => {
    expect(processCompatibleWithMachine(null, "Voron 350")).toBe(true);
    expect(processCompatibleWithMachine("[]", "Voron 350")).toBe(true);
  });
  it("returns false when list is non-empty and no name matches", () => {
    expect(processCompatibleWithMachine(JSON.stringify(["Bambu X1C"]), "Voron 350")).toBe(false);
  });
});
