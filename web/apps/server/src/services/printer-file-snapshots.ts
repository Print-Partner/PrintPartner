import { randomUUID, createHash } from "node:crypto";
import type { AppRepository } from "../db/repository.js";
import type { PrinterFileIdentity, PrintFileClassification } from "@print-partner/contracts";
import {
  classifyPrintFileBytes,
  MAX_CLASSIFIABLE_BYTES,
} from "../lib/print-file-classification.js";
import { loadFleet } from "./printer-fleet.js";
import { getIntegrationConfig } from "../integrations/store.js";

type Scope = { profileId: number; printerId: string; remotePath: string; filename: string };
type Inspection = {
  outcome: "inspected";
  classification: PrintFileClassification;
  identity: PrinterFileIdentity;
};
type Snapshot = { scope: Scope; source: string; expiresAt: number; inspection: Inspection };
type Store = { active: boolean; ready: Map<string, Snapshot> };
const stores = new WeakMap<AppRepository, Store>();
const TTL = 15 * 60_000;

function storeFor(repo: AppRepository): Store {
  let store = stores.get(repo);
  if (!store) {
    store = { active: false, ready: new Map() };
    stores.set(repo, store);
  }
  for (const [token, snapshot] of store.ready) {
    if (snapshot.expiresAt <= Date.now()) store.ready.delete(token);
  }
  return store;
}

export function printerFileSnapshotSource(repo: AppRepository, printerId: string): string | null {
  const printer = loadFleet(repo).find((entry) => entry.id === printerId);
  const integration = printer?.integration_id
    ? getIntegrationConfig(repo, printer.integration_id)
    : null;
  if (!integration || integration.config.enabled === false) return null;
  return createHash("sha256").update(JSON.stringify(integration)).digest("hex");
}

/** Capture only bytes pulled by the client. A token becomes usable at successful EOF. */
export function capturePrinterFile(
  repo: AppRepository,
  scope: Scope,
  identity: PrinterFileIdentity,
  response: Response,
  source: string | null,
) {
  const store = storeFor(repo);
  if (
    !response.body ||
    !response.ok ||
    !source ||
    source !== printerFileSnapshotSource(repo, scope.printerId) ||
    store.active ||
    store.ready.size >= 128
  ) {
    void response.body?.cancel().catch(() => undefined);
    throw new Error("File inspection unavailable. Finish the current file, then reopen this file.");
  }
  store.active = true;
  const token = randomUUID();
  const reader = response.body.getReader();
  let chunks: Uint8Array[] = [];
  let length = 0;
  let finished = false;
  const release = () => {
    if (finished) return;
    finished = true;
    chunks = [];
    store.active = false;
  };
  const body = new ReadableStream<Uint8Array>({
    async pull(controller) {
      try {
        const next = await reader.read();
        if (finished) return;
        if (!next.done) {
          length += next.value.byteLength;
          if (length > MAX_CLASSIFIABLE_BYTES)
            throw new Error("Printer file exceeds inspection size limit");
          chunks.push(next.value.slice());
          controller.enqueue(next.value);
          return;
        }
        const bytes = new Uint8Array(length);
        let offset = 0;
        for (const chunk of chunks) {
          bytes.set(chunk, offset);
          offset += chunk.byteLength;
        }
        const classified = classifyPrintFileBytes(bytes);
        if (classified.outcome === "rejected")
          throw new Error("Printer file could not be inspected");
        store.ready.set(token, {
          scope,
          source,
          expiresAt: Date.now() + TTL,
          inspection: {
            outcome: "inspected",
            classification: classified.classification,
            identity: { ...identity, size_bytes: classified.size_bytes, sha256: classified.sha256 },
          },
        });
        release();
        reader.releaseLock();
        controller.close();
      } catch (error) {
        if (finished) return;
        release();
        await reader.cancel().catch(() => undefined);
        reader.releaseLock();
        controller.error(error);
      }
    },
    async cancel(reason) {
      release();
      store.ready.delete(token);
      try {
        await reader.cancel(reason);
      } finally {
        reader.releaseLock();
      }
    },
  });
  const headers = new Headers(response.headers);
  // The client must await EOF, not merely Content-Length, before using the token.
  headers.delete("content-length");
  return { token, response: new Response(body, { status: response.status, headers }) };
}

export function readPrinterFileSnapshot(
  repo: AppRepository,
  token: string,
  scope: Scope,
): Inspection | null {
  const snapshot = storeFor(repo).ready.get(token);
  if (!snapshot || snapshot.source !== printerFileSnapshotSource(repo, scope.printerId))
    return null;
  const saved = snapshot.scope;
  return saved.profileId === scope.profileId &&
    saved.printerId === scope.printerId &&
    saved.remotePath === scope.remotePath &&
    saved.filename === scope.filename
    ? snapshot.inspection
    : null;
}

export function consumePrinterFileSnapshot(repo: AppRepository, token: string) {
  storeFor(repo).ready.delete(token);
}
