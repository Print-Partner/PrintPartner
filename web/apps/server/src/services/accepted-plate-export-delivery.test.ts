import { createHash } from "node:crypto";
import { spawn } from "node:child_process";
import {
  existsSync,
  chmodSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  realpathSync,
  rmSync,
  statSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, relative } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { runWithTenantDiskQuota, TenantDiskQuotaError } from "../lib/tenant-disk-quota.js";
import type { AcceptedPlateExportInput } from "../db/accepted-plates.js";
import type { AcceptedOperationalArtifact } from "../db/accepted-plan-operational.js";
import { parseRequiredUnitToken } from "./required-units.js";
import {
  materializeAcceptedPlateExport,
  stageAcceptedPlateExport,
  type MaterializeAcceptedPlateExportDependencies,
} from "./accepted-plate-export-delivery.js";

const roots: string[] = [];

async function runMaterializerProcess(
  payload: Readonly<{
    input: AcceptedPlateExportInput;
    reposDir: string;
    tenantExportsDir: string;
    limits: MaterializeAcceptedPlateExportDependencies["limits"];
    command: { profileId: number; expectedPlateRevisionId: number };
    barrierDirectory: string;
  }>,
): Promise<string> {
  const moduleUrl = new URL("./accepted-plate-export-delivery.ts", import.meta.url).href;
  const source = `
    import { writeFile, readdir } from "node:fs/promises";
    import { setTimeout as wait } from "node:timers/promises";
    import { materializeAcceptedPlateExport } from ${JSON.stringify(moduleUrl)};
    const payload = JSON.parse(process.env.PP_MATERIALIZER_PAYLOAD);
    await writeFile(\`${"${payload.barrierDirectory}"}/\${process.pid}\`, "ready");
    while ((await readdir(payload.barrierDirectory)).length < 2) await wait(5);
    const result = await materializeAcceptedPlateExport({
      repository: { readAcceptedPlateExportInput: () => ({ kind: "ready", input: payload.input }) },
      reposDir: payload.reposDir,
      tenantExportsDir: payload.tenantExportsDir,
      limits: payload.limits,
    }, payload.command);
    process.stdout.write(JSON.stringify(result));
  `;
  return new Promise((resolve, reject) => {
    const child = spawn(
      process.execPath,
      ["--import", "tsx", "--input-type=module", "--eval", source],
      {
        cwd: new URL("../../..", import.meta.url),
        env: { ...process.env, PP_MATERIALIZER_PAYLOAD: JSON.stringify(payload) },
        stdio: ["ignore", "pipe", "pipe"],
      },
    );
    let stdout = "";
    let stderr = "";
    child.stdout.setEncoding("utf8").on("data", (chunk: string) => {
      stdout += chunk;
    });
    child.stderr.setEncoding("utf8").on("data", (chunk: string) => {
      stderr += chunk;
    });
    child.on("error", reject);
    child.on("close", (code) => {
      if (code === 0) resolve(stdout);
      else reject(new Error(`Materializer process exited ${code}: ${stderr}`));
    });
  });
}

afterEach(() => {
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true });
});

function fixture(): {
  dependencies: MaterializeAcceptedPlateExportDependencies;
  command: { profileId: number; expectedPlateRevisionId: number };
} {
  const root = mkdtempSync(join(tmpdir(), "pp-accepted-export-delivery-"));
  roots.push(root);
  const reposDir = join(root, "repos");
  const tenantExportsDir = join(root, "exports", "tenant-default");
  const snapshotRoot = join(reposDir, "snapshots", "accepted");
  mkdirSync(snapshotRoot, { recursive: true });
  const bytes = Buffer.from(`solid accepted
facet normal 0 0 1
outer loop
vertex 0 0 0
vertex 3 0 0
vertex 0 5 10
endloop
endfacet
endsolid accepted`);
  writeFileSync(join(snapshotRoot, "part.stl"), bytes);
  const artifact: Extract<AcceptedOperationalArtifact, { kind: "tracked" }> = {
    kind: "tracked",
    sourceId: 1,
    sourceRevisionId: 2,
    snapshotRoot,
    relativePath: "part.stl",
    expectedSha256: createHash("sha256").update(bytes).digest("hex"),
  };
  const input: AcceptedPlateExportInput = {
    basis: {
      profileId: 7,
      planVersion: 3,
      revisionId: 11,
      revisionDigest: "a".repeat(64),
      requiredUnitMappingDigest: "b".repeat(64),
    },
    plateRevisionId: 19,
    plateRevisionNumber: 2,
    layoutDigest: "c".repeat(64),
    plates: [
      {
        plateId: "plate-one",
        ordinal: 1,
        printerId: "printer-one",
        printerName: "Printer One",
        printerModel: "Model One",
        bedWidthUm: 250_000,
        bedDepthUm: 250_000,
        bedHeightUm: 250_000,
        marginUm: 0,
        units: [
          {
            token: parseRequiredUnitToken("ppu_00000000000000000000000000000001"),
            objectName: "Part__ppu_00000000000000000000000000000001",
            xUm: 1_000,
            yUm: 2_000,
            widthUm: 3_000,
            depthUm: 5_000,
            heightUm: 10_000,
            artifact,
          },
        ],
      },
    ],
  };
  return {
    dependencies: {
      repository: { readAcceptedPlateExportInput: () => ({ kind: "ready", input }) },
      reposDir,
      tenantExportsDir,
      limits: {
        maxArtifactBytes: 1_000_000,
        maxTotalSourceBytes: 2_000_000,
        maxObjects: 100,
        maxTriangles: 100,
        maxOutputBytes: 10_000_000,
        maxPlates: 100,
      },
    },
    command: { profileId: 7, expectedPlateRevisionId: 19 },
  };
}

describe("materializeAcceptedPlateExport", () => {
  it("rejects the complete temporary tree before writing when quota is too small", async () => {
    const { dependencies, command } = fixture();
    const dataDir = dirname(dirname(dependencies.tenantExportsDir));

    await expect(
      runWithTenantDiskQuota(
        {
          dataDir,
          reposDir: dependencies.reposDir,
          tenantId: "default",
          sourceIds: () => [],
          quotaBytes: 1,
        },
        async () => materializeAcceptedPlateExport(dependencies, command),
      ),
    ).rejects.toBeInstanceOf(TenantDiskQuotaError);

    expect(
      existsSync(
        join(dependencies.tenantExportsDir, "accepted-plates", "profile-7", "revision-19"),
      ),
    ).toBe(false);
  });

  it("publishes the fixed immutable tree with verified metadata", async () => {
    const { dependencies, command } = fixture();
    const result = await materializeAcceptedPlateExport(dependencies, command);

    expect(result.kind).toBe("materialized");
    if (result.kind !== "materialized") throw new Error("materialization failed");
    expect(result.plates).toHaveLength(1);
    expect(result.plates[0]).toMatchObject({
      filename: "0001.3mf",
      relativePath: "accepted-plates/profile-7/revision-19/plates/0001.3mf",
    });
    expect(result.manifest.relativePath).toBe(
      "accepted-plates/profile-7/revision-19/manifest.json",
    );
    expect(result.bundle.relativePath).toBe(
      "accepted-plates/profile-7/revision-19/accepted-plates.zip",
    );
    for (const file of [result.manifest, result.bundle, ...result.plates]) {
      expect(relative(realpathSync(dependencies.tenantExportsDir), file.absolutePath)).not.toMatch(
        /^\.\./,
      );
      const published = readFileSync(file.absolutePath);
      expect(published).toHaveLength(file.byteLength);
      expect(createHash("sha256").update(published).digest("hex")).toBe(file.sha256);
    }
  });

  it("returns identical metadata on retries without replacing published files", async () => {
    const { dependencies, command } = fixture();
    const first = await materializeAcceptedPlateExport(dependencies, command);
    expect(first.kind).toBe("materialized");
    if (first.kind !== "materialized") throw new Error("materialization failed");
    const manifestBytes = readFileSync(first.manifest.absolutePath);
    const firstIdentities = [first.manifest, first.bundle, ...first.plates].map((file) => {
      const stat = statSync(file.absolutePath, { bigint: true });
      return { inode: stat.ino, modifiedAt: stat.mtimeNs };
    });

    const second = await materializeAcceptedPlateExport(dependencies, command);
    const secondIdentities = [first.manifest, first.bundle, ...first.plates].map((file) => {
      const stat = statSync(file.absolutePath, { bigint: true });
      return { inode: stat.ino, modifiedAt: stat.mtimeNs };
    });

    expect(second).toEqual(first);
    expect(readFileSync(first.manifest.absolutePath)).toEqual(manifestBytes);
    expect(secondIdentities).toEqual(firstIdentities);
  });

  it("returns equal results for concurrent calls", async () => {
    const { dependencies, command } = fixture();
    const [first, second] = await Promise.all([
      materializeAcceptedPlateExport(dependencies, command),
      materializeAcceptedPlateExport(dependencies, command),
    ]);

    expect(first.kind).toBe("materialized");
    expect(second).toEqual(first);
  });

  it("converges two processes released through one publication barrier", async () => {
    const { dependencies, command } = fixture();
    const resolved = dependencies.repository.readAcceptedPlateExportInput(command.profileId);
    expect(resolved.kind).toBe("ready");
    if (resolved.kind !== "ready") throw new Error("fixture input is unavailable");
    const barrierDirectory = join(dependencies.tenantExportsDir, "process-barrier");
    mkdirSync(barrierDirectory, { recursive: true });
    const payload = {
      input: resolved.input,
      reposDir: dependencies.reposDir,
      tenantExportsDir: dependencies.tenantExportsDir,
      limits: dependencies.limits,
      command,
      barrierDirectory,
    };

    const [first, second] = await Promise.all([
      runMaterializerProcess(payload),
      runMaterializerProcess(payload),
    ]);
    const verified = await materializeAcceptedPlateExport(dependencies, command);

    expect(first).toBe(second);
    expect(verified.kind).toBe("materialized");
  }, 15_000);

  it.each([
    ["altered", (path: string) => writeFileSync(path, "altered")],
    ["missing", (path: string) => rmSync(path)],
    [
      "symlinked",
      (path: string) => {
        rmSync(path);
        symlinkSync("../manifest.json", path);
      },
    ],
    [
      "non-regular",
      (path: string) => {
        rmSync(path);
        mkdirSync(path);
      },
    ],
  ])("rejects %s published Plate content without repairing it", async (_case, mutate) => {
    const { dependencies, command } = fixture();
    const first = await materializeAcceptedPlateExport(dependencies, command);
    expect(first.kind).toBe("materialized");
    if (first.kind !== "materialized") throw new Error("materialization failed");
    const path = first.plates[0]!.absolutePath;
    mutate(path);

    await expect(materializeAcceptedPlateExport(dependencies, command)).resolves.toEqual({
      kind: "output_conflict",
    });
  });

  it("rejects extra published content without deleting it", async () => {
    const { dependencies, command } = fixture();
    const first = await materializeAcceptedPlateExport(dependencies, command);
    expect(first.kind).toBe("materialized");
    if (first.kind !== "materialized") throw new Error("materialization failed");
    const extra = join(first.manifest.absolutePath, "..", "unexpected.txt");
    writeFileSync(extra, "unexpected");

    await expect(materializeAcceptedPlateExport(dependencies, command)).resolves.toEqual({
      kind: "output_conflict",
    });
    expect(readFileSync(extra, "utf8")).toBe("unexpected");
  });

  it("rejects a symlinked publication parent before writing outside the tenant root", async () => {
    const { dependencies, command } = fixture();
    const tenantRoot = dependencies.tenantExportsDir;
    const outside = join(tenantRoot, "..", "outside");
    mkdirSync(tenantRoot, { recursive: true });
    mkdirSync(outside);
    symlinkSync(outside, join(tenantRoot, "accepted-plates"));

    await expect(materializeAcceptedPlateExport(dependencies, command)).resolves.toEqual({
      kind: "output_conflict",
    });
    expect(existsSync(join(outside, "profile-7"))).toBe(false);
  });

  it("leaves no final revision when generation fails", async () => {
    const { dependencies, command } = fixture();
    const result = await materializeAcceptedPlateExport(
      {
        ...dependencies,
        repository: { readAcceptedPlateExportInput: () => ({ kind: "empty_plan" }) },
      },
      command,
    );

    expect(result).toEqual({ kind: "empty_plan" });
    expect(
      existsSync(
        join(dependencies.tenantExportsDir, "accepted-plates", "profile-7", "revision-19"),
      ),
    ).toBe(false);
  });

  it("leaves no final revision when the publication parent is unwritable", async () => {
    const { dependencies, command } = fixture();
    const parent = join(
      dependencies.tenantExportsDir,
      "accepted-plates",
      `profile-${command.profileId}`,
    );
    mkdirSync(parent, { recursive: true });
    chmodSync(parent, 0o500);

    try {
      await expect(materializeAcceptedPlateExport(dependencies, command)).rejects.toThrow();
      expect(existsSync(join(parent, `revision-${command.expectedPlateRevisionId}`))).toBe(false);
    } finally {
      chmodSync(parent, 0o700);
    }
  });

  it.each([
    { profileId: 0, expectedPlateRevisionId: 19 },
    { profileId: 7.5, expectedPlateRevisionId: 19 },
    { profileId: 7, expectedPlateRevisionId: Number.MAX_SAFE_INTEGER + 1 },
  ])("rejects unsafe identifiers before constructing output paths %#", async (command) => {
    const { dependencies } = fixture();
    const unusedRoot = join(dependencies.tenantExportsDir, "unused");

    await expect(
      materializeAcceptedPlateExport(
        {
          ...dependencies,
          tenantExportsDir: unusedRoot,
        },
        command,
      ),
    ).rejects.toThrow(RangeError);
    expect(existsSync(unusedRoot)).toBe(false);
  });
});

describe("stageAcceptedPlateExport", () => {
  it("publishes byte-identical Plate files in a revision-specific inbox", async () => {
    const { dependencies, command } = fixture();
    const materialized = await materializeAcceptedPlateExport(dependencies, command);
    if (materialized.kind !== "materialized") throw new Error("materialization failed");
    const exchangeRoot = join(dependencies.tenantExportsDir, "..", "exchange");
    mkdirSync(exchangeRoot);

    const staged = await stageAcceptedPlateExport({
      materialized,
      exchangeRoot,
      instanceId: "orca/main",
    });

    expect(staged.kind).toBe("staged");
    if (staged.kind !== "staged") throw new Error("staging failed");
    expect(staged.inboxRelativePath).toBe("pp-inbox/orca_main/profile-7/revision-19");
    expect(staged.staged).toEqual([{ ordinal: 1, filename: "0001.3mf" }]);
    expect(readFileSync(join(staged.absoluteDirectory, "0001.3mf"))).toEqual(
      readFileSync(materialized.plates[0]!.absolutePath),
    );
    await expect(
      stageAcceptedPlateExport({
        materialized,
        exchangeRoot,
        instanceId: "orca/main",
      }),
    ).resolves.toEqual(staged);
  });

  it("rejects a differing inbox without overwriting it", async () => {
    const { dependencies, command } = fixture();
    const materialized = await materializeAcceptedPlateExport(dependencies, command);
    if (materialized.kind !== "materialized") throw new Error("materialization failed");
    const exchangeRoot = join(dependencies.tenantExportsDir, "..", "exchange");
    mkdirSync(exchangeRoot);
    const first = await stageAcceptedPlateExport({
      materialized,
      exchangeRoot,
      instanceId: "orca",
    });
    if (first.kind !== "staged") throw new Error("staging failed");
    const stagedFile = join(first.absoluteDirectory, "0001.3mf");
    writeFileSync(stagedFile, "tampered");

    await expect(
      stageAcceptedPlateExport({ materialized, exchangeRoot, instanceId: "orca" }),
    ).resolves.toEqual({ kind: "output_conflict" });
    expect(readFileSync(stagedFile, "utf8")).toBe("tampered");
  });
});
