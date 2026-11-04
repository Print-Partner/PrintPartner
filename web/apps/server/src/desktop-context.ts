import { createHmac, timingSafeEqual } from "node:crypto";
import {
  closeSync,
  existsSync,
  fstatSync,
  linkSync,
  lstatSync,
  mkdirSync,
  openSync,
  realpathSync,
  renameSync,
  rmdirSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { dirname, join } from "node:path";
import type { FastifyInstance, FastifyRequest, FastifyReply } from "fastify";
import { z } from "zod";
import { admitDesktopAccount, accountModeSchema, type DesktopAccount } from "./desktop-account.js";
import { admitDesktopDataDirectory } from "./desktop-child-credential.js";
import {
  acquireStorageLease,
  ownerIsStale,
  readOwnershipRecord,
  recordNamesInstance,
  storageParentIdentityEnv,
} from "./storage-lease.js";
import type { SessionUser } from "./routes/auth-types.js";

const baseSetupSchema = z.strictObject({
  version: z.literal(1),
  data_dir: z.string().min(1),
  socket_path: z.string().min(1),
  generation: z.string().regex(/^[a-f0-9]{32}$/),
  key: z.string().regex(/^[a-f0-9]{64}$/),
  parent_pid: z.number().int().positive(),
  port: z.number().int().min(1).max(65535),
  gateway_rate_limit: z.boolean().optional(),
});
const setupSchema = z.union([
  baseSetupSchema.extend({ account_mode: accountModeSchema }),
  baseSetupSchema,
]);
const assertionSchema = z.strictObject({
  version: z.literal(1),
  generation: z.string(),
  tenant: z.literal("default"),
  actor: z.literal("desktop-owner"),
  method: z.string(),
  target: z.string(),
  issued_at: z.number().int().nonnegative(),
});
let context: z.infer<typeof setupSchema> | null = null;
let account: DesktopAccount = { kind: "synthetic" };
const principals = new WeakMap<FastifyRequest, SessionUser>();

export function desktopContext() {
  return (
    context && {
      data_dir: context.data_dir,
      socket_path: context.socket_path,
      parent_pid: context.parent_pid,
      port: context.port,
      gateway_rate_limit: context.gateway_rate_limit === true,
    }
  );
}

const quotaSchema = z.strictObject({
  remaining: z.number().int().min(0).max(1000),
  reset: z.number().int().min(1).max(60),
  exceeded: z.boolean(),
});

// Only routes using the global limiter call this hook. Fastify retains ownership
// of stricter route-specific limits and health remains exempt.
export async function desktopRateLimit(
  request: FastifyRequest,
  reply: FastifyReply,
): Promise<void> {
  const setup = context;
  if (!setup?.gateway_rate_limit || /^(?:\/api\/v[12])?\/health(?:\?|$)/.test(request.url)) return;
  const path = request.url.split("?", 1)[0];
  const media = /^(?:\/api\/v[12])?\/parts\/\d+\/(?:mesh|thumbnail)$/.test(path);
  const encoded = Buffer.from(
    JSON.stringify({
      generation: setup.generation,
      bucket: media ? "media" : "app",
      issued_at: Date.now(),
    }),
  ).toString("hex");
  const signature = createHmac("sha256", Buffer.from(setup.key, "hex"))
    .update(encoded)
    .digest("hex");
  let quota: z.infer<typeof quotaSchema>;
  try {
    const response = await fetch(`http://127.0.0.1:${setup.port}/__desktop/rate-limit`, {
      method: "POST",
      headers: { "x-pp-rate-debit": encoded, "x-pp-rate-signature": signature },
      signal: AbortSignal.timeout(5000),
    });
    if (!response.ok) throw new Error("Quota debit rejected");
    quota = quotaSchema.parse(await response.json());
  } catch {
    throw Object.assign(new Error("Desktop rate limit unavailable"), { statusCode: 503 });
  }
  reply
    .header("x-ratelimit-limit", 1000)
    .header("x-ratelimit-remaining", quota.remaining)
    .header("x-ratelimit-reset", quota.reset);
  if (quota.exceeded) {
    reply.header("retry-after", quota.reset);
    const after =
      quota.reset === 60 ? "1 minute" : `${quota.reset} second${quota.reset === 1 ? "" : "s"}`;
    throw Object.assign(new Error(`Rate limit exceeded, retry in ${after}`), { statusCode: 429 });
  }
}

export function installDesktopContext(input: unknown): void {
  if (context) throw new Error("Desktop context already installed");
  const parsed = setupSchema.parse(input);
  const dataDir = realpathSync(parsed.data_dir);
  if (process.env.PP_STORAGE_CORE_LOCK !== "1") throw new Error("Core storage lock required");
  const inherited = fstatSync(198);
  const ownerLock = statSync(join(dataDir, "printpartner.lock"));
  if (!inherited.isFile() || inherited.dev !== ownerLock.dev || inherited.ino !== ownerLock.ino)
    throw new Error("Inherited storage lock rejected");
  if (parsed.parent_pid !== process.ppid || parsed.data_dir !== dataDir)
    throw new Error("Desktop storage owner rejected");
  account = admitDesktopAccount(
    dataDir,
    "account_mode" in parsed ? parsed.account_mode.pin : { kind: "disabled" },
  );
  context = parsed;
  admitDesktopDataDirectory(dataDir);
}

/** Remove the private socket after the desktop parent dies. */
export function releaseOrphanedDesktopOwner(setup: {
  data_dir: string;
  socket_path: string;
  parent_pid: number;
}): void {
  try {
    rmSync(setup.socket_path, { force: true });
    rmdirSync(dirname(setup.socket_path));
  } catch {
    /* Cleanup can race the parent-death watcher. */
  }
}

// Rust writes this marker when it takes a data directory over. Any entry at this name
// refuses standalone Node, whatever it holds, so a rollback cannot write into Rust data.
const rustOwnershipMarker = ".pp-rust-owned.json";

function assertNotRustOwned(dataDir: string): void {
  try {
    lstatSync(join(dataDir, rustOwnershipMarker));
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return;
    throw error;
  }
  throw new Error(
    `Data directory is owned by the Rust runtime (${rustOwnershipMarker}); standalone Node will not open it`,
  );
}

function assertDataDirectoryAvailable(dataDir: string): void {
  if (!existsSync(dataDir)) return;
  const canonical = realpathSync(dataDir);
  if (!existsSync(join(canonical, ".desktop-owner.json"))) return;
  if (context?.data_dir === canonical) return;
  // Under the shared storage lease, recover stale markers from either runtime.
  const owner = z
    .object({
      pid: z.number().int().positive().max(2147483647),
      process_identity: z.string().optional(),
      kind: z.string().optional(),
      storage_protocol_version: z.unknown().optional(),
      child_pid: z.number().int().positive().max(2147483647).optional(),
      child_process_identity: z.string().min(1).optional(),
    })
    .safeParse(JSON.parse(readOwnershipRecord(join(canonical, ".desktop-owner.json"))));
  if (
    owner.success &&
    ownerIsStale(owner.data) &&
    (owner.data.kind === "standalone" ||
      // Version 1 guarantees recording the child before setup opens storage.
      (owner.data.storage_protocol_version === 1 &&
        owner.data.child_pid === undefined &&
        owner.data.child_process_identity === undefined) ||
      (owner.data.child_pid !== undefined &&
        owner.data.child_process_identity !== undefined &&
        ownerIsStale({
          pid: owner.data.child_pid,
          process_identity: owner.data.child_process_identity,
        })))
  ) {
    renameSync(
      join(canonical, ".desktop-owner.json"),
      join(canonical, ".desktop-lease", "previous-marker.json"),
    );
    return;
  }
  throw new Error(
    "Data directory is already owned; stop the other process before opening another writer",
  );
}

export function mappedDesktopTenant(): string | null {
  if (account.kind !== "existing") return null;
  return account.mapping.tenant_id;
}

export function mappedDesktopDatabaseAvailable(): boolean {
  if (account.kind !== "existing") return true;
  try {
    account.assertDatabase();
    return true;
  } catch {
    return false;
  }
}

export function closeDesktopAccount(): void {
  if (account.kind === "existing") account.close();
}

export function desktopPrincipal(request: FastifyRequest): SessionUser | null {
  return principals.get(request) ?? null;
}

export function registerDesktopBoundary(app: FastifyInstance): void {
  const setup = context;
  if (!setup) return;
  app.addHook("onRequest", async (request, reply) => {
    const encoded = request.headers["x-pp-principal"];
    const signature = request.headers["x-pp-signature"];
    delete request.headers["x-pp-principal"];
    delete request.headers["x-pp-signature"];
    try {
      const principal = verifyDesktopPrincipal(
        { encoded, signature, method: request.method, target: request.url, now: Date.now() },
        setup,
      );
      principals.set(
        request,
        account.kind === "existing"
          ? account.principal()
          : {
              user_id: principal.actor,
              tenant_id: principal.tenant,
              login: "desktop",
              display_name: "Desktop owner",
              email: null,
              provider: "desktop",
              is_admin: true,
            },
      );
    } catch {
      return reply.status(401).send({ detail: "Valid desktop principal required" });
    }
  });
}

export function verifyDesktopPrincipal(
  request: { encoded: unknown; signature: unknown; method: string; target: string; now: number },
  setup: { key: string; generation: string },
) {
  const { encoded, signature } = request;
  if (
    typeof encoded !== "string" ||
    typeof signature !== "string" ||
    !/^[a-f0-9]{64}$/.test(signature) ||
    !/^[a-f0-9]+$/.test(encoded) ||
    encoded.length > 4096
  ) {
    throw new Error("Desktop principal rejected");
  }
  const expected = createHmac("sha256", Buffer.from(setup.key, "hex")).update(encoded).digest();
  if (!timingSafeEqual(expected, Buffer.from(signature, "hex")))
    throw new Error("Desktop principal rejected");
  const principal = assertionSchema.parse(JSON.parse(Buffer.from(encoded, "hex").toString("utf8")));
  const age = request.now - principal.issued_at;
  if (
    principal.generation !== setup.generation ||
    principal.method !== request.method ||
    principal.target !== request.target ||
    age < -1000 ||
    age > 30_000
  )
    throw new Error("Desktop principal rejected");
  return principal;
}

type HeldDataDirectory = { refs: number; identity: string; release: () => void };
const heldDataDirectories = new Map<string, HeldDataDirectory>();

function heldRelease(held: HeldDataDirectory): () => void {
  let released = false;
  return () => {
    if (released) return;
    if (held.refs === 1) held.release();
    held.refs--;
    released = true;
  };
}

// Connections share the lease this process already holds for the copy, so an
// open SqliteDatabase keeps the copy reserved even without acquireDataDirectory.
export function retainDataDirectory(dataDir: string): () => void {
  if (!context) {
    mkdirSync(dataDir, { recursive: true });
    const canonical = realpathSync(dataDir);
    const held = heldDataDirectories.get(canonical);
    if (held) {
      held.refs++;
      return heldRelease(held);
    }
  }
  return acquireDataDirectory(dataDir);
}

/** Environment for a worker process that opens a copy this process holds with the "parent" lease. */
export function storageWorkerEnv(dataDir: string): Record<string, string> {
  const held = heldDataDirectories.get(realpathSync(dataDir));
  if (!held) throw new Error("This process does not hold the data directory");
  return { [storageParentIdentityEnv]: held.identity };
}

export function acquireDataDirectory(dataDir: string): () => void {
  if (context) {
    if (realpathSync(dataDir) !== context.data_dir)
      throw new Error("Desktop storage lease rejected");
    return () => {};
  }
  mkdirSync(dataDir, { recursive: true });
  const canonical = realpathSync(dataDir);
  const lease = acquireStorageLease(canonical);
  const marker = join(canonical, ".desktop-owner.json");
  try {
    assertNotRustOwned(canonical);
    assertDataDirectoryAvailable(canonical);
    const fd = openSync(marker, "wx", 0o600);
    // The instance tells this reservation's marker apart from another one in the same process.
    try {
      writeFileSync(
        fd,
        JSON.stringify({
          pid: process.pid,
          kind: "standalone",
          process_identity: lease.identity,
          instance: lease.instance,
        }),
      );
    } catch (error) {
      rmSync(marker);
      throw error;
    } finally {
      closeSync(fd);
    }
  } catch (error) {
    lease.release();
    throw error;
  }
  const detachedMarker = join(canonical, `.desktop-owner-release-${lease.instance}.json`);
  let markerStage: "recorded" | "detached" | "checked" | "removed" = "recorded";
  const held: HeldDataDirectory = {
    refs: 1,
    identity: lease.identity,
    release: () => {
      // Preserve ownership if marker cleanup fails, so callers can retry safely.
      if (markerStage === "recorded") {
        // A marker removed or rewritten by someone else is no longer ours to remove.
        if (!recordNamesInstance(marker, lease.instance)) markerStage = "removed";
        else if (!lease.releaseRecoveryEnabled) {
          rmSync(marker, { force: true });
          markerStage = "removed";
        } else {
          try {
            renameSync(marker, detachedMarker);
            markerStage = "detached";
          } catch (error) {
            if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
            markerStage = "removed";
          }
        }
      }
      if (markerStage === "detached") {
        // The check and the rename cannot be one step, so hand back a marker rewritten in between.
        // Neither link nor a directory rename replaces an entry republished since.
        // A detached marker already removed by someone else leaves nothing to hand back.
        if (!recordNamesInstance(detachedMarker, lease.instance)) {
          try {
            if (lstatSync(detachedMarker).isDirectory()) renameSync(detachedMarker, marker);
            else linkSync(detachedMarker, marker);
          } catch (error) {
            if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
          }
        }
        markerStage = "checked";
      }
      if (markerStage === "checked") {
        rmSync(detachedMarker, { force: true });
        markerStage = "removed";
      }
      // Once the marker is gone this entry no longer stands for a reservation, even
      // if detaching the lease fails. A later connection then has to acquire again.
      if (heldDataDirectories.get(canonical) === held) heldDataDirectories.delete(canonical);
      lease.release();
    },
  };
  heldDataDirectories.set(canonical, held);
  return heldRelease(held);
}
