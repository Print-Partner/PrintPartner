import { createHash, randomUUID } from "node:crypto";
import {
  closeSync,
  constants,
  existsSync,
  fstatSync,
  fsyncSync,
  mkdirSync,
  openSync,
  readFileSync,
  linkSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

/**
 * Node owns Source Sync until a committed cutover record gives the whole
 * operation to Rust. Then Node fences every participant and never routes back.
 */
export const SOURCE_SYNC_OPERATION = "source.sync";
const SYNC_RESOURCES = [
  "table:projects",
  "table:source_revisions",
  "table:source_docs",
  "file:source-workspace",
  "file:source-covers",
] as const;
const UPLOAD_RESOURCES = [...SYNC_RESOURCES, "table:build_profiles"] as const;
const UPDATE_RESOURCES = ["table:projects", "table:app_settings", "table:app_events"] as const;
const DOC_RESOURCES = ["table:source_docs", "file:source-workspace"] as const;
const INDEX_RESOURCES = ["table:source_docs"] as const;
const COVER_RESOURCES = ["file:source-covers"] as const;
const LOCAL_RESOURCES = [
  "table:projects",
  "table:source_revisions",
  "table:source_docs",
  "file:source-workspace",
  "table:build_profiles",
] as const;
const CREATE_RESOURCES = ["table:projects", "table:source_revisions"] as const;
const MIGRATION_RESOURCES = [
  "table:projects",
  "table:source_revisions",
  "file:source-workspace",
] as const;
const WORKER_RESOURCES = [
  "table:projects",
  "table:source_revisions",
  "table:source_docs",
  "file:source-workspace",
  "file:source-covers",
  "table:app_settings",
  "table:app_events",
] as const;
const SOURCE_SYNC_PARTICIPANT_SPECS = {
  "http.POST /jobs/check-source-updates": { kind: "http", resources: UPDATE_RESOURCES },
  "http.POST /jobs/extract-source-docs": { kind: "http", resources: DOC_RESOURCES },
  "http.POST /jobs/import-scan": { kind: "http", resources: SYNC_RESOURCES },
  "http.POST /jobs/sync": { kind: "http", resources: SYNC_RESOURCES },
  "http.POST /api/v1/jobs/check-source-updates": {
    kind: "http_alias",
    resources: UPDATE_RESOURCES,
  },
  "http.POST /api/v1/jobs/extract-source-docs": { kind: "http_alias", resources: DOC_RESOURCES },
  "http.POST /api/v1/jobs/import-scan": { kind: "http_alias", resources: SYNC_RESOURCES },
  "http.POST /api/v1/jobs/sync": { kind: "http_alias", resources: SYNC_RESOURCES },
  "http.POST /sources": { kind: "http", resources: CREATE_RESOURCES },
  "http.POST /api/v1/sources": { kind: "http_alias", resources: CREATE_RESOURCES },
  "http.PUT /sources/:id/repo-manifest": { kind: "http", resources: MIGRATION_RESOURCES },
  "http.PUT /api/v1/sources/:id/repo-manifest": {
    kind: "http_alias",
    resources: MIGRATION_RESOURCES,
  },
  "http.POST /assistant/actions/apply": { kind: "http", resources: SYNC_RESOURCES },
  "http.POST /api/v1/assistant/actions/apply": { kind: "http_alias", resources: SYNC_RESOURCES },
  "http.POST /sources/:id/upload-zip": { kind: "http", resources: UPLOAD_RESOURCES },
  "http.POST /api/v1/sources/:id/upload-zip": { kind: "http_alias", resources: UPLOAD_RESOURCES },
  "http.POST /sources/:id/upload-files": { kind: "http", resources: UPLOAD_RESOURCES },
  "http.POST /api/v1/sources/:id/upload-files": { kind: "http_alias", resources: UPLOAD_RESOURCES },
  "http.GET /sources/:id/docs": { kind: "http", resources: INDEX_RESOURCES },
  "http.GET /api/v1/sources/:id/docs": { kind: "http_alias", resources: INDEX_RESOURCES },
  "http.GET /sources/:id/docs/*": { kind: "http", resources: DOC_RESOURCES },
  "http.GET /api/v1/sources/:id/docs/*": { kind: "http_alias", resources: DOC_RESOURCES },
  "http.GET /sources/:id/cover": { kind: "http", resources: COVER_RESOURCES },
  "http.GET /api/v1/sources/:id/cover": { kind: "http_alias", resources: COVER_RESOURCES },
  "assistant.propose_import_source_files": { kind: "assistant_action", resources: LOCAL_RESOURCES },
  "mcp.start_sync": { kind: "mcp_action", resources: SYNC_RESOURCES },
  "job.check-source-updates": { kind: "job", resources: UPDATE_RESOURCES },
  "job.extract-source-docs": { kind: "job", resources: DOC_RESOURCES },
  "job.import-scan": { kind: "job", resources: SYNC_RESOURCES },
  "job.sync": { kind: "job", resources: SYNC_RESOURCES },
  "startup.startSourceWatcher": { kind: "startup_watcher", resources: SYNC_RESOURCES },
  "worker.startSourceWatcher": { kind: "worker", resources: WORKER_RESOURCES },
  "migration.migrateLegacySourceManifestOverridesForTenant": {
    kind: "migration",
    resources: MIGRATION_RESOURCES,
  },
} as const;
export const SOURCE_SYNC_PARTICIPANTS: readonly string[] = Object.freeze(
  Object.keys(SOURCE_SYNC_PARTICIPANT_SPECS),
);
const expectedParticipantSpecs = new Map(Object.entries(SOURCE_SYNC_PARTICIPANT_SPECS));

const MANIFEST_FIELDS = new Set(["operation", "owner", "live_format", "participants"]);
const RECEIPT_FIELDS = new Set([
  "operation",
  "owner",
  "worker_pid",
  "admission_count",
  "participant_count",
  "manifest_sha256",
]);
const MAX_PROCESS_ID = 2_147_483_647;
let admittedManifestSha256: string | undefined;
let admittedReceiptPath: string | undefined;
let rustOwnsSourceSync = false;

/** Drops the in-process admission session. Tests use this so a deleted temp directory does not leak into the next case. A live server does not call it. */
export function resetSourceSyncAdmissionSession(): void {
  admittedManifestSha256 = undefined;
  admittedReceiptPath = undefined;
  rustOwnsSourceSync = false;
}

/** Whether this process admitted a cutover record that gives Source Sync to Rust. */
export function sourceSyncOwnedByRust(): boolean {
  return rustOwnsSourceSync;
}
const LIVE_FORMAT_FIELDS = new Set(["id", "rollback"]);
const PARTICIPANT_FIELDS = new Set(["id", "kind", "owner", "resources"]);
const KINDS = new Set([
  "http",
  "http_alias",
  "mcp_action",
  "job",
  "startup_watcher",
  "worker",
  "migration",
  "assistant_action",
]);

export class OwnershipAdmissionError extends Error {
  readonly statusCode = 503;
  readonly code: string;

  constructor(code: string, message: string) {
    super(message);
    this.name = "OwnershipAdmissionError";
    this.code = code;
  }
}

type RuntimeOwner = "node" | "rust";

type ParsedParticipant = {
  id: string;
  kind: string;
  owner: RuntimeOwner;
  resources: string[];
};

function fail(code: string, message: string): never {
  throw new OwnershipAdmissionError(code, message);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function unknownFields(value: Record<string, unknown>, allowed: Set<string>): string | null {
  for (const key of Object.keys(value)) {
    if (!allowed.has(key)) return key;
  }
  return null;
}

function parseOwner(value: unknown): RuntimeOwner {
  if (value === "node" || value === "compat") return "node";
  if (value === "rust") return "rust";
  return fail("unknown_owner", `unknown owner ${String(value)}`);
}

function resourceName(resource: string): string | null {
  if (resource.startsWith("table:")) return resource.slice("table:".length);
  if (resource.startsWith("file:")) return resource.slice("file:".length);
  return null;
}

/** Path shipped beside this module in source and in `dist/current`. */
export function packagedSourceSyncManifestPath(moduleUrl: string = import.meta.url): string {
  return resolve(dirname(fileURLToPath(moduleUrl)), "../data/source-sync-ownership.json");
}

export function defaultSourceSyncManifestPath(): string {
  const override = process.env.PP_SOURCE_SYNC_OWNERSHIP_MANIFEST;
  if (override) return override;
  const packaged = packagedSourceSyncManifestPath();
  if (existsSync(packaged)) return packaged;
  return fail(
    "malformed",
    "malformed ownership manifest: packaged source sync ownership manifest is missing",
  );
}

export function admitSourceSyncManifest(text: string): {
  operation: string;
  owner: "node";
  participantIds: string[];
  manifestSha256: string;
} {
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error);
    return fail("malformed", `malformed ownership manifest: ${detail}`);
  }
  if (!isRecord(parsed)) return fail("malformed", "malformed ownership manifest: expected object");
  const extra = unknownFields(parsed, MANIFEST_FIELDS);
  if (extra !== null)
    return fail("malformed", `malformed ownership manifest: unknown field ${extra}`);
  if (parsed.operation !== SOURCE_SYNC_OPERATION) {
    return fail("unknown_operation", `unknown operation ${String(parsed.operation)}`);
  }
  if (!isRecord(parsed.live_format)) {
    return fail("malformed", "malformed ownership manifest: live_format");
  }
  const liveExtra = unknownFields(parsed.live_format, LIVE_FORMAT_FIELDS);
  if (liveExtra !== null)
    return fail("malformed", `malformed ownership manifest: unknown field ${liveExtra}`);
  const formatId = parsed.live_format.id;
  if (formatId !== "source-revision-workspace-v1") {
    return fail("unknown_live_format", `unknown live format rollback ${String(formatId)}`);
  }
  if (parsed.live_format.rollback === "incompatible") {
    return fail("incompatible_rollback", "incompatible live-format rollback for source.sync");
  }
  if (parsed.live_format.rollback !== "compatible") {
    return fail(
      "unknown_live_format",
      `unknown live format rollback ${String(parsed.live_format.rollback)}`,
    );
  }
  const operationOwner = parseOwner(parsed.owner);
  if (!Array.isArray(parsed.participants)) {
    return fail("malformed", "malformed ownership manifest: participants");
  }
  const seen = new Set<string>();
  const participants: ParsedParticipant[] = [];
  for (const entry of parsed.participants) {
    if (!isRecord(entry)) return fail("malformed", "malformed ownership manifest: participant");
    const participantExtra = unknownFields(entry, PARTICIPANT_FIELDS);
    if (participantExtra !== null) {
      return fail("malformed", `malformed ownership manifest: unknown field ${participantExtra}`);
    }
    if (typeof entry.id !== "string" || entry.id === "") {
      return fail("malformed", "malformed ownership manifest: participant id");
    }
    if (seen.has(entry.id))
      return fail("duplicate_participant", `duplicate participant ${entry.id}`);
    seen.add(entry.id);
    if (typeof entry.kind !== "string" || !KINDS.has(entry.kind)) {
      return fail("unknown_kind", `unknown participant kind ${String(entry.kind)}`);
    }
    const expectedSpec = expectedParticipantSpecs.get(entry.id);
    if (expectedSpec !== undefined && entry.kind !== expectedSpec.kind) {
      return fail(
        "participant_kind_mismatch",
        `participant kind mismatch for ${entry.id}: expected ${expectedSpec.kind}, got ${entry.kind}`,
      );
    }
    if (!Array.isArray(entry.resources) || entry.resources.length === 0) {
      return fail(
        "malformed",
        "malformed ownership manifest: participant resources must name a table or file",
      );
    }
    const resources: string[] = [];
    for (const resource of entry.resources) {
      if (typeof resource !== "string" || !resourceName(resource)) {
        return fail(
          "malformed",
          `malformed ownership manifest: resource ${String(resource)} is not table: or file:`,
        );
      }
      resources.push(resource);
    }
    if (expectedSpec) {
      const actual = new Set(resources);
      if (
        actual.size !== expectedSpec.resources.length ||
        expectedSpec.resources.some((resource) => !actual.has(resource))
      ) {
        return fail("resource_set_mismatch", `participant resource set mismatch for ${entry.id}`);
      }
    }
    participants.push({
      id: entry.id,
      kind: entry.kind,
      owner: parseOwner(entry.owner),
      resources,
    });
  }
  const worker = participants.find((item) => item.kind === "worker");
  const startup = participants.find((item) => item.kind === "startup_watcher");
  if (worker && startup && worker.owner !== startup.owner) {
    return fail(
      "conflicting_participant",
      `conflicting ownership for source.sync: ${worker.id}:${worker.owner} vs ${startup.id}:${startup.owner}`,
    );
  }
  const resources = new Map<string, RuntimeOwner>();
  for (const participant of participants) {
    for (const resource of participant.resources) {
      const existing = resources.get(resource);
      if (existing && existing !== participant.owner) {
        return fail("conflicting_resource", `conflicting resource authority for ${resource}`);
      }
      resources.set(resource, participant.owner);
    }
    if (
      participant.owner !== operationOwner &&
      (participant.owner === "rust" || operationOwner === "rust")
    ) {
      return fail(
        "conflicting_participant",
        `conflicting ownership for source.sync: operation:${operationOwner} vs ${participant.id}:${participant.owner}`,
      );
    }
  }
  if (operationOwner === "rust" || participants.some((item) => item.owner === "rust")) {
    const participant = participants.find((item) => item.owner === "rust");
    return fail(
      "unsupported_rust_owner",
      `unsupported rust owner for ${participant?.id ?? SOURCE_SYNC_OPERATION}; Node remains the Source Sync owner`,
    );
  }
  const expected = new Set<string>(SOURCE_SYNC_PARTICIPANTS);
  const missing = [...expected].filter((id) => !seen.has(id));
  const unexpected = [...seen].filter((id) => !expected.has(id));
  if (missing.length > 0 || unexpected.length > 0) {
    return fail(
      "participant_set_mismatch",
      `participant set mismatch missing=${JSON.stringify(missing)} unexpected=${JSON.stringify(unexpected)}`,
    );
  }
  return {
    operation: SOURCE_SYNC_OPERATION,
    owner: "node",
    participantIds: [...seen],
    manifestSha256: createHash("sha256").update(text).digest("hex"),
  };
}

export function requireSourceSyncParticipant(id: string): undefined {
  if (rustOwnsSourceSync) {
    fail("rust_owner", `Rust owns source.sync; Node does not run ${id}`);
  }
  const admitted = admitSourceSyncManifest(readSourceSyncManifest());
  if (admittedManifestSha256 !== undefined && admitted.manifestSha256 !== admittedManifestSha256) {
    fail("restart_manifest_changed", "runtime manifest does not match the admitted record");
  }
  if (admittedReceiptPath !== undefined) {
    const receipt = readExistingAdmissionReceipt(admittedReceiptPath, admitted);
    if (receipt === null) {
      fail("malformed", "admission receipt is not a regular file or cannot be opened");
    }
  }
  if (!admitted.participantIds.includes(id)) {
    fail(
      "participant_set_mismatch",
      `participant set mismatch missing=${JSON.stringify([id])} unexpected=[]`,
    );
  }
}

function readSourceSyncManifest(): string {
  try {
    return readFileSync(defaultSourceSyncManifestPath(), "utf8");
  } catch (error) {
    if (error instanceof OwnershipAdmissionError) throw error;
    return fail("malformed", "malformed ownership manifest: cannot read Source Sync manifest");
  }
}

export type SourceSyncAdmissionReceipt = {
  operation: "source.sync";
  owner: "node";
  worker_pid: number;
  admission_count: number;
  participant_count: number;
  manifest_sha256: string;
};

export function sourceSyncAdmissionReceiptPath(dataDir: string): string {
  return resolve(dataDir, "source-sync-admission.json");
}

function isProcessId(value: unknown): value is number {
  return (
    typeof value === "number" && Number.isInteger(value) && value > 0 && value <= MAX_PROCESS_ID
  );
}

/** Read a stored receipt. Structural failures are admission errors and do not repair the file. */
export function parseSourceSyncAdmissionReceipt(
  text: string,
  participantCount: number,
): SourceSyncAdmissionReceipt {
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error);
    return fail("malformed", `malformed ownership manifest: admission receipt ${detail}`);
  }
  if (!isRecord(parsed)) {
    return fail(
      "malformed",
      "malformed ownership manifest: admission receipt is not a single Node admission",
    );
  }
  if (unknownFields(parsed, RECEIPT_FIELDS) !== null) {
    return fail(
      "malformed",
      "malformed ownership manifest: admission receipt is not a single Node admission",
    );
  }
  const manifestSha256 = parsed.manifest_sha256;
  if (
    parsed.operation !== SOURCE_SYNC_OPERATION ||
    parsed.owner !== "node" ||
    !isProcessId(parsed.worker_pid) ||
    parsed.admission_count !== 1 ||
    parsed.participant_count !== participantCount ||
    typeof manifestSha256 !== "string" ||
    !/^[0-9a-f]{64}$/.test(manifestSha256)
  ) {
    return fail(
      "malformed",
      "malformed ownership manifest: admission receipt is not a single Node admission",
    );
  }
  return {
    operation: SOURCE_SYNC_OPERATION,
    owner: "node",
    worker_pid: parsed.worker_pid,
    admission_count: 1,
    participant_count: participantCount,
    manifest_sha256: manifestSha256,
  };
}

function readExistingAdmissionReceipt(
  receiptPath: string,
  admitted: ReturnType<typeof admitSourceSyncManifest>,
): SourceSyncAdmissionReceipt | null {
  let fd: number;
  try {
    fd = openSync(receiptPath, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
  } catch (error) {
    if (isRecord(error) && error.code === "ENOENT") return null;
    return fail("malformed", "admission receipt is not a regular file or cannot be opened");
  }
  try {
    if (!fstatSync(fd).isFile()) {
      return fail("malformed", "admission receipt is not a regular file");
    }
    const existing = parseSourceSyncAdmissionReceipt(
      readFileSync(fd, "utf8"),
      admitted.participantIds.length,
    );
    if (existing.manifest_sha256 !== admitted.manifestSha256) {
      return fail(
        "restart_manifest_changed",
        "restart manifest does not match the admitted record",
      );
    }
    return existing;
  } finally {
    closeSync(fd);
  }
}

// The gateway commits the record only for the complete participant set, and
// fences the HTTP participants itself. A partial set means the record is not
// one the gateway wrote, so Node refuses to start rather than split the operation.
function cutoverGivesSourceSyncToRust(rustOwned: ReadonlySet<string>): boolean {
  const fenced = SOURCE_SYNC_PARTICIPANTS.filter((id) => !id.startsWith("http."));
  const missing = fenced.filter((id) => !rustOwned.has(id));
  if (missing.length === fenced.length) return false;
  if (missing.length > 0) {
    fail(
      "participant_set_mismatch",
      `cutover gives Rust part of source.sync; missing=${JSON.stringify(missing)}`,
    );
  }
  return true;
}

/**
 * Admit the bundled manifest and persist one receipt. A later boot reuses it.
 * `rustOwned` is `rustOwnedNodeParticipants(dataDir)`; this module stays free
 * of imports so fixtures can load it with type stripping alone. Returns null
 * when the cutover record gives Source Sync to Rust: Node then writes no
 * receipt and refuses every participant.
 */
export function beginSourceSyncAdmission(
  dataDir: string,
  rustOwned: ReadonlySet<string> = new Set(),
): SourceSyncAdmissionReceipt | null {
  const admitted = admitSourceSyncManifest(readSourceSyncManifest());
  if (cutoverGivesSourceSyncToRust(rustOwned)) {
    rustOwnsSourceSync = true;
    return null;
  }
  const receiptPath = sourceSyncAdmissionReceiptPath(dataDir);
  const existing = readExistingAdmissionReceipt(receiptPath, admitted);
  if (existing) {
    admittedManifestSha256 = existing.manifest_sha256;
    admittedReceiptPath = receiptPath;
    return existing;
  }
  let receipt: SourceSyncAdmissionReceipt = {
    operation: SOURCE_SYNC_OPERATION,
    owner: "node",
    worker_pid: process.pid,
    admission_count: 1,
    participant_count: admitted.participantIds.length,
    manifest_sha256: admitted.manifestSha256,
  };
  const temporary = `${receiptPath}.${randomUUID()}.tmp`;
  const payload = Buffer.from(JSON.stringify(receipt, null, 2));
  mkdirSync(dataDir, { recursive: true });
  const fd = openSync(
    temporary,
    constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | constants.O_NOFOLLOW,
    0o600,
  );
  try {
    try {
      writeFileSync(fd, payload);
      fsyncSync(fd);
    } finally {
      closeSync(fd);
    }
    try {
      linkSync(temporary, receiptPath);
    } catch (error) {
      if (!isRecord(error) || error.code !== "EEXIST") throw error;
      receipt =
        readExistingAdmissionReceipt(receiptPath, admitted) ??
        fail("malformed", "concurrent admission receipt disappeared");
    }
  } finally {
    rmSync(temporary, { force: true });
  }
  // Windows flushes only a handle with write access; a read-only sync fails with EPERM.
  // Only Windows opens a directory for writing.
  const directory = openSync(dataDir, process.platform === "win32" ? "r+" : "r");
  try {
    fsyncSync(directory);
  } finally {
    closeSync(directory);
  }
  admittedManifestSha256 = receipt.manifest_sha256;
  admittedReceiptPath = receiptPath;
  return receipt;
}

function runningAsCli(): boolean {
  const entry = process.argv[1];
  if (!entry) return false;
  return fileURLToPath(import.meta.url) === resolve(entry);
}

if (runningAsCli()) {
  const manifestFlag = process.argv.indexOf("--manifest");
  const participantFlag = process.argv.indexOf("--participant");
  try {
    if (manifestFlag >= 0) {
      process.env.PP_SOURCE_SYNC_OWNERSHIP_MANIFEST = process.argv[manifestFlag + 1];
    }
    if (participantFlag >= 0) {
      requireSourceSyncParticipant(process.argv[participantFlag + 1] ?? "");
    } else {
      admitSourceSyncManifest(readFileSync(defaultSourceSyncManifestPath(), "utf8"));
    }
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exit(1);
  }
}
