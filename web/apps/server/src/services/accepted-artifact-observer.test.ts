import { mkdirSync, mkdtempSync, readdirSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, expect, it, vi } from "vitest";
import { createAcceptedArtifactObserver } from "./accepted-artifacts.js";

vi.mock("node:fs", async (importOriginal) => {
  const actual = await importOriginal<typeof import("node:fs")>();
  return { ...actual, readdirSync: vi.fn(actual.readdirSync) };
});

const roots: string[] = [];
afterEach(() => {
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true });
  vi.clearAllMocks();
});

it("scans shared directories once per observation batch, but rechecks on the next batch", () => {
  const reposDir = mkdtempSync(join(tmpdir(), "pp-observation-"));
  roots.push(reposDir);
  const snapshotRoot = join(reposDir, "snapshot");
  mkdirSync(join(snapshotRoot, "parts"), { recursive: true });
  for (const name of ["a.stl", "b.stl"]) writeFileSync(join(snapshotRoot, "parts", name), "solid");
  const input = (name: string) => ({
    reposDir,
    artifact: {
      kind: "tracked" as const,
      sourceId: 1,
      sourceRevisionId: 1,
      snapshotRoot,
      relativePath: `parts/${name}`,
      expectedSha256: "a".repeat(64),
    },
  });
  const observe = createAcceptedArtifactObserver();
  expect(observe(input("a.stl"))).toEqual({ kind: "available" });
  expect(observe(input("b.stl"))).toEqual({ kind: "available" });
  expect(readdirSync).toHaveBeenCalledTimes(2);
  writeFileSync(join(snapshotRoot, "parts", "c.stl"), "new model");
  expect(createAcceptedArtifactObserver()(input("c.stl"))).toEqual({ kind: "available" });
  expect(readdirSync).toHaveBeenCalledTimes(4);
});

it("does not cache file safety checks within a batch", () => {
  const reposDir = mkdtempSync(join(tmpdir(), "pp-observation-"));
  roots.push(reposDir);
  const snapshotRoot = join(reposDir, "snapshot");
  mkdirSync(snapshotRoot);
  const file = join(snapshotRoot, "a.stl");
  writeFileSync(file, "solid");
  const input = {
    reposDir,
    artifact: {
      kind: "tracked" as const,
      sourceId: 1,
      sourceRevisionId: 1,
      snapshotRoot,
      relativePath: "a.stl",
      expectedSha256: "a".repeat(64),
    },
  };
  const observe = createAcceptedArtifactObserver();
  expect(observe(input)).toEqual({ kind: "available" });
  rmSync(file);
  symlinkSync(join(reposDir, "outside.stl"), file);
  expect(observe(input)).toEqual({ kind: "unusable", reason: "symlink" });
});
