import { createHash } from "node:crypto";
import {
  appendFileSync,
  mkdirSync,
  mkdtempSync,
  renameSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { buffer } from "node:stream/consumers";
import { afterEach, describe, expect, it } from "vitest";
import type { AcceptedOperationalArtifact } from "../db/accepted-plan-operational.js";
import { observeAcceptedSnapshotRoot, openVerifiedAcceptedArtifact } from "./accepted-artifacts.js";

const temporaryRoots: string[] = [];

function fixture() {
  const root = mkdtempSync(join(tmpdir(), "print-partner-accepted-artifact-"));
  temporaryRoots.push(root);
  const reposDir = join(root, "repos");
  const snapshotRoot = join(reposDir, "snapshots", "one");
  mkdirSync(join(snapshotRoot, "parts"), { recursive: true });
  return { reposDir, snapshotRoot };
}

function trackedArtifact(input: {
  snapshotRoot: string;
  relativePath: string;
  expectedSha256: string;
}): AcceptedOperationalArtifact {
  return {
    kind: "tracked",
    sourceId: 11,
    sourceRevisionId: 17,
    snapshotRoot: input.snapshotRoot,
    relativePath: input.relativePath,
    expectedSha256: input.expectedSha256,
  };
}

afterEach(() => {
  for (const root of temporaryRoots.splice(0)) {
    rmSync(root, { recursive: true, force: true });
  }
});

describe("observeAcceptedSnapshotRoot", () => {
  it("observes a contained accepted snapshot directory", () => {
    const { reposDir, snapshotRoot } = fixture();

    expect(observeAcceptedSnapshotRoot({ reposDir, snapshotRoot })).toEqual({
      kind: "available",
    });
  });

  it("fails closed without exposing a resolved path", () => {
    const { reposDir, snapshotRoot } = fixture();
    const missing = join(reposDir, "snapshots", "missing");
    const outside = join(reposDir, "..", "outside-root");
    const rootLink = join(reposDir, "snapshot-link");
    const rootFile = join(reposDir, "snapshot-file");
    mkdirSync(outside);
    symlinkSync(snapshotRoot, rootLink);
    writeFileSync(rootFile, "not a directory");

    expect(observeAcceptedSnapshotRoot({ reposDir, snapshotRoot: missing })).toEqual({
      kind: "unusable",
      reason: "missing",
    });
    expect(observeAcceptedSnapshotRoot({ reposDir, snapshotRoot: outside })).toEqual({
      kind: "unusable",
      reason: "unsafe_path",
    });
    expect(observeAcceptedSnapshotRoot({ reposDir, snapshotRoot: rootLink })).toEqual({
      kind: "unusable",
      reason: "symlink",
    });
    expect(observeAcceptedSnapshotRoot({ reposDir, snapshotRoot: rootFile })).toEqual({
      kind: "unusable",
      reason: "not_file",
    });
  });
});

describe("openVerifiedAcceptedArtifact", () => {
  it("rejects an empty regular artifact before hashing", () => {
    const { reposDir, snapshotRoot } = fixture();
    const empty = Buffer.alloc(0);
    writeFileSync(join(snapshotRoot, "parts", "empty-open.stl"), empty);

    const result = openVerifiedAcceptedArtifact({
      reposDir,
      artifact: trackedArtifact({
        snapshotRoot,
        relativePath: "parts/empty-open.stl",
        expectedSha256: createHash("sha256").update(empty).digest("hex"),
      }),
    });

    expect(result).toEqual({ kind: "unusable", reason: "empty" });
  });

  it("hashes and streams the accepted bytes from one descriptor", async () => {
    const { reposDir, snapshotRoot } = fixture();
    const bytes = Buffer.from("solid verified");
    writeFileSync(join(snapshotRoot, "parts", "verified.stl"), bytes);

    const result = openVerifiedAcceptedArtifact({
      reposDir,
      artifact: trackedArtifact({
        snapshotRoot,
        relativePath: "parts/verified.stl",
        expectedSha256: createHash("sha256").update(bytes).digest("hex"),
      }),
    });

    expect(result.kind).toBe("verified");
    if (result.kind !== "verified") throw new Error("Expected verified artifact");
    expect(await buffer(result.lease.createReadStream())).toEqual(bytes);
    result.lease.close();
    expect(() => result.lease.createReadStream()).toThrow("lease is closed");
  });

  it("accepts the exact size limit and rejects one byte over it", () => {
    const { reposDir, snapshotRoot } = fixture();
    const bytes = Buffer.from("12345");
    writeFileSync(join(snapshotRoot, "parts", "bounded.stl"), bytes);
    const artifact = trackedArtifact({
      snapshotRoot,
      relativePath: "parts/bounded.stl",
      expectedSha256: createHash("sha256").update(bytes).digest("hex"),
    });

    const exact = openVerifiedAcceptedArtifact({ reposDir, artifact, maxBytes: bytes.length });
    expect(exact.kind).toBe("verified");
    if (exact.kind !== "verified") throw new Error("Expected verified artifact");
    expect(exact.lease.size).toBe(bytes.length);
    exact.lease.close();

    expect(
      openVerifiedAcceptedArtifact({ reposDir, artifact, maxBytes: bytes.length - 1 }),
    ).toEqual({ kind: "unusable", reason: "too_large" });
  });

  it("streams the verified descriptor after its path is replaced", async () => {
    const { reposDir, snapshotRoot } = fixture();
    const acceptedBytes = Buffer.from("solid accepted inode");
    const artifactPath = join(snapshotRoot, "parts", "replace.stl");
    writeFileSync(artifactPath, acceptedBytes);
    const result = openVerifiedAcceptedArtifact({
      reposDir,
      artifact: trackedArtifact({
        snapshotRoot,
        relativePath: "parts/replace.stl",
        expectedSha256: createHash("sha256").update(acceptedBytes).digest("hex"),
      }),
    });

    expect(result.kind).toBe("verified");
    if (result.kind !== "verified") throw new Error("Expected verified artifact");
    renameSync(artifactPath, join(snapshotRoot, "parts", "old.stl"));
    writeFileSync(artifactPath, "solid replacement inode");

    expect(await buffer(result.lease.createReadStream())).toEqual(acceptedBytes);
    result.lease.close();
  });

  it("does not stream bytes appended after verification", async () => {
    const { reposDir, snapshotRoot } = fixture();
    const acceptedBytes = Buffer.from("solid accepted extent");
    const artifactPath = join(snapshotRoot, "parts", "append.stl");
    writeFileSync(artifactPath, acceptedBytes);
    const result = openVerifiedAcceptedArtifact({
      reposDir,
      artifact: trackedArtifact({
        snapshotRoot,
        relativePath: "parts/append.stl",
        expectedSha256: createHash("sha256").update(acceptedBytes).digest("hex"),
      }),
    });

    expect(result.kind).toBe("verified");
    if (result.kind !== "verified") throw new Error("Expected verified artifact");
    appendFileSync(artifactPath, " appended after hash");

    expect(result.lease.size).toBe(acceptedBytes.length);
    expect(await buffer(result.lease.createReadStream())).toEqual(acceptedBytes);
    result.lease.close();
  });

  it("rejects a digest mismatch without returning a descriptor lease", () => {
    const { reposDir, snapshotRoot } = fixture();
    writeFileSync(join(snapshotRoot, "parts", "mismatch.stl"), "solid mismatch");

    const result = openVerifiedAcceptedArtifact({
      reposDir,
      artifact: trackedArtifact({
        snapshotRoot,
        relativePath: "parts/mismatch.stl",
        expectedSha256: createHash("sha256").update("other bytes").digest("hex"),
      }),
    });

    expect(result).toEqual({ kind: "unusable", reason: "digest_mismatch" });
  });

  it("preserves unavailable evidence without probing the filesystem", () => {
    const result = openVerifiedAcceptedArtifact({
      reposDir: join(tmpdir(), "missing-repositories-root"),
      artifact: { kind: "unavailable", reason: "legacy" },
    });

    expect(result).toEqual({ kind: "unavailable", reason: "legacy" });
  });
});
