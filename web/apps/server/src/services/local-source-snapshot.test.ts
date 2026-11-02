import { createHash } from "node:crypto";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { lstat, mkdir, readdir, rename, rm, writeFile } from "node:fs/promises";
import * as fsPromises from "node:fs/promises";
import {
  beginSourceSyncAdmission,
  packagedSourceSyncManifestPath,
  requireSourceSyncParticipant,
} from "../ownership/source-sync-admission.js";
vi.mock("node:fs/promises", { spy: true });
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { Readable } from "node:stream";
import { afterEach, describe, expect, it, vi } from "vitest";
import { runWithTenantDiskQuota, TenantDiskQuotaError } from "../lib/tenant-disk-quota.js";
import {
  LocalSourceSnapshotStore,
  MAX_SOURCE_SNAPSHOT_BYTES,
  SOURCE_SNAPSHOT_MANIFEST_FILE,
  releaseSharedSnapshot,
  sharedSnapshotHolderCount,
  sourceRelativePath,
  type SnapshotFile,
  type SnapshotSelection,
} from "./local-source-snapshot.js";

vi.mock("node:fs/promises", { spy: true });

const roots: string[] = [];

function tempReposDir(): string {
  const root = mkdtempSync(join(tmpdir(), "pp-source-snapshots-"));
  roots.push(root);
  return root;
}

function file(
  path: string,
  kind: SnapshotFile["kind"],
  content: string,
): {
  descriptor: SnapshotFile;
  content: Buffer;
} {
  const buffer = Buffer.from(content);
  return {
    descriptor: {
      path: sourceRelativePath(path),
      kind,
      sizeHintBytes: buffer.byteLength,
    },
    content: buffer,
  };
}

function response(content: Buffer, contentLengthBytes = content.byteLength) {
  return { stream: Readable.from(content), contentLengthBytes };
}

function selection(omittedFiles: SnapshotSelection["omittedFiles"] = []): SnapshotSelection {
  return {
    maxStlFiles: 500,
    maxDocumentationBytes: 25 * 1024 * 1024,
    omittedFiles,
  };
}

async function candidateNames(reposDir: string, sourceId: number): Promise<string[]> {
  const revisionRoot = join(reposDir, String(sourceId), "revisions");
  if (!existsSync(revisionRoot)) return [];
  return (await readdir(revisionRoot)).filter((name) => name.startsWith(".candidate-"));
}

afterEach(async () => {
  vi.restoreAllMocks();
  await Promise.all(roots.splice(0).map((root) => rm(root, { recursive: true, force: true })));
});

describe("LocalSourceSnapshotStore", () => {
  it("does not create a candidate after admission changes during revision-root creation", async () => {
    const reposDir = tempReposDir();
    const manifest = join(reposDir, "ownership.json");
    const original = readFileSync(packagedSourceSyncManifestPath(), "utf8");
    const previous = process.env.PP_SOURCE_SYNC_OWNERSHIP_MANIFEST;
    const actualFs = await vi.importActual<typeof fsPromises>("node:fs/promises");
    const candidates: string[] = [];
    try {
      writeFileSync(manifest, original);
      process.env.PP_SOURCE_SYNC_OWNERSHIP_MANIFEST = manifest;
      beginSourceSyncAdmission(reposDir);
      vi.mocked(fsPromises.mkdir).mockImplementationOnce(async (...args) => {
        const result = await actualFs.mkdir(...args);
        writeFileSync(manifest, `${original}\n`);
        return result;
      });
      vi.mocked(fsPromises.mkdtemp).mockImplementation(async (...args) => {
        candidates.push(String(args[0]));
        return actualFs.mkdtemp(...args);
      });
      const part = file("cube.stl", "stl", "solid cube");
      await expect(
        new LocalSourceSnapshotStore({ reposDir }).materialize({
          sourceId: 1,
          upstreamRevisionKey: "candidate-admission",
          files: [part.descriptor],
          selection: selection(),
          beforePublish: () => requireSourceSyncParticipant("job.sync"),
          openFile: async () => response(part.content),
        }),
      ).rejects.toThrow(/runtime manifest does not match/);
      expect(candidates).toEqual([]);
      expect(await candidateNames(reposDir, 1)).toEqual([]);
    } finally {
      if (previous === undefined) delete process.env.PP_SOURCE_SYNC_OWNERSHIP_MANIFEST;
      else process.env.PP_SOURCE_SYNC_OWNERSHIP_MANIFEST = previous;
    }
  });

  it("does not consume snapshot content after admission changes while opening a file", async () => {
    const reposDir = tempReposDir();
    const manifest = join(reposDir, "ownership.json");
    const original = readFileSync(packagedSourceSyncManifestPath(), "utf8");
    const previous = process.env.PP_SOURCE_SYNC_OWNERSHIP_MANIFEST;
    const readContent = vi.fn();
    const part = file("cube.stl", "stl", "solid cube");
    const stream = Readable.from(
      (async function* () {
        readContent();
        yield part.content;
      })(),
    );
    try {
      writeFileSync(manifest, original);
      process.env.PP_SOURCE_SYNC_OWNERSHIP_MANIFEST = manifest;
      beginSourceSyncAdmission(reposDir);
      await expect(
        new LocalSourceSnapshotStore({ reposDir }).materialize({
          sourceId: 1,
          upstreamRevisionKey: "open-file-admission",
          files: [part.descriptor],
          selection: selection(),
          beforePublish: () => requireSourceSyncParticipant("job.sync"),
          openFile: async () => {
            await Promise.resolve();
            writeFileSync(manifest, `${original}\n`);
            return { stream, contentLengthBytes: part.content.length };
          },
        }),
      ).rejects.toThrow(/runtime manifest does not match/);
      expect(readContent).not.toHaveBeenCalled();
      expect(stream.destroyed).toBe(true);
      expect(await candidateNames(reposDir, 1)).toEqual([]);
      expect(existsSync(join(reposDir, "1/revisions/open-file-admission"))).toBe(false);
    } finally {
      stream.destroy();
      if (previous === undefined) delete process.env.PP_SOURCE_SYNC_OWNERSHIP_MANIFEST;
      else process.env.PP_SOURCE_SYNC_OWNERSHIP_MANIFEST = previous;
    }
  });

  it("does not write snapshot metadata after admission changes during the final stream", async () => {
    const reposDir = tempReposDir();
    const manifest = join(reposDir, "ownership.json");
    const original = readFileSync(packagedSourceSyncManifestPath(), "utf8");
    const previous = process.env.PP_SOURCE_SYNC_OWNERSHIP_MANIFEST;
    const actualFs = await vi.importActual<typeof fsPromises>("node:fs/promises");
    const metadataWrites: string[] = [];
    try {
      writeFileSync(manifest, original);
      process.env.PP_SOURCE_SYNC_OWNERSHIP_MANIFEST = manifest;
      beginSourceSyncAdmission(reposDir);
      vi.mocked(fsPromises.writeFile).mockImplementation(async (...args) => {
        if (String(args[0]).endsWith(SOURCE_SNAPSHOT_MANIFEST_FILE))
          metadataWrites.push(String(args[0]));
        return actualFs.writeFile(...args);
      });
      const store = new LocalSourceSnapshotStore({ reposDir });
      const part = file("cube.stl", "stl", "solid cube");
      await expect(
        store.materialize({
          sourceId: 1,
          upstreamRevisionKey: "admission-stream",
          files: [part.descriptor],
          selection: selection(),
          beforePublish: () => requireSourceSyncParticipant("job.sync"),
          openFile: async () => ({
            contentLengthBytes: part.content.length,
            stream: Readable.from(
              (async function* () {
                yield part.content;
                writeFileSync(manifest, `${original}\n`);
              })(),
            ),
          }),
        }),
      ).rejects.toThrow(/runtime manifest does not match/);
      expect(metadataWrites).toEqual([]);
      expect(await candidateNames(reposDir, 1)).toEqual([]);
      expect(existsSync(join(reposDir, "1/revisions/admission-stream"))).toBe(false);
    } finally {
      if (previous === undefined) delete process.env.PP_SOURCE_SYNC_OWNERSHIP_MANIFEST;
      else process.env.PP_SOURCE_SYNC_OWNERSHIP_MANIFEST = previous;
    }
  });

  it("publishes a canonical snapshot by renaming a candidate inside the revision root", async () => {
    const reposDir = tempReposDir();
    const observedRenames: Array<{ from: string; to: string }> = [];
    const store = new LocalSourceSnapshotStore({
      reposDir,
      dependencies: {
        renameDirectory: async (from, to) => {
          observedRenames.push({ from, to });
          await rename(from, to);
        },
      },
    });
    const bracket = file("parts/bracket.stl", "stl", "solid bracket");
    const readme = file("README.md", "readme", "# Build notes\n");
    const contents = new Map([
      [bracket.descriptor.path, bracket.content],
      [readme.descriptor.path, readme.content],
    ]);

    const result = await store.materialize({
      sourceId: 42,
      upstreamRevisionKey: "abc123def456",
      files: [bracket.descriptor, readme.descriptor],
      selection: selection(),
      openFile: async (descriptor) => response(contents.get(descriptor.path) ?? Buffer.alloc(0)),
    });

    expect(result.publication).toBe("created");
    expect(result.snapshotLocator).toBe("42/revisions/abc123def456");
    expect(result.absolutePath).toBe(join(reposDir, "42", "revisions", "abc123def456"));
    expect(observedRenames).toHaveLength(1);
    const candidatePrefix = join(reposDir, "42", "revisions", ".candidate-abc123def456-");
    expect(observedRenames[0]?.from.startsWith(candidatePrefix)).toBe(true);
    expect(observedRenames[0]?.to).toBe(result.absolutePath);
    expect(readFileSync(join(result.absolutePath, "parts/bracket.stl"), "utf8")).toBe(
      "solid bracket",
    );
    const manifestText = readFileSync(
      join(result.absolutePath, SOURCE_SNAPSHOT_MANIFEST_FILE),
      "utf8",
    );
    const expectedDigest = createHash("sha256").update(JSON.stringify(result.files)).digest("hex");
    expect(result.manifestDigest).toBe(expectedDigest);
    expect(manifestText).toContain(`"manifestDigest": "${expectedDigest}"`);
    expect(await candidateNames(reposDir, 42)).toEqual([]);
  });

  it("keeps prior revisions immutable when files are deleted or renamed upstream", async () => {
    const reposDir = tempReposDir();
    const store = new LocalSourceSnapshotStore({ reposDir });
    const oldPart = file("parts/old-name.stl", "stl", "old geometry");
    const retained = file("parts/retained.stl", "stl", "same geometry");
    const renamed = file("parts/new-name.stl", "stl", "new geometry");

    const first = await store.materialize({
      sourceId: 8,
      upstreamRevisionKey: "revision-a",
      files: [oldPart.descriptor, retained.descriptor],
      selection: selection(),
      openFile: async (descriptor) =>
        response(descriptor.path === oldPart.descriptor.path ? oldPart.content : retained.content),
    });
    const second = await store.materialize({
      sourceId: 8,
      upstreamRevisionKey: "revision-b",
      files: [renamed.descriptor, retained.descriptor],
      selection: selection(),
      openFile: async (descriptor) =>
        response(descriptor.path === renamed.descriptor.path ? renamed.content : retained.content),
    });

    expect(existsSync(join(first.absolutePath, "parts/old-name.stl"))).toBe(true);
    expect(existsSync(join(first.absolutePath, "parts/new-name.stl"))).toBe(false);
    expect(existsSync(join(second.absolutePath, "parts/old-name.stl"))).toBe(false);
    expect(existsSync(join(second.absolutePath, "parts/new-name.stl"))).toBe(true);
    expect(readFileSync(join(first.absolutePath, "parts/retained.stl"), "utf8")).toBe(
      "same geometry",
    );
  });

  it("derives one file replacement while preserving the selected content policy", async () => {
    const reposDir = tempReposDir();
    const store = new LocalSourceSnapshotStore({ reposDir });
    const part = file("parts/part.stl", "stl", "solid part");
    const omittedReadme = file("README.md", "readme", "omitted notes");
    const baseSelection = selection([
      {
        path: omittedReadme.descriptor.path,
        kind: "readme",
        sizeHintBytes: omittedReadme.content.byteLength,
        reason: "documentation-byte-budget",
      },
    ]);
    const first = await store.materialize({
      sourceId: 81,
      upstreamRevisionKey: "commit-a",
      files: [part.descriptor],
      selection: baseSelection,
      openFile: async () => response(part.content),
    });

    const derived = await store.deriveFileReplacement({
      sourceId: 81,
      baseRevisionKey: "commit-a",
      sourceVersion: "commit-a",
      replacement: {
        path: sourceRelativePath("print-partner.manifest.yaml"),
        kind: "artifact",
        content: Buffer.from("format: print-partner-manifest-v2\nversion: 2\n"),
      },
    });

    expect(derived.upstreamRevisionKey).not.toBe(first.upstreamRevisionKey);
    expect(derived.selection).toEqual(first.selection);
    expect(derived.files.map((entry) => entry.path)).toEqual([
      "parts/part.stl",
      "print-partner.manifest.yaml",
    ]);
    expect(existsSync(join(first.absolutePath, "print-partner.manifest.yaml"))).toBe(false);
    expect(readFileSync(join(first.absolutePath, "parts/part.stl"), "utf8")).toBe("solid part");
  });

  it("keeps derived snapshots distinct when equal content has different selection metadata", async () => {
    const reposDir = tempReposDir();
    const store = new LocalSourceSnapshotStore({ reposDir });
    const part = file("parts/part.stl", "stl", "solid part");
    const firstSelection = selection();
    const secondSelection = {
      ...selection(),
      maxDocumentationBytes: 1024,
    };
    for (const [upstreamRevisionKey, snapshotSelection] of [
      ["commit-a", firstSelection],
      ["commit-b", secondSelection],
    ] as const) {
      await store.materialize({
        sourceId: 82,
        upstreamRevisionKey,
        files: [part.descriptor],
        selection: snapshotSelection,
        openFile: async () => response(part.content),
      });
    }

    const replacement = {
      path: sourceRelativePath("print-partner.manifest.yaml"),
      kind: "artifact" as const,
      content: Buffer.from("format: print-partner-manifest-v2\nversion: 2\n"),
    };
    const firstDerived = await store.deriveFileReplacement({
      sourceId: 82,
      baseRevisionKey: "commit-a",
      sourceVersion: "upstream-a",
      replacement,
    });
    const secondDerived = await store.deriveFileReplacement({
      sourceId: 82,
      baseRevisionKey: "commit-b",
      sourceVersion: "upstream-a",
      replacement,
    });
    const nextUpstreamDerived = await store.deriveFileReplacement({
      sourceId: 82,
      baseRevisionKey: "commit-a",
      sourceVersion: "upstream-b",
      replacement,
    });

    expect(secondDerived.upstreamRevisionKey).not.toBe(firstDerived.upstreamRevisionKey);
    expect(nextUpstreamDerived.upstreamRevisionKey).not.toBe(firstDerived.upstreamRevisionKey);
    expect(firstDerived.selection).toEqual(firstSelection);
    expect(secondDerived.selection).toEqual(secondSelection);
  });

  it("removes a failed candidate and leaves the previous revision intact", async () => {
    const reposDir = tempReposDir();
    const store = new LocalSourceSnapshotStore({ reposDir });
    const original = file("part.stl", "stl", "complete");
    const first = await store.materialize({
      sourceId: 9,
      upstreamRevisionKey: "stable-revision",
      files: [original.descriptor],
      selection: selection(),
      openFile: async () => response(original.content),
    });
    const broken = file("part.stl", "stl", "partial download");
    const failingStream = Readable.from(
      (async function* streamThenFail() {
        yield broken.content.subarray(0, 4);
        throw new Error("network disconnected");
      })(),
    );

    await expect(
      store.materialize({
        sourceId: 9,
        upstreamRevisionKey: "broken-revision",
        files: [broken.descriptor],
        selection: selection(),
        openFile: async () => ({
          stream: failingStream,
          contentLengthBytes: broken.content.byteLength,
        }),
      }),
    ).rejects.toThrow("network disconnected");

    expect(readFileSync(join(first.absolutePath, "part.stl"), "utf8")).toBe("complete");
    expect(existsSync(join(reposDir, "9", "revisions", "broken-revision"))).toBe(false);
    expect(await candidateNames(reposDir, 9)).toEqual([]);
  });

  it("rejects a response-length mismatch without publishing", async () => {
    const reposDir = tempReposDir();
    const store = new LocalSourceSnapshotStore({ reposDir });

    await expect(
      store.materialize({
        sourceId: 10,
        upstreamRevisionKey: "short-download",
        files: [
          {
            path: sourceRelativePath("part.stl"),
            kind: "stl",
            sizeHintBytes: 500,
          },
        ],
        selection: selection(),
        openFile: async () => ({
          stream: Readable.from(Buffer.from("short")),
          contentLengthBytes: 100,
        }),
      }),
    ).rejects.toThrow("expected 100, received 5");

    expect(existsSync(join(reposDir, "10", "revisions", "short-download"))).toBe(false);
    expect(await candidateNames(reposDir, 10)).toEqual([]);
  });

  it("blocks quota exhaustion before publishing a source snapshot", async () => {
    const reposDir = tempReposDir();
    const store = new LocalSourceSnapshotStore({ reposDir });
    const part = file("part.stl", "stl", "solid quota");

    await expect(
      runWithTenantDiskQuota(
        {
          dataDir: reposDir,
          reposDir,
          tenantId: "tenant-a",
          quotaBytes: part.content.byteLength - 1,
          sourceIds: () => [16],
        },
        () =>
          store.materialize({
            sourceId: 16,
            upstreamRevisionKey: "quota-blocked",
            files: [part.descriptor],
            selection: selection(),
            openFile: async () => response(part.content),
          }),
      ),
    ).rejects.toBeInstanceOf(TenantDiskQuotaError);

    expect(existsSync(join(reposDir, "16", "revisions", "quota-blocked"))).toBe(false);
    expect(await candidateNames(reposDir, 16)).toEqual([]);
  });

  it.each([
    ["invalid", -1, "non-negative safe integer"],
    ["oversized", MAX_SOURCE_SNAPSHOT_BYTES + 1, "stored-content limit"],
  ])(
    "closes a response stream with an %s declared length",
    async (label, contentLengthBytes, errorMessage) => {
      const reposDir = tempReposDir();
      const store = new LocalSourceSnapshotStore({ reposDir });
      const stream = Readable.from(Buffer.from("unread"));

      await expect(
        store.materialize({
          sourceId: 103,
          upstreamRevisionKey: `${label}-response`,
          files: [
            {
              path: sourceRelativePath("part.stl"),
              kind: "stl",
              sizeHintBytes: null,
            },
          ],
          selection: selection(),
          openFile: async () => ({
            stream,
            contentLengthBytes,
          }),
        }),
      ).rejects.toThrow(errorMessage);

      expect(stream.destroyed).toBe(true);
      expect(await candidateNames(reposDir, 103)).toEqual([]);
    },
  );

  it("enforces the documentation budget using actual bytes when tree size is unknown", async () => {
    const reposDir = tempReposDir();
    const store = new LocalSourceSnapshotStore({ reposDir });
    const manual = file("manual.pdf", "pdf", "larger than budget");

    await expect(
      store.materialize({
        sourceId: 101,
        upstreamRevisionKey: "unknown-doc-size",
        files: [{ ...manual.descriptor, sizeHintBytes: null }],
        selection: { ...selection(), maxDocumentationBytes: 4 },
        openFile: async () => response(manual.content),
      }),
    ).rejects.toThrow("documentation exceeds");

    expect(existsSync(join(reposDir, "101", "revisions", "unknown-doc-size"))).toBe(false);
    expect(await candidateNames(reposDir, 101)).toEqual([]);
  });

  it("enforces the STL selection limit at the storage boundary", async () => {
    const reposDir = tempReposDir();
    const store = new LocalSourceSnapshotStore({ reposDir });
    const first = file("first.stl", "stl", "one");
    const second = file("second.stl", "stl", "two");

    await expect(
      store.materialize({
        sourceId: 102,
        upstreamRevisionKey: "too-many-stls",
        files: [first.descriptor, second.descriptor],
        selection: { ...selection(), maxStlFiles: 1 },
        openFile: async (descriptor) =>
          response(descriptor.path === first.descriptor.path ? first.content : second.content),
      }),
    ).rejects.toThrow("exceeding the limit");

    expect(existsSync(join(reposDir, "102", "revisions", "too-many-stls"))).toBe(false);
    expect(await candidateNames(reposDir, 102)).toEqual([]);
  });

  it("treats the Git tree size as a hint and trusts the verified response length", async () => {
    const reposDir = tempReposDir();
    const store = new LocalSourceSnapshotStore({ reposDir });
    const content = Buffer.alloc(256 * 1024, 7);
    const descriptor: SnapshotFile = {
      path: sourceRelativePath("lfs-part.stl"),
      kind: "stl",
      sizeHintBytes: 128,
    };

    const published = await store.materialize({
      sourceId: 15,
      upstreamRevisionKey: "lfs-response",
      files: [descriptor],
      selection: selection(),
      openFile: async () => response(content),
    });

    expect(published.files[0]?.sizeBytes).toBe(content.byteLength);
    expect(published.files[0]?.sha256).toBe(createHash("sha256").update(content).digest("hex"));
  });

  it("fails EXDEV promotion without copying or leaving a candidate", async () => {
    const reposDir = tempReposDir();
    const store = new LocalSourceSnapshotStore({
      reposDir,
      dependencies: {
        renameDirectory: async () => {
          throw Object.assign(new Error("cross-device link"), { code: "EXDEV" });
        },
      },
    });
    const part = file("part.stl", "stl", "geometry");

    await expect(
      store.materialize({
        sourceId: 11,
        upstreamRevisionKey: "cross-device",
        files: [part.descriptor],
        selection: selection(),
        openFile: async () => response(part.content),
      }),
    ).rejects.toThrow(/EXDEV.*no copy fallback/);

    expect(existsSync(join(reposDir, "11", "revisions", "cross-device"))).toBe(false);
    expect(await candidateNames(reposDir, 11)).toEqual([]);
  });

  it("reuses an exact published revision without opening the upstream streams", async () => {
    const reposDir = tempReposDir();
    const store = new LocalSourceSnapshotStore({ reposDir });
    const part = file("part.stl", "stl", "geometry");
    const first = await store.materialize({
      sourceId: 12,
      upstreamRevisionKey: "same-revision",
      files: [part.descriptor],
      selection: selection(),
      openFile: async () => response(part.content),
    });
    const openFile = vi.fn(async () => response(Buffer.from("should not download")));

    const reused = await store.materialize({
      sourceId: 12,
      upstreamRevisionKey: "same-revision",
      files: [part.descriptor],
      selection: selection(),
      openFile,
    });

    expect(reused.publication).toBe("reused");
    expect(reused.manifestDigest).toBe(first.manifestDigest);
    expect(openFile).not.toHaveBeenCalled();
  });

  it("keeps the first valid snapshot when selection settings later change", async () => {
    const reposDir = tempReposDir();
    const store = new LocalSourceSnapshotStore({ reposDir });
    const part = file("part.stl", "stl", "geometry");
    const readme = file("README.md", "readme", "notes");
    const first = await store.materialize({
      sourceId: 120,
      upstreamRevisionKey: "fixed-policy-revision",
      files: [part.descriptor],
      selection: selection([
        {
          path: readme.descriptor.path,
          kind: "readme",
          sizeHintBytes: readme.content.byteLength,
          reason: "documentation-byte-budget",
        },
      ]),
      openFile: async () => ({
        stream: Readable.from(part.content),
        contentLengthBytes: part.content.byteLength,
      }),
    });
    const openFile = vi.fn(async () => ({
      stream: Readable.from(Buffer.from("unused")),
      contentLengthBytes: null,
    }));

    const reused = await store.materialize({
      sourceId: 120,
      upstreamRevisionKey: "fixed-policy-revision",
      files: [part.descriptor, readme.descriptor],
      selection: { ...selection(), maxDocumentationBytes: 100 },
      openFile,
    });

    expect(reused.publication).toBe("reused");
    expect(reused.manifestDigest).toBe(first.manifestDigest);
    expect(reused.files.map((entry) => entry.path)).toEqual([part.descriptor.path]);
    expect(openFile).not.toHaveBeenCalled();
  });

  it("rejects a tampered or extra file in an existing revision", async () => {
    const reposDir = tempReposDir();
    const store = new LocalSourceSnapshotStore({ reposDir });
    const part = file("parts/part.stl", "stl", "geometry");
    const published = await store.materialize({
      sourceId: 13,
      upstreamRevisionKey: "tamper-check",
      files: [part.descriptor],
      selection: selection(),
      openFile: async () => response(part.content),
    });
    await writeFile(join(published.absolutePath, "parts/part.stl"), "changed");

    await expect(
      store.materialize({
        sourceId: 13,
        upstreamRevisionKey: "tamper-check",
        files: [part.descriptor],
        selection: selection(),
        openFile: async () => response(part.content),
      }),
    ).rejects.toThrow(/size mismatch|does not match its manifest/);

    await writeFile(join(published.absolutePath, "parts/part.stl"), part.content);
    await mkdir(join(published.absolutePath, "unexpected"));
    await writeFile(join(published.absolutePath, "unexpected/extra.stl"), "extra");
    await expect(
      store.materialize({
        sourceId: 13,
        upstreamRevisionKey: "tamper-check",
        files: [part.descriptor],
        selection: selection(),
        openFile: async () => response(part.content),
      }),
    ).rejects.toThrow("do not match the selected file set");
  });

  it("rejects unsafe and case-colliding paths before opening a stream", async () => {
    expect(() => sourceRelativePath("../outside.stl")).toThrow("Unsafe Source snapshot path");
    expect(() => sourceRelativePath("parts\\outside.stl")).toThrow("Unsafe Source snapshot path");
    expect(() => sourceRelativePath("cafe\u0301.stl")).toThrow("Unsafe Source snapshot path");
    expect(() => sourceRelativePath(SOURCE_SNAPSHOT_MANIFEST_FILE)).toThrow("reserved path");

    const reposDir = tempReposDir();
    const store = new LocalSourceSnapshotStore({ reposDir });
    const openFile = vi.fn(async () => response(Buffer.from("unused")));
    await expect(
      store.materialize({
        sourceId: 14,
        upstreamRevisionKey: "duplicate-paths",
        files: [
          { path: sourceRelativePath("Part.stl"), kind: "stl", sizeHintBytes: null },
          { path: sourceRelativePath("part.stl"), kind: "stl", sizeHintBytes: null },
        ],
        selection: selection(),
        openFile,
      }),
    ).rejects.toThrow("Duplicate Source snapshot path");
    expect(openFile).not.toHaveBeenCalled();
    expect(await candidateNames(reposDir, 14)).toEqual([]);
  });

  it("keeps a same-digest directory when its creator releases during the existence check", async () => {
    const reposDir = tempReposDir();
    const creator = new LocalSourceSnapshotStore({ reposDir, retainUntilRelease: true });
    const part = file("part.stl", "stl", "geometry");
    const created = await creator.materialize({
      sourceId: 42,
      upstreamRevisionKey: "same-digest",
      files: [part.descriptor],
      selection: selection(),
      openFile: async () => response(part.content),
    });
    const { lstat: actualLstat } =
      await vi.importActual<typeof import("node:fs/promises")>("node:fs/promises");
    let sawDirectory = false;
    vi.mocked(lstat).mockImplementation(async (path, options) => {
      const stat = await actualLstat(path, options);
      if (!sawDirectory && resolve(String(path)) === resolve(created.absolutePath)) {
        sawDirectory = true;
        releaseSharedSnapshot(created);
        if (sharedSnapshotHolderCount(created.absolutePath) === 0) {
          rmSync(created.absolutePath, { recursive: true, force: true });
        }
      }
      return stat;
    });
    const reuser = new LocalSourceSnapshotStore({ reposDir, retainUntilRelease: true });
    try {
      const reused = await reuser.materialize({
        sourceId: 42,
        upstreamRevisionKey: "same-digest",
        files: [part.descriptor],
        selection: selection(),
        openFile: async () => response(Buffer.from("should not replace geometry")),
      });
      expect(sawDirectory).toBe(true);
      expect(reused.publication).toBe("reused");
      expect(reused.manifestDigest).toBe(created.manifestDigest);
      expect(readFileSync(join(reused.absolutePath, "part.stl"), "utf8")).toBe("geometry");
      expect(sharedSnapshotHolderCount(reused.absolutePath)).toBe(1);
      releaseSharedSnapshot(reused);
      expect(sharedSnapshotHolderCount(reused.absolutePath)).toBe(0);
    } finally {
      vi.mocked(lstat).mockRestore();
      releaseSharedSnapshot(created);
    }
  });

  it("keeps a same-digest directory when its creator releases during a rename collision", async () => {
    const reposDir = tempReposDir();
    const creator = new LocalSourceSnapshotStore({ reposDir, retainUntilRelease: true });
    const part = file("part.stl", "stl", "geometry");
    const created = await creator.materialize({
      sourceId: 43,
      upstreamRevisionKey: "same-digest-collision",
      files: [part.descriptor],
      selection: selection(),
      openFile: async () => response(part.content),
    });
    const { lstat: actualLstat } =
      await vi.importActual<typeof import("node:fs/promises")>("node:fs/promises");
    let hidDirectory = false;
    let sawDirectory = false;
    vi.mocked(lstat).mockImplementation(async (path, options) => {
      if (!hidDirectory && resolve(String(path)) === resolve(created.absolutePath)) {
        hidDirectory = true;
        throw Object.assign(new Error("missing"), { code: "ENOENT" });
      }
      const stat = await actualLstat(path, options);
      if (!sawDirectory && resolve(String(path)) === resolve(created.absolutePath)) {
        sawDirectory = true;
        releaseSharedSnapshot(created);
        if (sharedSnapshotHolderCount(created.absolutePath) === 0) {
          rmSync(created.absolutePath, { recursive: true, force: true });
        }
      }
      return stat;
    });
    const reuser = new LocalSourceSnapshotStore({
      reposDir,
      retainUntilRelease: true,
      dependencies: {
        renameDirectory: async () => {
          throw Object.assign(new Error("exists"), { code: "EEXIST" });
        },
      },
    });
    try {
      const reused = await reuser.materialize({
        sourceId: 43,
        upstreamRevisionKey: "same-digest-collision",
        files: [part.descriptor],
        selection: selection(),
        openFile: async () => response(part.content),
      });
      expect(hidDirectory).toBe(true);
      expect(sawDirectory).toBe(true);
      expect(reused.publication).toBe("reused");
      expect(readFileSync(join(reused.absolutePath, "part.stl"), "utf8")).toBe("geometry");
      releaseSharedSnapshot(reused);
      expect(sharedSnapshotHolderCount(created.absolutePath)).toBe(0);
    } finally {
      vi.mocked(lstat).mockRestore();
      releaseSharedSnapshot(created);
    }
  });

  it("drops an unreferenced digest after a missing reuse claim is released", async () => {
    const reposDir = tempReposDir();
    const store = new LocalSourceSnapshotStore({ reposDir, retainUntilRelease: true });
    const loser = file("loser.stl", "stl", "loser-bytes");
    const winner = file("winner.stl", "stl", "winner-bytes");
    const lost = await store.materialize({
      sourceId: 8,
      upstreamRevisionKey: "digest-loser",
      files: [loser.descriptor],
      selection: selection(),
      openFile: async () => response(loser.content),
    });
    const kept = await store.materialize({
      sourceId: 8,
      upstreamRevisionKey: "digest-winner",
      files: [winner.descriptor],
      selection: selection(),
      openFile: async () => response(winner.content),
    });
    const absent = join(reposDir, "8", "revisions", "digest-absent");
    await expect(
      store.materialize({
        sourceId: 8,
        upstreamRevisionKey: "digest-absent",
        files: [loser.descriptor],
        selection: selection(),
        openFile: async () => {
          throw new Error("stop before publish");
        },
      }),
    ).rejects.toThrow("stop before publish");
    expect(sharedSnapshotHolderCount(absent)).toBe(0);
    expect(existsSync(absent)).toBe(false);
    releaseSharedSnapshot(lost);
    expect(sharedSnapshotHolderCount(lost.absolutePath)).toBe(0);
    rmSync(lost.absolutePath, { recursive: true, force: true });
    expect(existsSync(lost.absolutePath)).toBe(false);
    expect(readFileSync(join(kept.absolutePath, "winner.stl"), "utf8")).toBe("winner-bytes");
    releaseSharedSnapshot(kept);
    expect(sharedSnapshotHolderCount(kept.absolutePath)).toBe(0);
  });
});
