import { createHash } from "node:crypto";
import {
  mkdtempSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  rmSync,
  statfsSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { availableParallelism, loadavg, tmpdir } from "node:os";
import { basename, dirname, join, sep } from "node:path";
import { monitorEventLoopDelay } from "node:perf_hooks";
import AdmZip from "adm-zip";
import { miloFilenameGrouping } from "@print-partner/contracts";
import { afterEach, describe, expect, it, vi } from "vitest";
import { runWithTenantDiskQuota, TenantDiskQuotaError } from "../lib/tenant-disk-quota.js";
import type {
  AcceptedExportPart,
  AcceptedOperationalExport,
  CaptureAcceptedOperationalExportResult,
} from "./accepted-operational-export.js";
import {
  materializeAcceptedStlBundle,
  type AcceptedStlBundleWarning,
  parseStlPackUnitTokens,
} from "./export-stl-pack.js";

type ExportDiagnostic = {
  outcome:
    | { kind: "running" }
    | { kind: "returned"; resultKind: string; elapsedMs: number }
    | { kind: "threw"; elapsedMs: number };
  start: number;
  firstZipSourceAt: number | null;
  flushCalls: number;
  flushCallbacks: number;
  flushErrors: number;
  flushInflight: number;
  flushMaxInflight: number;
  flushLatencyTotalMs: number;
  flushLatencyMaxMs: number;
  zipSources: number;
  zipEnds: number;
  zipErrors: number;
  zipBytes: number;
};

const acceptedArtifactTestHook = vi.hoisted(() => {
  const diagnostic: { current: ExportDiagnostic | null } = { current: null };
  return {
    diagnostic,
    afterVerifiedOpen: undefined as (() => void) | undefined,
    failZipRead: false,
    zipReadFailures: 0,
    failFileFlush: false,
    syncMode: "real" as "real" | "missing" | "nonzero",
    syncCalls: 0,
    syncLoopTicks: 0,
  };
});

vi.mock("node:fs", async (importOriginal) => {
  const actual = await importOriginal<typeof import("node:fs")>();
  return {
    ...actual,
    fsync(descriptor: number, callback: (error: NodeJS.ErrnoException | null) => void) {
      if (acceptedArtifactTestHook.failFileFlush) {
        callback(new Error("file flush failed"));
        return;
      }
      const measurement = acceptedArtifactTestHook.diagnostic.current;
      const start = measurement ? performance.now() : 0;
      if (measurement) {
        measurement.flushCalls++;
        measurement.flushInflight++;
        measurement.flushMaxInflight = Math.max(
          measurement.flushMaxInflight,
          measurement.flushInflight,
        );
      }
      actual.fsync(descriptor, (error) => {
        if (measurement) {
          const elapsed = performance.now() - start;
          measurement.flushCallbacks++;
          measurement.flushInflight--;
          if (error) measurement.flushErrors++;
          measurement.flushLatencyTotalMs += elapsed;
          measurement.flushLatencyMaxMs = Math.max(measurement.flushLatencyMaxMs, elapsed);
        }
        callback(error);
      });
    },
    createReadStream(...args: Parameters<typeof actual.createReadStream>) {
      const [path, options] = args;
      if (
        acceptedArtifactTestHook.failZipRead &&
        typeof path === "string" &&
        path.includes("/.tmp-") &&
        path.endsWith(".stl")
      ) {
        acceptedArtifactTestHook.zipReadFailures++;
        return actual.createReadStream(`${path}.missing`, options);
      }
      const stream = actual.createReadStream(...args);
      const measurement = acceptedArtifactTestHook.diagnostic.current;
      if (
        measurement &&
        typeof path === "string" &&
        path.split(sep).some((segment) => segment.startsWith(".tmp-")) &&
        path.endsWith(".stl")
      ) {
        measurement.firstZipSourceAt ??= performance.now();
        measurement.zipSources++;
        stream.once("end", () => {
          measurement.zipEnds++;
          measurement.zipBytes += stream.bytesRead;
        });
        stream.once("error", () => {
          measurement.zipErrors++;
        });
      }
      return stream;
    },
  };
});

vi.mock("node:child_process", async (importOriginal) => {
  const actual = await importOriginal<typeof import("node:child_process")>();
  const callbackOf = (args: readonly unknown[]) => {
    const last = args[args.length - 1];
    return typeof last === "function"
      ? (last as (error: Error | null, stdout: string, stderr: string) => void)
      : null;
  };
  const execFile = ((...args: readonly [string, ...(readonly unknown[])]) => {
    const callback = callbackOf(args);
    if (args[0] !== "sync" || !callback) return Reflect.apply(actual.execFile, actual, args);
    acceptedArtifactTestHook.syncCalls += 1;
    if (acceptedArtifactTestHook.syncMode === "real")
      return Reflect.apply(actual.execFile, actual, args);
    const timer = setInterval(() => {
      acceptedArtifactTestHook.syncLoopTicks += 1;
    }, 5);
    setTimeout(() => {
      clearInterval(timer);
      const error =
        acceptedArtifactTestHook.syncMode === "missing"
          ? Object.assign(new Error("spawn sync ENOENT"), { code: "ENOENT" })
          : Object.assign(new Error("sync -f failed"), { code: 1 });
      callback(error, "", "");
    }, 40);
    return undefined;
  }) as typeof actual.execFile;
  const spawnSync = ((file: string, ...rest: readonly unknown[]) => {
    if (file === "sync") throw new Error("sync must not run through spawnSync");
    return Reflect.apply(actual.spawnSync, actual, [file, ...rest]);
  }) as typeof actual.spawnSync;
  return { ...actual, execFile, spawnSync };
});

vi.mock("./accepted-artifacts.js", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./accepted-artifacts.js")>();
  return {
    ...actual,
    openVerifiedAcceptedArtifact(input: Parameters<typeof actual.openVerifiedAcceptedArtifact>[0]) {
      const result = actual.openVerifiedAcceptedArtifact(input);
      const hook = acceptedArtifactTestHook.afterVerifiedOpen;
      acceptedArtifactTestHook.afterVerifiedOpen = undefined;
      hook?.();
      return result;
    },
  };
});

const roots: string[] = [];

let stopDiagnostic: (() => void) | undefined;

function diagnosticFile(path: string): string | null {
  try {
    return readFileSync(path, "utf8").slice(0, 4096).trim();
  } catch {
    return null;
  }
}

function cgroupCpuContext() {
  const cgroup = diagnosticFile("/proc/self/cgroup")
    ?.split("\n")
    .find((line) => line.startsWith("0::/"))
    ?.slice(3);
  if (!cgroup || cgroup.split("/").includes("..")) return null;
  const mount = "/sys/fs/cgroup";
  let directory = join(mount, cgroup.slice(1));
  const ancestors = [];
  for (let depth = 0; depth < 16; depth++) {
    ancestors.push({
      depth,
      cpuMax: diagnosticFile(join(directory, "cpu.max")),
      cpuStat: diagnosticFile(join(directory, "cpu.stat")),
    });
    if (directory === mount) break;
    directory = dirname(directory);
  }
  return ancestors;
}

function flushMeasurement(): ExportDiagnostic {
  return {
    outcome: { kind: "running" },
    start: performance.now(),
    firstZipSourceAt: null,
    flushCalls: 0,
    flushCallbacks: 0,
    flushErrors: 0,
    flushInflight: 0,
    flushMaxInflight: 0,
    flushLatencyTotalMs: 0,
    flushLatencyMaxMs: 0,
    zipSources: 0,
    zipEnds: 0,
    zipErrors: 0,
    zipBytes: 0,
  };
}

function measureLargeExport(root: string) {
  const filesystem = statfsSync(root);
  const measurement = flushMeasurement();
  const eventLoop = monitorEventLoopDelay({ resolution: 20 });
  eventLoop.enable();
  const cpuStart = process.cpuUsage();
  acceptedArtifactTestHook.diagnostic.current = measurement;
  const emit = (event: "start" | "progress" | "test-ended") => {
    const now = performance.now();
    const cpu = process.cpuUsage(cpuStart);
    process.stdout.write(
      `GRE-928 accepted-STL diagnostic ${JSON.stringify({
        event,
        node: process.version,
        runner: process.env.RUNNER_NAME ?? null,
        runnerEnvironment: process.env.RUNNER_ENVIRONMENT ?? null,
        runId: process.env.GITHUB_RUN_ID ?? null,
        runAttempt: process.env.GITHUB_RUN_ATTEMPT ?? null,
        workerPool: process.env.UV_THREADPOOL_SIZE ?? "default (4)",
        customTmpdir: process.env.TMPDIR !== undefined,
        availableParallelism: availableParallelism(),
        loadAverage: loadavg(),
        filesystemType: `0x${filesystem.type.toString(16)}`,
        cgroupV2Cpu: cgroupCpuContext(),
        cpuQuotaV1: diagnosticFile("/sys/fs/cgroup/cpu/cpu.cfs_quota_us"),
        cpuPeriodV1: diagnosticFile("/sys/fs/cgroup/cpu/cpu.cfs_period_us"),
        cpuPressure: diagnosticFile("/proc/pressure/cpu"),
        ioPressure: diagnosticFile("/proc/pressure/io"),
        elapsedMs: now - measurement.start,
        copiesUntilFirstZipSourceMs:
          measurement.firstZipSourceAt === null
            ? null
            : measurement.firstZipSourceAt - measurement.start,
        firstZipSourceUntilNowMs:
          measurement.firstZipSourceAt === null ? null : now - measurement.firstZipSourceAt,
        cpuUserMs: cpu.user / 1000,
        cpuSystemMs: cpu.system / 1000,
        eventLoopDelayMs: {
          mean: Number.isFinite(eventLoop.mean) ? eventLoop.mean / 1e6 : null,
          max: eventLoop.max / 1e6,
          p99: eventLoop.percentile(99) / 1e6,
        },
        measurement,
      })}\n`,
    );
  };
  emit("start");
  const timer = setInterval(() => emit("progress"), 15_000);
  stopDiagnostic = () => {
    clearInterval(timer);
    eventLoop.disable();
    emit("test-ended");
    acceptedArtifactTestHook.diagnostic.current = null;
    stopDiagnostic = undefined;
  };
  return measurement;
}

function fixture() {
  const root = mkdtempSync(join(tmpdir(), "print-partner-accepted-stl-"));
  roots.push(root);
  const reposDir = join(root, "repos");
  const snapshotRoot = join(reposDir, "snapshots", "one");
  const tenantExportsDir = join(root, "exports");
  mkdirSync(snapshotRoot, { recursive: true });
  return { reposDir, snapshotRoot, tenantExportsDir };
}

/** A Required-unit token in the real `ppu_<32 hex>` spelling, stable per unit. */
function unitToken(revisionPartId: number, unitIndex: number): string {
  const digest = createHash("sha256").update(`unit:${revisionPartId}:${unitIndex}`).digest("hex");
  return `ppu_${digest.slice(0, 32)}`;
}

/**
 * Recomputes the publication key the way the service did before per-unit
 * exports existed, from the published bytes alone. A drift here means an
 * already-published bundle would be re-exported under a new path.
 */
function keyBeforeUnitTokens(
  rootPath: string,
  warnings: readonly AcceptedStlBundleWarning[],
): string {
  const files: Array<{ relativePath: string; size: number; sha256: string }> = [];
  const walk = (directory: string, prefix: string): void => {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) {
        walk(path, `${prefix}${entry.name}/`);
        continue;
      }
      const bytes = readFileSync(path);
      files.push({
        relativePath: `${prefix}${entry.name}`,
        size: bytes.length,
        sha256: createHash("sha256").update(bytes).digest("hex"),
      });
    }
  };
  walk(rootPath, "");
  const identity = {
    files: files.sort((left, right) => left.relativePath.localeCompare(right.relativePath)),
    warnings: warnings
      .map((warning) => ({
        code: warning.code,
        relativePath: warning.relativePath,
        sourceLayer: warning.sourceLayer,
      }))
      .sort((left, right) =>
        `${left.relativePath}\0${left.sourceLayer}`.localeCompare(
          `${right.relativePath}\0${right.sourceLayer}`,
        ),
      ),
  };
  return createHash("sha256").update(JSON.stringify(identity)).digest("hex");
}

function part(input: {
  snapshotRoot: string;
  bytes?: Buffer;
  revisionPartId?: number;
  filename?: string;
  relativePath?: string;
  completed?: readonly boolean[];
  unavailable?: boolean;
}): AcceptedExportPart {
  const bytes = input.bytes ?? Buffer.from("accepted-stl");
  const revisionPartId = input.revisionPartId ?? 31;
  const filename = input.filename ?? "widget.stl";
  const relativePath = input.relativePath ?? filename;
  if (!input.unavailable) {
    const path = join(input.snapshotRoot, ...relativePath.split("/"));
    mkdirSync(dirname(path), { recursive: true });
    writeFileSync(path, bytes);
  }
  const completed = input.completed ?? [false, true];
  return {
    revisionPartId,
    projectionPartId: revisionPartId + 100,
    partKey: `part:${revisionPartId}`,
    relativePath,
    filename,
    sourceLayer: "base:Fixture",
    status: "ok",
    role: "accent",
    filamentColorId: null,
    filamentCustomHex: null,
    spoolmanSpoolId: null,
    quantityInferred: completed.length,
    quantityOverride: null,
    quantityEffective: completed.length,
    included: true,
    notes: "",
    geometrySame: null,
    requirement: null,
    optionGroupId: null,
    manifestSource: null,
    artifact: input.unavailable
      ? { kind: "unavailable", reason: "legacy" }
      : {
          kind: "tracked",
          sourceId: 1,
          sourceRevisionId: 2,
          snapshotRoot: input.snapshotRoot,
          relativePath,
          expectedSha256: createHash("sha256").update(bytes).digest("hex"),
        },
    units: completed.map((value, unitIndex) => ({
      token: unitToken(revisionPartId, unitIndex),
      unitIndex,
      completed: value,
      assembled: false,
    })),
  };
}

function capture(
  parts: readonly AcceptedExportPart[],
): Extract<CaptureAcceptedOperationalExportResult, { readonly kind: "ready" }> {
  const accepted: AcceptedOperationalExport = {
    basis: {
      profileId: 7,
      planVersion: 4,
      revisionId: 19,
      revisionDigest: "a".repeat(64),
      requiredUnitMappingDigest: "b".repeat(64),
    },
    profile: {
      id: 7,
      name: "Accepted Build",
      orderNumber: null,
      specialRequest: null,
      archivedAt: null,
    },
    provenance: { kind: "legacy" },
    parts,
  };
  return { kind: "ready", export: accepted };
}

afterEach(() => {
  stopDiagnostic?.();
  acceptedArtifactTestHook.afterVerifiedOpen = undefined;
  acceptedArtifactTestHook.failZipRead = false;
  acceptedArtifactTestHook.zipReadFailures = 0;
  acceptedArtifactTestHook.failFileFlush = false;
  acceptedArtifactTestHook.syncMode = "real";
  acceptedArtifactTestHook.syncCalls = 0;
  acceptedArtifactTestHook.syncLoopTicks = 0;
  acceptedArtifactTestHook.diagnostic.current = null;
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true });
});

describe("materializeAcceptedStlBundle", () => {
  it("does not publish an export when a file flush fails", async () => {
    const paths = fixture();
    acceptedArtifactTestHook.failFileFlush = true;
    const result = await materializeAcceptedStlBundle({
      ...paths,
      capture: capture([part({ snapshotRoot: paths.snapshotRoot, completed: [false] })]),
      selection: "all",
      groupBy: "color",
      roleOrder: ["accent"],
    });

    expect(result).toEqual({ kind: "output_failure" });
    const entries = readdirSync(paths.tenantExportsDir, { recursive: true });
    expect(
      entries.some(
        (entry) => String(entry).includes(".tmp-") || String(entry).includes("content-"),
      ),
    ).toBe(false);
  });

  it("removes partial copies when a later unit exceeds the quota", async () => {
    const paths = fixture();
    await expect(
      runWithTenantDiskQuota(
        {
          dataDir: dirname(paths.tenantExportsDir),
          reposDir: paths.reposDir,
          tenantId: "default",
          sourceIds: () => [],
          quotaBytes: 24,
        },
        async () =>
          materializeAcceptedStlBundle({
            ...paths,
            capture: capture([
              part({ snapshotRoot: paths.snapshotRoot, completed: [false, false, false, false] }),
            ]),
            selection: "all",
            groupBy: "color",
            roleOrder: ["accent"],
          }),
      ),
    ).rejects.toBeInstanceOf(TenantDiskQuotaError);

    const entries = readdirSync(paths.tenantExportsDir, { recursive: true });
    expect(
      entries.some(
        (entry) => String(entry).includes(".tmp-") || String(entry).includes("content-"),
      ),
    ).toBe(false);
  });

  it("propagates quota rejection and removes copied and ZIP staging", async () => {
    const paths = fixture();
    const dataDir = dirname(paths.tenantExportsDir);
    const input = {
      ...paths,
      capture: capture([
        part({
          snapshotRoot: paths.snapshotRoot,
          bytes: Buffer.from("accepted-stl"),
          completed: [false],
        }),
      ]),
      selection: "all",
      groupBy: "color",
      roleOrder: ["accent"],
    } satisfies Parameters<typeof materializeAcceptedStlBundle>[0];

    await expect(
      runWithTenantDiskQuota(
        {
          dataDir,
          reposDir: paths.reposDir,
          tenantId: "default",
          sourceIds: () => [],
          quotaBytes: 5,
        },
        async () => materializeAcceptedStlBundle(input),
      ),
    ).rejects.toBeInstanceOf(TenantDiskQuotaError);

    const entries = readdirSync(paths.tenantExportsDir, { recursive: true });
    expect(
      entries.some(
        (entry) => String(entry).includes(".tmp-") || String(entry).includes("content-"),
      ),
    ).toBe(false);
  });

  it("returns output_failure and removes staging when a lazy ZIP source cannot be read", async () => {
    const paths = fixture();
    acceptedArtifactTestHook.failZipRead = true;
    const result = await materializeAcceptedStlBundle({
      ...paths,
      capture: capture([part({ snapshotRoot: paths.snapshotRoot, completed: [false] })]),
      selection: "all",
      groupBy: "color",
      roleOrder: ["accent"],
    });
    expect(acceptedArtifactTestHook.zipReadFailures).toBe(1);
    expect(result).toEqual({ kind: "output_failure" });
    const entries = readdirSync(paths.tenantExportsDir, { recursive: true });
    expect(
      entries.some(
        (entry) => String(entry).includes(".tmp-") || String(entry).includes("content-"),
      ),
    ).toBe(false);
  });
  it("preserves Unicode custom folder names in the ZIP", async () => {
    const paths = fixture();
    const result = await materializeAcceptedStlBundle({
      ...paths,
      capture: capture([
        part({ snapshotRoot: paths.snapshotRoot, filename: "mount-S.stl", completed: [false] }),
      ]),
      selection: "all",
      groupBy: "color",
      roleOrder: ["accent"],
      filenameGrouping: {
        definition: { name: "設定", rules: [{ suffix: "-S", group: "構造" }], overrides: {} },
        arrangement: "group",
      },
    });
    expect(result.kind).toBe("materialized");
    if (result.kind !== "materialized") return;
    expect(
      new AdmZip(result.bundlePath ?? "").getEntries().map((entry) => entry.entryName),
    ).toEqual(["構造/mount-S_01.stl"]);
  });
  it("refuses overlapping rules instead of duplicating selected files", async () => {
    const paths = fixture();
    const result = await materializeAcceptedStlBundle({
      ...paths,
      capture: capture([part({ snapshotRoot: paths.snapshotRoot, filename: "mount-S.stl" })]),
      selection: "all",
      groupBy: "color",
      roleOrder: ["accent"],
      filenameGrouping: {
        definition: {
          ...miloFilenameGrouping,
          rules: [...miloFilenameGrouping.rules, { suffix: "S", group: "Other" }],
        },
        arrangement: "group",
      },
    });
    expect(result).toEqual({ kind: "grouping_conflict" });
  });
  it("intersects color and filename settings with inclusion, remaining units and selected tokens", async () => {
    const paths = fixture();
    const structural = part({
      snapshotRoot: paths.snapshotRoot,
      filename: "mount-S.stl",
      completed: [true, false],
    });
    const other = part({
      snapshotRoot: paths.snapshotRoot,
      filename: "cover-A.stl",
      revisionPartId: 32,
    });
    const primary = {
      ...part({ snapshotRoot: paths.snapshotRoot, filename: "primary-S.stl", revisionPartId: 33 }),
      role: "primary",
    };
    const excluded = {
      ...part({ snapshotRoot: paths.snapshotRoot, filename: "excluded-S.stl", revisionPartId: 34 }),
      included: false,
    };
    const result = await materializeAcceptedStlBundle({
      ...paths,
      capture: capture([structural, other, primary, excluded]),
      selection: "missing",
      groupBy: "color",
      roleOrder: ["primary", "accent"],
      unitTokens: [unitToken(31, 0), unitToken(31, 1), unitToken(32, 1), unitToken(33, 1)],
      filenameGrouping: {
        definition: miloFilenameGrouping,
        arrangement: "color_group",
        role: "accent",
        group: "Structural",
      },
    });
    expect(result.kind).toBe("materialized");
    if (result.kind !== "materialized") return;
    expect(
      new AdmZip(result.bundlePath ?? "").getEntries().map((entry) => entry.entryName),
    ).toEqual(["accent/Structural/mount-S_02.stl"]);
    expect(readFileSync(join(result.rootPath, "accent/Structural/mount-S_02.stl"))).toEqual(
      Buffer.from("accepted-stl"),
    );
  });

  it.each(["group", "group_color"] as const)(
    "keeps unmatched files with %s ordering",
    async (arrangement) => {
      const paths = fixture();
      const result = await materializeAcceptedStlBundle({
        ...paths,
        capture: capture([part({ snapshotRoot: paths.snapshotRoot, completed: [false] })]),
        selection: "all",
        groupBy: "color",
        roleOrder: ["accent"],
        filenameGrouping: { definition: miloFilenameGrouping, arrangement },
      });
      expect(result.kind).toBe("materialized");
      if (result.kind !== "materialized") return;
      expect(
        new AdmZip(result.bundlePath ?? "").getEntries().map((entry) => entry.entryName),
      ).toEqual([
        arrangement === "group" ? "Unassigned/widget_01.stl" : "Unassigned/accent/widget_01.stl",
      ]);
    },
  );
  it("exports one byte-identical file per selected accepted Required unit", async () => {
    const fixturePaths = fixture();
    const bytes = Buffer.from("verified descriptor bytes");
    const captured = capture([
      part({ snapshotRoot: fixturePaths.snapshotRoot, bytes, completed: [true, false] }),
    ]);

    const all = await materializeAcceptedStlBundle({
      capture: captured,
      ...fixturePaths,
      selection: "all",
      groupBy: "color_dir",
      roleOrder: ["primary", "accent", "clear", "opaque"],
    });
    expect(all.kind).toBe("materialized");
    if (all.kind !== "materialized") return;
    expect(readFileSync(join(all.rootPath, "accent", "_root", "widget_01.stl"))).toEqual(bytes);
    expect(readFileSync(join(all.rootPath, "accent", "_root", "widget_02.stl"))).toEqual(bytes);
    expect(new AdmZip(all.bundlePath ?? "").getEntries().map((entry) => entry.entryName)).toEqual([
      "accent/_root/widget_01.stl",
      "accent/_root/widget_02.stl",
    ]);

    const missing = await materializeAcceptedStlBundle({
      capture: captured,
      ...fixturePaths,
      selection: "missing",
      groupBy: "color_dir",
      roleOrder: ["primary", "accent", "clear", "opaque"],
    });
    expect(missing.kind).toBe("materialized");
    if (missing.kind !== "materialized") return;
    expect(readFileSync(join(missing.rootPath, "accent", "_root", "widget_02.stl"))).toEqual(bytes);
    expect(missing.fileCounts).toEqual({ accent: 1 });
  });

  it("warns once for an unavailable Part while publishing verified peers", async () => {
    const fixturePaths = fixture();
    const captured = capture([
      part({ snapshotRoot: fixturePaths.snapshotRoot, revisionPartId: 31, unavailable: true }),
      part({ snapshotRoot: fixturePaths.snapshotRoot, revisionPartId: 32, filename: "peer.stl" }),
    ]);

    const result = await materializeAcceptedStlBundle({
      capture: captured,
      ...fixturePaths,
      selection: "all",
      groupBy: "color",
      roleOrder: ["primary", "accent", "clear", "opaque"],
    });

    expect(result.kind).toBe("materialized");
    if (result.kind !== "materialized") return;
    expect(result.fileCounts).toEqual({ accent: 2 });
    expect(result.warnings).toEqual([
      {
        code: "artifact_unavailable",
        relativePath: "widget.stl",
        sourceLayer: "base:Fixture",
      },
    ]);
  });

  it("does not publish bytes changed in place after accepted verification", async () => {
    const fixturePaths = fixture();
    const race = part({
      snapshotRoot: fixturePaths.snapshotRoot,
      revisionPartId: 31,
      filename: "race.stl",
      completed: [false],
    });
    const peerBytes = Buffer.from("stable peer");
    const peer = part({
      snapshotRoot: fixturePaths.snapshotRoot,
      revisionPartId: 32,
      filename: "peer.stl",
      bytes: peerBytes,
      completed: [false],
    });
    acceptedArtifactTestHook.afterVerifiedOpen = () => {
      writeFileSync(join(fixturePaths.snapshotRoot, "race.stl"), Buffer.alloc(0));
    };

    const result = await materializeAcceptedStlBundle({
      capture: capture([race, peer]),
      ...fixturePaths,
      selection: "all",
      groupBy: "color",
      roleOrder: ["primary", "accent", "clear", "opaque"],
    });

    expect(result.kind).toBe("materialized");
    if (result.kind !== "materialized") return;
    expect(result.warnings).toEqual([
      {
        code: "artifact_unavailable",
        relativePath: "race.stl",
        sourceLayer: "base:Fixture",
      },
    ]);
    expect(result.fileCounts).toEqual({ accent: 1 });
    expect(readFileSync(join(result.rootPath, "accent", "peer_01.stl"))).toEqual(peerBytes);
  });

  it("publishes a restored artifact beside an earlier partial result", async () => {
    const fixturePaths = fixture();
    const unavailable = part({
      snapshotRoot: fixturePaths.snapshotRoot,
      completed: [false],
      unavailable: true,
    });
    const first = await materializeAcceptedStlBundle({
      capture: capture([unavailable]),
      ...fixturePaths,
      selection: "missing",
      groupBy: "color",
      roleOrder: ["primary", "accent", "clear", "opaque"],
    });

    const bytes = Buffer.from("restored accepted STL");
    const restored = part({
      snapshotRoot: fixturePaths.snapshotRoot,
      bytes,
      completed: [false],
    });
    const second = await materializeAcceptedStlBundle({
      capture: capture([restored]),
      ...fixturePaths,
      selection: "missing",
      groupBy: "color",
      roleOrder: ["primary", "accent", "clear", "opaque"],
    });

    expect(first.kind).toBe("materialized");
    expect(second.kind).toBe("materialized");
    if (first.kind !== "materialized" || second.kind !== "materialized") return;
    expect(first.rootPath).not.toBe(second.rootPath);
    expect(first.warnings).toHaveLength(1);
    expect(second.warnings).toEqual([]);
    expect(readFileSync(join(second.rootPath, "accent", "widget_01.stl"))).toEqual(bytes);
  });

  it("publishes changed missing-unit selections beside the prior result", async () => {
    const fixturePaths = fixture();
    const first = await materializeAcceptedStlBundle({
      capture: capture([
        part({ snapshotRoot: fixturePaths.snapshotRoot, completed: [false, false] }),
      ]),
      ...fixturePaths,
      selection: "missing",
      groupBy: "color",
      roleOrder: ["primary", "accent", "clear", "opaque"],
    });
    const second = await materializeAcceptedStlBundle({
      capture: capture([
        part({ snapshotRoot: fixturePaths.snapshotRoot, completed: [true, false] }),
      ]),
      ...fixturePaths,
      selection: "missing",
      groupBy: "color",
      roleOrder: ["primary", "accent", "clear", "opaque"],
    });

    expect(first.kind).toBe("materialized");
    expect(second.kind).toBe("materialized");
    if (first.kind !== "materialized" || second.kind !== "materialized") return;
    expect(first.rootPath).not.toBe(second.rootPath);
    expect(first.fileCounts).toEqual({ accent: 2 });
    expect(second.fileCounts).toEqual({ accent: 1 });
  });

  it("exports more than 10,000 selected accepted units", async () => {
    const fixturePaths = fixture();
    const base = part({ snapshotRoot: fixturePaths.snapshotRoot, completed: [false] });
    const units = Array.from({ length: 10_001 }, (_, unitIndex) => ({
      token: `31:${unitIndex}`,
      unitIndex,
      completed: false,
      assembled: false,
    }));

    const measurement = measureLargeExport(dirname(fixturePaths.tenantExportsDir));
    const result = await materializeAcceptedStlBundle({
      capture: capture([{ ...base, quantityEffective: units.length, units }]),
      ...fixturePaths,
      selection: "all",
      groupBy: "color_dir",
      roleOrder: ["primary", "accent", "clear", "opaque"],
    }).then(
      (value) => {
        measurement.outcome = {
          kind: "returned",
          resultKind: value.kind,
          elapsedMs: performance.now() - measurement.start,
        };
        return value;
      },
      (error: unknown) => {
        measurement.outcome = { kind: "threw", elapsedMs: performance.now() - measurement.start };
        throw error;
      },
    );
    expect(result).toMatchObject({ kind: "materialized", fileCounts: { accent: 10_001 } });
    if (result.kind !== "materialized") return;
    const published = join(result.rootPath, "accent", "_root");
    const firstPath = join(published, "widget_01.stl");
    const secondPath = join(published, "widget_02.stl");
    const lastPath = join(published, "widget_10001.stl");
    const original = Buffer.from("accepted-stl");
    expect(readFileSync(firstPath)).toEqual(original);
    expect(readFileSync(secondPath)).toEqual(original);
    expect(readFileSync(lastPath)).toEqual(original);
    const names = readdirSync(published).filter((name) => name.endsWith(".stl"));
    const inodes = new Set<number>();
    for (const name of names) {
      const stats = statSync(join(published, name));
      expect(stats.isFile()).toBe(true);
      expect(stats.nlink).toBe(1);
      inodes.add(stats.ino);
    }
    expect(names).toHaveLength(10_001);
    expect(inodes.size).toBe(10_001);
    const edited = Buffer.from("edited-unit!");
    writeFileSync(firstPath, edited);
    expect(readFileSync(firstPath)).toEqual(edited);
    expect(readFileSync(secondPath)).toEqual(original);
    expect(readFileSync(lastPath)).toEqual(original);
    // One syncfs for the export, then the source flush plus accent/_root, accent,
    // and the stage. Copies are not flushed.
    expect(acceptedArtifactTestHook.syncCalls).toBe(1);
    expect(measurement).toMatchObject({
      flushCalls: 4,
      flushCallbacks: 4,
      flushErrors: 0,
      flushInflight: 0,
      zipSources: 10_001,
      zipEnds: 10_001,
      zipErrors: 0,
      zipBytes: 10_001 * Buffer.byteLength("accepted-stl"),
    });
    expect(measurement.firstZipSourceAt).not.toBeNull();
  }, 60_000);

  it("syncs accepted copies once after every part is placed", async () => {
    const fixturePaths = fixture();
    const measurement = flushMeasurement();
    acceptedArtifactTestHook.diagnostic.current = measurement;
    const result = await materializeAcceptedStlBundle({
      capture: capture([
        part({ snapshotRoot: fixturePaths.snapshotRoot, completed: [false, false] }),
        part({
          snapshotRoot: fixturePaths.snapshotRoot,
          revisionPartId: 32,
          filename: "peer.stl",
          completed: [false],
        }),
      ]),
      ...fixturePaths,
      selection: "all",
      groupBy: "color",
      roleOrder: ["accent"],
    });

    expect(result.kind).toBe("materialized");
    expect(acceptedArtifactTestHook.syncCalls).toBe(1);
    expect(measurement).toMatchObject({
      flushCalls: 4,
      flushCallbacks: 4,
      flushErrors: 0,
      flushInflight: 0,
    });
    if (result.kind !== "materialized") return;
    const published = join(result.rootPath, "accent");
    const paths = ["widget_01.stl", "widget_02.stl", "peer_01.stl"].map((name) =>
      join(published, name),
    );
    const inodes = new Set(
      paths.map((path) => {
        const stats = statSync(path);
        expect(stats.nlink).toBe(1);
        return stats.ino;
      }),
    );
    expect(inodes.size).toBe(3);
    writeFileSync(paths[0]!, Buffer.from("edited-unit!"));
    expect(readFileSync(paths[1]!)).toEqual(Buffer.from("accepted-stl"));
    expect(readFileSync(paths[2]!)).toEqual(Buffer.from("accepted-stl"));
  });

  it.each(["missing", "nonzero"] as const)(
    "flushes each accepted copy when sync -f is %s without blocking the event loop",
    async (syncMode) => {
      const fixturePaths = fixture();
      acceptedArtifactTestHook.syncMode = syncMode;
      const measurement = flushMeasurement();
      acceptedArtifactTestHook.diagnostic.current = measurement;
      const result = await materializeAcceptedStlBundle({
        capture: capture([
          part({ snapshotRoot: fixturePaths.snapshotRoot, completed: [false, false] }),
          part({
            snapshotRoot: fixturePaths.snapshotRoot,
            revisionPartId: 32,
            filename: "peer.stl",
            completed: [false],
          }),
        ]),
        ...fixturePaths,
        selection: "all",
        groupBy: "color",
        roleOrder: ["accent"],
      });

      expect(result.kind).toBe("materialized");
      expect(acceptedArtifactTestHook.syncCalls).toBe(1);
      expect(acceptedArtifactTestHook.syncLoopTicks).toBeGreaterThan(0);
      // Two source flushes, three copy flushes, then accent and the stage.
      expect(measurement).toMatchObject({
        flushCalls: 7,
        flushCallbacks: 7,
        flushErrors: 0,
        flushInflight: 0,
      });
      if (result.kind !== "materialized") return;
      const published = join(result.rootPath, "accent");
      const paths = ["widget_01.stl", "widget_02.stl", "peer_01.stl"].map((name) =>
        join(published, name),
      );
      const inodes = new Set(
        paths.map((path) => {
          const stats = statSync(path);
          expect(stats.nlink).toBe(1);
          return stats.ino;
        }),
      );
      expect(inodes.size).toBe(3);
      writeFileSync(paths[0]!, Buffer.from("edited-unit!"));
      expect(readFileSync(paths[1]!)).toEqual(Buffer.from("accepted-stl"));
      expect(readFileSync(paths[2]!)).toEqual(Buffer.from("accepted-stl"));
    },
  );

  it("exports more than 256 MiB of source files and 512 MiB of copies", async () => {
    const fixturePaths = fixture();
    const base = part({
      snapshotRoot: fixturePaths.snapshotRoot,
      completed: [false, false],
      bytes: Buffer.alloc(15 * 1024 * 1024, 65),
    });
    const parts = Array.from({ length: 18 }, (_, index) => ({
      ...base,
      revisionPartId: index + 1,
      projectionPartId: index + 101,
      partKey: `part:${index + 1}`,
      artifact:
        base.artifact.kind === "tracked"
          ? { ...base.artifact, sourceId: index + 1 }
          : base.artifact,
    }));

    await expect(
      materializeAcceptedStlBundle({
        capture: capture(parts),
        ...fixturePaths,
        selection: "all",
        groupBy: "color",
        roleOrder: ["primary", "accent", "clear", "opaque"],
      }),
    ).resolves.toMatchObject({ kind: "materialized", fileCounts: { accent: 36 } });
  }, 60_000);

  it("publishes the ZIP alongside the exported files", async () => {
    const fixturePaths = fixture();
    const bytes = Buffer.from(Array.from({ length: 80 }, (_, index) => index));

    await expect(
      materializeAcceptedStlBundle({
        capture: capture([
          part({ snapshotRoot: fixturePaths.snapshotRoot, bytes, completed: [false] }),
        ]),
        ...fixturePaths,
        selection: "all",
        groupBy: "color",
        roleOrder: ["primary", "accent", "clear", "opaque"],
      }),
    ).resolves.toMatchObject({ kind: "materialized" });
  });

  it("maps publication setup failures to output_failure", async () => {
    const fixturePaths = fixture();
    writeFileSync(fixturePaths.tenantExportsDir, "not a directory");

    await expect(
      materializeAcceptedStlBundle({
        capture: capture([part({ snapshotRoot: fixturePaths.snapshotRoot, completed: [false] })]),
        ...fixturePaths,
        selection: "all",
        groupBy: "color",
        roleOrder: ["primary", "accent", "clear", "opaque"],
      }),
    ).resolves.toEqual({ kind: "output_failure" });
  });

  it("keeps a configured traversal role inside the private publication tree", async () => {
    const fixturePaths = fixture();
    const base = part({ snapshotRoot: fixturePaths.snapshotRoot, completed: [false] });
    const result = await materializeAcceptedStlBundle({
      capture: capture([{ ...base, role: ".." }]),
      ...fixturePaths,
      selection: "all",
      groupBy: "color_dir",
      roleOrder: [".."],
    });

    expect(result.kind).toBe("materialized");
    if (result.kind !== "materialized") return;
    expect(readFileSync(join(result.rootPath, "_root", "_root", "widget_01.stl"))).toEqual(
      Buffer.from("accepted-stl"),
    );
    expect(() => readFileSync(join(result.rootPath, "..", "widget_01.stl"))).toThrow();
  });

  it("exports only the Required units named by unitTokens", async () => {
    const fixturePaths = fixture();
    const bytes = Buffer.from("named unit bytes");
    const captured = capture([
      part({
        snapshotRoot: fixturePaths.snapshotRoot,
        bytes,
        completed: [false, false, false],
      }),
    ]);

    const result = await materializeAcceptedStlBundle({
      capture: captured,
      ...fixturePaths,
      selection: "all",
      groupBy: "color",
      roleOrder: ["primary", "accent", "clear", "opaque"],
      unitTokens: [unitToken(31, 0), unitToken(31, 2)],
    });

    expect(result.kind).toBe("materialized");
    if (result.kind !== "materialized") return;
    expect(result.fileCounts).toEqual({ accent: 2 });
    expect(
      new AdmZip(result.bundlePath ?? "").getEntries().map((entry) => entry.entryName),
    ).toEqual(["accent/widget_01.stl", "accent/widget_03.stl"]);
    expect(readFileSync(join(result.rootPath, "accent", "widget_01.stl"))).toEqual(bytes);
    expect(() => readFileSync(join(result.rootPath, "accent", "widget_02.stl"))).toThrow();
  });

  it("narrows named Required units further when only missing ones are wanted", async () => {
    const fixturePaths = fixture();
    const captured = capture([
      part({ snapshotRoot: fixturePaths.snapshotRoot, completed: [false, true, false] }),
    ]);

    const result = await materializeAcceptedStlBundle({
      capture: captured,
      ...fixturePaths,
      selection: "missing",
      groupBy: "color",
      roleOrder: ["primary", "accent", "clear", "opaque"],
      unitTokens: [unitToken(31, 1), unitToken(31, 2)],
    });

    expect(result.kind).toBe("materialized");
    if (result.kind !== "materialized") return;
    expect(result.fileCounts).toEqual({ accent: 1 });
    expect(() => readFileSync(join(result.rootPath, "accent", "widget_03.stl"))).not.toThrow();
  });

  it("drops tokens the accepted revision does not hold and keeps the rest", async () => {
    const fixturePaths = fixture();
    const captured = capture([
      part({ snapshotRoot: fixturePaths.snapshotRoot, completed: [false, false] }),
    ]);

    const result = await materializeAcceptedStlBundle({
      capture: captured,
      ...fixturePaths,
      selection: "all",
      groupBy: "color",
      roleOrder: ["primary", "accent", "clear", "opaque"],
      unitTokens: [unitToken(31, 1), `ppu_${"f".repeat(32)}`],
    });

    expect(result.kind).toBe("materialized");
    if (result.kind !== "materialized") return;
    expect(result.fileCounts).toEqual({ accent: 1 });
    expect(() => readFileSync(join(result.rootPath, "accent", "widget_02.stl"))).not.toThrow();
  });

  it("refuses an export whose Required units are all unknown", async () => {
    const fixturePaths = fixture();

    await expect(
      materializeAcceptedStlBundle({
        capture: capture([
          part({ snapshotRoot: fixturePaths.snapshotRoot, completed: [false, false] }),
        ]),
        ...fixturePaths,
        selection: "all",
        groupBy: "color",
        roleOrder: ["primary", "accent", "clear", "opaque"],
        unitTokens: [`ppu_${"a".repeat(32)}`, `ppu_${"b".repeat(32)}`],
      }),
    ).resolves.toEqual({ kind: "unknown_unit_tokens" });
  });

  it("publishes two unit selections that share a file list under separate keys", async () => {
    const fixturePaths = fixture();
    const unavailable = () =>
      capture([
        part({
          snapshotRoot: fixturePaths.snapshotRoot,
          completed: [false, false],
          unavailable: true,
        }),
      ]);

    const first = await materializeAcceptedStlBundle({
      capture: unavailable(),
      ...fixturePaths,
      selection: "all",
      groupBy: "color",
      roleOrder: ["primary", "accent", "clear", "opaque"],
      unitTokens: [unitToken(31, 0)],
    });
    const second = await materializeAcceptedStlBundle({
      capture: unavailable(),
      ...fixturePaths,
      selection: "all",
      groupBy: "color",
      roleOrder: ["primary", "accent", "clear", "opaque"],
      unitTokens: [unitToken(31, 1)],
    });

    expect(first.kind).toBe("materialized");
    expect(second.kind).toBe("materialized");
    if (first.kind !== "materialized" || second.kind !== "materialized") return;
    // Same warning, same empty file list. Only the named units tell them apart.
    expect(first.warnings).toEqual(second.warnings);
    expect(first.fileCounts).toEqual({});
    expect(first.rootPath).not.toBe(second.rootPath);
  });

  it("keeps the publication key it published under before unitTokens existed", async () => {
    const fixturePaths = fixture();
    const captured = capture([
      part({ snapshotRoot: fixturePaths.snapshotRoot, completed: [false, true] }),
    ]);

    const result = await materializeAcceptedStlBundle({
      capture: captured,
      ...fixturePaths,
      selection: "all",
      groupBy: "color",
      roleOrder: ["primary", "accent", "clear", "opaque"],
    });
    const empty = await materializeAcceptedStlBundle({
      capture: captured,
      ...fixturePaths,
      selection: "all",
      groupBy: "color",
      roleOrder: ["primary", "accent", "clear", "opaque"],
      unitTokens: [],
    });

    expect(result.kind).toBe("materialized");
    expect(empty.kind).toBe("materialized");
    if (result.kind !== "materialized" || empty.kind !== "materialized") return;
    expect(basename(result.rootPath)).toBe(
      `content-${keyBeforeUnitTokens(result.rootPath, result.warnings)}`,
    );
    expect(empty.rootPath).toBe(result.rootPath);
  });
});

describe("parseStlPackUnitTokens", () => {
  it("accepts an absent field and a list of Required-unit tokens", () => {
    expect(parseStlPackUnitTokens(undefined)).toEqual([]);
    expect(parseStlPackUnitTokens(null)).toEqual([]);
    expect(parseStlPackUnitTokens([])).toEqual([]);
    const token = unitToken(31, 0);
    expect(parseStlPackUnitTokens([token])).toEqual([token]);
  });

  it("refuses invalid tokens but accepts more than 10,000 valid tokens", () => {
    expect(parseStlPackUnitTokens("ppu_" + "a".repeat(32))).toBe("invalid");
    expect(parseStlPackUnitTokens([42])).toBe("invalid");
    expect(parseStlPackUnitTokens(["31:0"])).toBe("invalid");
    expect(parseStlPackUnitTokens([`ppu_${"A".repeat(32)}`])).toBe("invalid");
    expect(
      parseStlPackUnitTokens(Array.from({ length: 10_001 }, () => unitToken(31, 0))),
    ).toHaveLength(10_001);
  });
});
