import { spawn, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  existsSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  readlinkSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import Database from "better-sqlite3";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { startServer } from "./app.js";
import { loadConfig } from "./config.js";
import { SqliteDatabase } from "./db/client.js";
import { acquireDataDirectory, retainDataDirectory } from "./desktop-context.js";
import { executeOwnershipCopy, inspectOwnershipCopy } from "./ownership-migration.js";
import { processIdentity } from "./storage-lease.js";

const sourceDir = dirname(fileURLToPath(import.meta.url));
const tsx = join(sourceDir, "../../../node_modules/tsx/dist/cli.mjs");
const refusal = /owned by the Rust runtime \(\.pp-rust-owned\.json\)/;
const validMarker = '{"version":1,"owner":"rust","created_at":"2026-11-03T00:00:00Z"}\n';

const envKeys = [
  "PRINT_PARTNER_DATA_DIR",
  "PRINT_PARTNER_API_KEY",
  "HOST",
  "PORT",
  "DEPLOY_MODE",
  "PP_DESKTOP_REBUILD",
] as const;
const previousEnv = Object.fromEntries(envKeys.map((key) => [key, process.env[key]]));
const roots: string[] = [];

function scratch(): string {
  const root = mkdtempSync(join(tmpdir(), "pp-rust-marker-"));
  roots.push(root);
  return root;
}

// The PID of an exited process, for owner records that name a dead owner.
function deadPid(): number {
  const { pid, status } = spawnSync("true");
  if (status !== 0 || !pid) throw new Error("dead pid unavailable");
  return pid;
}

function serverEnv(dataDir: string): NodeJS.ProcessEnv {
  return {
    ...process.env,
    DEPLOY_MODE: "self-host",
    HOST: "127.0.0.1",
    PORT: "0",
    PRINT_PARTNER_DATA_DIR: dataDir,
    PRINT_PARTNER_API_KEY: "test-rust-marker-key",
  };
}

// Runs a standalone entrypoint script with tsx, as `npm run mcp` and `npm run db:migrate` do.
function runEntrypoint(script: string, dataDir: string) {
  const result = spawnSync(process.execPath, [tsx, join(sourceDir, script)], {
    encoding: "utf8",
    timeout: 30_000,
    input: "",
    env: serverEnv(dataDir),
  });
  return { ...result, output: `${result.stdout}\n${result.stderr}` };
}

beforeEach(() => {
  delete process.env.PP_DESKTOP_REBUILD;
});

afterEach(() => {
  for (const key of envKeys) {
    const value = previousEnv[key];
    if (value === undefined) delete process.env[key];
    else process.env[key] = value;
  }
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true });
});

const markerShapes: Record<string, (path: string) => void> = {
  valid: (path) => writeFileSync(path, validMarker, { mode: 0o600 }),
  unparseable: (path) => writeFileSync(path, '{"version":1,"owner":"ru'),
  "unknown version": (path) =>
    writeFileSync(path, '{"version":2,"owner":"rust","created_at":"2026-11-03T00:00:00Z"}\n'),
  "wrong owner": (path) =>
    writeFileSync(path, '{"version":1,"owner":"node","created_at":"2026-11-03T00:00:00Z"}\n'),
  empty: (path) => writeFileSync(path, ""),
  directory: (path) => mkdirSync(path),
  "dangling symlink": (path) => symlinkSync(join(dirname(path), "missing"), path),
};

/**
 * A data directory Node already used: a SQLite database, product files and a crash-left
 * standalone owner record that assertDataDirectoryAvailable() would otherwise rename.
 */
function markedInstall(shape: keyof typeof markerShapes = "valid"): string {
  const dataDir = join(scratch(), "data");
  mkdirSync(join(dataDir, "repos", "1"), { recursive: true });
  const sqlite = new Database(join(dataDir, "print-partner.db"));
  sqlite.exec("CREATE TABLE app_settings (key TEXT PRIMARY KEY, value TEXT)");
  sqlite.close();
  writeFileSync(join(dataDir, "repos", "1", "part.stl"), "solid part\nendsolid part\n");
  writeFileSync(
    join(dataDir, ".desktop-owner.json"),
    JSON.stringify({ pid: deadPid(), kind: "standalone", process_identity: "prior" }),
  );
  markerShapes[shape](join(dataDir, ".pp-rust-owned.json"));
  return dataDir;
}

// Every entry except the lease files acquireStorageLease() may create and retire.
function snapshot(dataDir: string): Record<string, string> {
  const entries: Record<string, string> = {};
  const walk = (path: string, relative: string) => {
    const entry = lstatSync(path);
    if (entry.isSymbolicLink()) entries[relative] = `link:${readlinkSync(path)}`;
    else if (entry.isDirectory()) {
      entries[relative] = `dir:${entry.mode}`;
      for (const name of readdirSync(path).sort()) walk(join(path, name), join(relative, name));
    } else {
      const digest = createHash("sha256").update(readFileSync(path)).digest("hex");
      entries[relative] = `file:${entry.mode}:${digest}`;
    }
  };
  for (const name of readdirSync(dataDir).sort()) {
    if (name.startsWith(".desktop-lease")) continue;
    walk(join(dataDir, name), name);
  }
  return entries;
}

function expectUnchanged(dataDir: string, before: Record<string, string>): void {
  expect(snapshot(dataDir)).toEqual(before);
  expect(readdirSync(dataDir).filter((name) => name.startsWith(".desktop-lease"))).toEqual([]);
}

describe("standalone Node refuses a Rust-marked data directory", () => {
  it.each(Object.keys(markerShapes))("refuses a %s marker and releases the lease", (shape) => {
    const dataDir = markedInstall(shape);
    const before = snapshot(dataDir);
    expect(() => acquireDataDirectory(dataDir)).toThrow(refusal);
    expectUnchanged(dataDir, before);
    expect(() => acquireDataDirectory(dataDir)).toThrow(refusal);
    expectUnchanged(dataDir, before);
  });

  it("refuses startServer before upgrade, migration or listening", async () => {
    const dataDir = markedInstall();
    const before = snapshot(dataDir);
    Object.assign(process.env, serverEnv(dataDir));
    await expect(startServer(loadConfig())).rejects.toThrow(refusal);
    expect(existsSync(join(dataDir, "backups"))).toBe(false);
    expectUnchanged(dataDir, before);
  });

  it("refuses MCP stdio before opening the database", () => {
    const dataDir = markedInstall();
    const before = snapshot(dataDir);
    const started = runEntrypoint("mcp/stdio-server.ts", dataDir);
    expect(started.status, started.output).toBe(1);
    expect(started.stderr).toMatch(refusal);
    expectUnchanged(dataDir, before);
  });

  it("refuses db:migrate before opening the database", () => {
    const dataDir = markedInstall();
    const before = snapshot(dataDir);
    const migrated = runEntrypoint("db/migrate.ts", dataDir);
    expect(migrated.status, migrated.output).not.toBe(0);
    expect(migrated.stderr).toMatch(refusal);
    expect(migrated.stdout).not.toMatch(/Migrated SQLite database/);
    expectUnchanged(dataDir, before);
  });

  it("refuses ownership migration inspect and apply", async () => {
    process.env.PP_DESKTOP_REBUILD = "1";
    const dataDir = markedInstall();
    const destinationDir = join(dirname(dataDir), "copy");
    const before = snapshot(dataDir);
    expect(() => inspectOwnershipCopy({ sourceDir: dataDir })).toThrow(refusal);
    expectUnchanged(dataDir, before);
    await expect(
      executeOwnershipCopy({
        sourceDir: dataDir,
        destinationDir,
        manifest: {
          version: 1,
          source_digest: "0".repeat(64),
          selected_user_id: "account-a",
          claims: [],
        },
      }),
    ).rejects.toThrow(refusal);
    expect(existsSync(destinationDir)).toBe(false);
    expectUnchanged(dataDir, before);
  });

  it("refuses SqliteDatabase.connect() through retainDataDirectory", () => {
    const dataDir = markedInstall();
    const before = snapshot(dataDir);
    expect(() => retainDataDirectory(dataDir)).toThrow(refusal);
    expectUnchanged(dataDir, before);
    const database = new SqliteDatabase(dataDir);
    expect(() => database.connect()).toThrow(refusal);
    expectUnchanged(dataDir, before);
  });
});

describe("standalone Node without a Rust marker", () => {
  it("starts a plain Node install and releases it on close", async () => {
    const dataDir = join(scratch(), "data");
    Object.assign(process.env, serverEnv(dataDir));
    const { app, ports } = await startServer(loadConfig());
    try {
      expect(existsSync(join(dataDir, "print-partner.db"))).toBe(true);
      expect(JSON.parse(readFileSync(join(dataDir, ".desktop-owner.json"), "utf8"))).toMatchObject({
        pid: process.pid,
        kind: "standalone",
      });
      const health = await app.inject({ method: "GET", url: "/health" });
      expect(health.statusCode).toBe(200);
    } finally {
      await app.close();
      await ports.db.close();
    }
    expect(existsSync(join(dataDir, ".desktop-owner.json"))).toBe(false);
    expect(existsSync(join(dataDir, ".desktop-lease"))).toBe(false);
    expect(existsSync(join(dataDir, ".pp-rust-owned.json"))).toBe(false);
  });

  it("migrates a plain Node install with db:migrate", () => {
    const dataDir = join(scratch(), "data");
    const migrated = runEntrypoint("db/migrate.ts", dataDir);
    expect(migrated.status, migrated.output).toBe(0);
    expect(migrated.stdout).toMatch(/Migrated SQLite database/);
    expect(existsSync(join(dataDir, ".pp-rust-owned.json"))).toBe(false);
  });

  // Off Linux a foreign PID has no readable identity; a non-empty one keeps the lease live.
  const liveIdentity = (pid: number) =>
    process.platform === "linux" ? processIdentity(pid) : `instance:${"ef".repeat(16)}`;
  const leaseOwner = (pid: number, identity: string) =>
    JSON.stringify({ pid, process_identity: identity, instance: "ab".repeat(16) });

  // Each shape keeps the outcome it had before the marker check existed.
  const ownerAndLeaseShapes: Array<{
    name: string;
    write: (dataDir: string, livePid: number) => void;
    outcome: "acquired" | RegExp | typeof SyntaxError;
  }> = [
    { name: "empty directory", write: () => {}, outcome: "acquired" },
    {
      name: "stale standalone owner record",
      write: (dataDir) =>
        writeFileSync(
          join(dataDir, ".desktop-owner.json"),
          JSON.stringify({ pid: deadPid(), kind: "standalone", process_identity: "prior" }),
        ),
      outcome: "acquired",
    },
    {
      name: "stale core owner record without child",
      write: (dataDir) =>
        writeFileSync(
          join(dataDir, ".desktop-owner.json"),
          JSON.stringify({
            storage_protocol_version: 1,
            pid: deadPid(),
            process_identity: "prior",
          }),
        ),
      outcome: "acquired",
    },
    {
      name: "live owner record",
      write: (dataDir, livePid) =>
        writeFileSync(join(dataDir, ".desktop-owner.json"), JSON.stringify({ pid: livePid })),
      outcome: /already owned/,
    },
    {
      name: "unparseable owner record",
      write: (dataDir) => writeFileSync(join(dataDir, ".desktop-owner.json"), "{"),
      outcome: SyntaxError,
    },
    {
      name: "owner record directory",
      write: (dataDir) => mkdirSync(join(dataDir, ".desktop-owner.json")),
      outcome: /not a regular file/,
    },
    {
      name: "Rust regular-file lease",
      write: (dataDir) => writeFileSync(join(dataDir, ".desktop-lease"), ""),
      outcome: /storage_lease_marker_invalid/,
    },
    {
      name: "live Node lease directory",
      write: (dataDir, livePid) => {
        mkdirSync(join(dataDir, ".desktop-lease"));
        writeFileSync(
          join(dataDir, ".desktop-lease", "owner.json"),
          leaseOwner(livePid, liveIdentity(livePid)),
        );
      },
      outcome: /already owned/,
    },
    {
      name: "stale Node lease directory",
      write: (dataDir) => {
        mkdirSync(join(dataDir, ".desktop-lease"));
        writeFileSync(
          join(dataDir, ".desktop-lease", "owner.json"),
          leaseOwner(deadPid(), "prior"),
        );
      },
      outcome: "acquired",
    },
    {
      name: "malformed Node lease directory",
      write: (dataDir) => {
        mkdirSync(join(dataDir, ".desktop-lease"));
        writeFileSync(join(dataDir, ".desktop-lease", "owner.json"), "{");
      },
      outcome: /storage_lease_marker_invalid/,
    },
    {
      name: "empty Node lease directory",
      write: (dataDir) => mkdirSync(join(dataDir, ".desktop-lease")),
      outcome: /storage_lease_marker_invalid/,
    },
    {
      name: "lease and owner leftovers",
      write: (dataDir) => {
        for (const name of ["candidate", "release", "retired"])
          mkdirSync(join(dataDir, `.desktop-lease-${name}-${"cd".repeat(16)}`));
        writeFileSync(join(dataDir, `.desktop-owner-release-${"cd".repeat(16)}.json`), "{}");
        writeFileSync(join(dataDir, "printpartner.lock"), "");
      },
      outcome: "acquired",
    },
  ];

  it.each(ownerAndLeaseShapes)("stays silent for a $name", ({ write, outcome }) => {
    const dataDir = join(scratch(), "data");
    mkdirSync(dataDir);
    // Spawned, so the live owner is never this process and never PID-reused during the test.
    const live = spawn("sleep", ["30"]);
    try {
      if (typeof live.pid !== "number") throw new Error("live owner unavailable");
      write(dataDir, live.pid);
      let error: unknown;
      let release: (() => void) | undefined;
      try {
        release = acquireDataDirectory(dataDir);
      } catch (caught) {
        error = caught;
      }
      release?.();
      expect(String(error ?? "")).not.toMatch(/pp-rust-owned|Rust runtime/);
      if (outcome === "acquired") expect(error).toBeUndefined();
      else if (outcome === SyntaxError) expect(error).toBeInstanceOf(SyntaxError);
      else expect(String(error)).toMatch(outcome as RegExp);
    } finally {
      live.kill();
    }
  });
});
