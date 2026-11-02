import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { scratchDir } from "../test/scratch.js";
import { scanSourceArtifacts } from "./source-artifacts.js";

describe("Source artifact scanning", () => {
  it("indexes uploaded STL, 3MF, and archive files with durable provenance", () => {
    const root = scratchDir("source-artifacts-");
    mkdirSync(join(root, "parts"), { recursive: true });
    writeFileSync(join(root, "parts", "frame.stl"), "solid frame");
    writeFileSync(join(root, "plate.3mf"), "3mf bytes");
    writeFileSync(join(root, "original.zip"), "zip bytes");
    writeFileSync(join(root, "README.md"), "notes");

    expect(scanSourceArtifacts(root)).toEqual([
      expect.objectContaining({ path: "original.zip", format: "zip", printable: false }),
      expect.objectContaining({ path: "parts/frame.stl", format: "stl", printable: true }),
      expect.objectContaining({ path: "plate.3mf", format: "3mf", printable: true }),
    ]);
    expect(
      scanSourceArtifacts(root).every((artifact) => /^[0-9a-f]{64}$/.test(artifact.sha256)),
    ).toBe(true);
  });
});
