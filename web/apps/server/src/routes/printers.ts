import type { FastifyInstance, FastifyReply, FastifyRequest } from "fastify";
import { Readable } from "node:stream";
import type { AppRepository } from "../db/repository.js";
import {
  loadFleet,
  loadPrinterPresets,
  newMachineFromPreset,
  parsePrinterMachine,
  saveFleet,
} from "../services/printer-fleet.js";
import {
  currentPresetDetails,
  nullablePositiveMm,
  parsePrinterDetails,
  positiveMm,
  samePresetSnapshot,
  type PrinterDetailsInput,
} from "./printer-route-model.js";
import { buildAssignmentView } from "../services/printer-profile-assignments.js";
import type { ProfileSourceMode } from "../services/printer-profile-assignments.js";
import { getIntegrationAdapter } from "../integrations/registry.js";
import { getIntegrationConfig, type IntegrationAdapter } from "../integrations/store.js";
import { safeStoragePath } from "../integrations/adapters/storage-path.js";
import type {
  IntegrationCapabilities,
  IntegrationConfig,
  PrinterCamera,
} from "@print-partner/contracts";
import { sendProblem } from "../lib/api-error.js";
import { cancelResponseBody } from "../lib/bounded-response.js";
import { getLogger } from "../services/logger.js";
import {
  capturePrinterFile,
  printerFileSnapshotSource,
} from "../services/printer-file-snapshots.js";
import { printerStoredFileIdentity } from "../services/printer-checkoff.js";

type RouteDeps = { repo: AppRepository };

function findFleetPrinter(repo: AppRepository, printerId: string) {
  return loadFleet(repo).find((m) => m.id === printerId) ?? null;
}

type PrinterHostCapability =
  | {
      ok: false;
      error: "printer_not_found" | "host_not_linked" | "host_not_available";
    }
  | {
      ok: true;
      printer: NonNullable<ReturnType<typeof findFleetPrinter>>;
      integration: NonNullable<ReturnType<typeof getIntegrationConfig>>;
      adapter: NonNullable<ReturnType<typeof getIntegrationAdapter>>;
    };

function printerHostCapability(repo: AppRepository, printerId: string): PrinterHostCapability {
  const printer = findFleetPrinter(repo, printerId);
  if (!printer) return { ok: false, error: "printer_not_found" };
  const integrationId = printer.integration_id?.trim();
  if (!integrationId) return { ok: false, error: "host_not_linked" };
  const integration = getIntegrationConfig(repo, integrationId);
  if (!integration || integration.config.enabled === false) {
    return { ok: false, error: "host_not_available" };
  }
  const adapter = getIntegrationAdapter(integration.type);
  if (!adapter) return { ok: false, error: "host_not_available" };
  return { ok: true, printer, integration, adapter };
}

function publicCapabilityError(
  reply: FastifyReply,
  error: "printer_not_found" | "host_not_linked" | "host_not_available",
) {
  if (error === "printer_not_found") {
    return sendProblem(reply, 404, "Not Found", "Printer not found");
  }
  if (error === "host_not_linked") {
    return sendProblem(reply, 409, "Conflict", "Printer is not linked to a host");
  }
  return sendProblem(reply, 503, "Service Unavailable", "Printer host is unavailable");
}

async function sendUpstreamResponse(
  reply: FastifyReply,
  response: Response,
  onStreamError?: (error: Error) => void,
) {
  if (!response.ok || !response.body) {
    await cancelResponseBody(response);
    return sendProblem(
      reply,
      response.status === 404 ? 404 : 502,
      response.status === 404 ? "Not Found" : "Bad Gateway",
      response.status === 404
        ? "Printer resource not found"
        : `Printer host returned HTTP ${response.status}`,
    );
  }
  const contentType = response.headers.get("content-type");
  const contentLength = response.headers.get("content-length");
  if (contentType) reply.header("Content-Type", contentType);
  if (contentLength) reply.header("Content-Length", contentLength);
  reply.header("Cache-Control", "private, no-store");
  const stream = Readable.fromWeb(response.body);
  if (onStreamError) stream.once("error", onStreamError);
  return reply.send(stream);
}

/**
 * Which host surfaces the linked adapter can actually serve. Shared with the
 * integration list so there is one capability matrix, not two that can drift.
 */
type PrinterHostCapabilities = IntegrationCapabilities;

/**
 * Resolve a printer's linked host capability and run `use` against it.
 *
 * Every printer-host route shares this shape: find the linked host, map a
 * resolution failure to a public status, decide what to answer when the adapter
 * cannot serve the capability at all, and translate an upstream throw into 502.
 * The log line carries only ids, because host configs hold credentials.
 *
 * These routes are read-only by design (the research doc's "Treat `Inspect` as
 * read-only"), so nothing here selects, starts, uploads, or deletes.
 */
async function withPrinterCapability<Access, Result = never>(args: {
  repo: AppRepository;
  log: FastifyRequest["log"];
  reply: FastifyReply;
  printerId: string;
  /** Which adapter capability this route needs. */
  select: (adapter: IntegrationAdapter) => Access | undefined;
  /** What to answer when the resolved adapter does not offer that capability. */
  unsupported: () => Result | FastifyReply;
  /** Server log line and public fallback detail for an upstream failure. */
  failure: { log: string; detail: string };
  use: (access: Access, config: IntegrationConfig) => Promise<Result | FastifyReply>;
}): Promise<Result | FastifyReply> {
  const capability = printerHostCapability(args.repo, args.printerId);
  if (!capability.ok) return publicCapabilityError(args.reply, capability.error);
  const access = args.select(capability.adapter);
  if (!access) return args.unsupported();
  try {
    return await args.use(access, capability.integration.config);
  } catch (error) {
    args.log.warn(
      { printerId: args.printerId, integrationId: capability.integration.id },
      args.failure.log,
    );
    return sendProblem(
      args.reply,
      502,
      "Bad Gateway",
      error instanceof Error ? error.message : args.failure.detail,
    );
  }
}

export async function registerPrinterRoutes(app: FastifyInstance, deps: RouteDeps): Promise<void> {
  app.get("/printers", async () => ({ printers: loadFleet(deps.repo) }));

  app.get<{ Params: { id: string } }>(
    "/printers/:id/capabilities",
    { config: { rateLimit: { max: 120, timeWindow: "1 minute" } } },
    async (request, reply) => {
      const capability = printerHostCapability(deps.repo, request.params.id);
      if (!capability.ok) {
        // A printer with no reachable host has no capabilities rather than an
        // error, so the client can ask about every printer unconditionally.
        if (capability.error === "printer_not_found") {
          return publicCapabilityError(reply, capability.error);
        }
        return { files: false, cameras: false, status: false } satisfies PrinterHostCapabilities;
      }
      return {
        files: capability.adapter.files !== undefined,
        cameras: capability.adapter.cameras !== undefined,
        status: capability.adapter.getStatus !== undefined,
      } satisfies PrinterHostCapabilities;
    },
  );

  app.get<{ Params: { id: string }; Querystring: { path?: unknown } }>(
    "/printers/:id/files",
    { config: { rateLimit: { max: 60, timeWindow: "1 minute" } } },
    async (request, reply) => {
      const requested = String(request.query.path ?? "").trim();
      // No path, or an empty one, means the host's storage root.
      const path = requested === "" ? "" : safeStoragePath(requested, { trimTrailing: true });
      if (path === null) {
        return sendProblem(reply, 400, "Bad Request", "path is not a valid printer storage path");
      }
      return withPrinterCapability({
        repo: deps.repo,
        log: request.log,
        reply,
        printerId: request.params.id,
        select: (adapter) => adapter.files,
        unsupported: () =>
          sendProblem(reply, 501, "Not Implemented", "Printer host does not support file browsing"),
        failure: { log: "Printer file browsing failed", detail: "Could not browse printer files" },
        use: (files, config) => files.browse(config, path),
      });
    },
  );

  app.get<{ Params: { id: string }; Querystring: { path?: unknown; profile_id?: unknown } }>(
    "/printers/:id/files/content",
    { config: { rateLimit: { max: 20, timeWindow: "1 minute" } } },
    async (request, reply) => {
      const profileId =
        request.query.profile_id === undefined ? undefined : Number(request.query.profile_id);
      if (
        profileId !== undefined &&
        (!Number.isInteger(profileId) || profileId <= 0 || !deps.repo.getProfileHeader(profileId))
      ) {
        return sendProblem(
          reply,
          400,
          "Bad Request",
          "Choose an existing Build before opening a print file",
        );
      }
      const requested = String(request.query.path ?? "").trim();
      if (!requested) return sendProblem(reply, 400, "Bad Request", "path is required");
      const filePath = safeStoragePath(requested, { trimTrailing: true });
      if (!filePath) {
        return sendProblem(reply, 400, "Bad Request", "path is not a valid printer storage path");
      }
      const separator = filePath.lastIndexOf("/");
      const directory = separator === -1 ? "" : filePath.slice(0, separator);
      return withPrinterCapability({
        repo: deps.repo,
        log: request.log,
        reply,
        printerId: request.params.id,
        select: (adapter) => adapter.files,
        unsupported: () =>
          sendProblem(reply, 501, "Not Implemented", "Printer host does not support file browsing"),
        failure: { log: "Printer file open failed", detail: "Could not open printer file" },
        use: async (files, config) => {
          const startedAt = Date.now();
          const snapshotSource =
            profileId === undefined
              ? null
              : printerFileSnapshotSource(deps.repo, request.params.id);
          // Only open something the host actually listed, so a crafted path
          // cannot reach a file outside the browsable storage tree.
          const listing = await files.browse(config, directory);
          const listed = listing.entries.find(
            (entry) => entry.kind === "file" && entry.path === filePath,
          );
          if (!listed) return sendProblem(reply, 404, "Not Found", "Printer file not found");
          let response = await files.open(config, filePath);
          if (request.raw.aborted || reply.raw.destroyed) {
            await cancelResponseBody(response);
            return reply;
          }
          if (profileId !== undefined && listed.kind === "file" && response.ok && response.body) {
            const captured = capturePrinterFile(
              deps.repo,
              {
                profileId,
                printerId: request.params.id,
                remotePath: filePath,
                filename: listed.name,
              },
              printerStoredFileIdentity(listed),
              response,
              snapshotSource,
            );
            response = captured.response;
            reply.header("X-PrintPartner-Snapshot", captured.token);
            reply.header("Access-Control-Expose-Headers", "X-PrintPartner-Snapshot");
          }
          return sendUpstreamResponse(reply, response, (error) => {
            if (request.raw.aborted || reply.raw.destroyed) return;
            const failure = error.name === "TimeoutError" ? "timeout" : "stream_interrupted";
            getLogger().logWorkflow({
              method: "GET",
              url: "/printers/:id/files/content",
              statusCode: reply.statusCode,
              duration: Date.now() - startedAt,
              severity: "error",
              message: "Printer file download interrupted",
              context: {
                printerId: request.params.id,
                failure,
              },
            });
          });
        },
      });
    },
  );

  app.get<{ Params: { id: string } }>(
    "/printers/:id/cameras",
    { config: { rateLimit: { max: 60, timeWindow: "1 minute" } } },
    async (request, reply) =>
      withPrinterCapability({
        repo: deps.repo,
        log: request.log,
        reply,
        printerId: request.params.id,
        select: (adapter) => adapter.cameras,
        // Camera controls appear only for capabilities the integration can
        // serve, so an adapter without cameras reports none rather than failing.
        unsupported: (): { cameras: PrinterCamera[] } => ({ cameras: [] }),
        failure: {
          log: "Printer camera discovery failed",
          detail: "Could not discover printer cameras",
        },
        use: async (cameras, config) => ({ cameras: await cameras.list(config) }),
      }),
  );

  app.get<{ Params: { id: string }; Querystring: { id?: unknown } }>(
    "/printers/:id/cameras/view",
    { config: { rateLimit: { max: 20, timeWindow: "1 minute" } } },
    async (request, reply) => {
      const cameraId = String(request.query.id ?? "").trim();
      if (!cameraId) return sendProblem(reply, 400, "Bad Request", "id is required");
      return withPrinterCapability({
        repo: deps.repo,
        log: request.log,
        reply,
        printerId: request.params.id,
        select: (adapter) => adapter.cameras,
        unsupported: () => sendProblem(reply, 404, "Not Found", "Printer camera not found"),
        failure: { log: "Printer camera view failed", detail: "Could not open printer camera" },
        use: async (cameras, config) => {
          const discovered = await cameras.list(config);
          if (!discovered.some((camera) => camera.id === cameraId)) {
            return sendProblem(reply, 404, "Not Found", "Printer camera not found");
          }
          return sendUpstreamResponse(reply, await cameras.open(config, cameraId));
        },
      });
    },
  );

  app.get("/slicer-profile-options", async () => {
    const printers = deps.repo.listSlicerPrinterProfiles().map((row) => {
      const full = deps.repo.getSlicerPrinterProfileById(row.id);
      return {
        id: row.id,
        name: row.name,
        last_synced_at: full?.lastSyncedAt ?? null,
      };
    });
    const filaments = deps.repo.listSlicerFilamentProfiles().map((row) => {
      const full = deps.repo.getSlicerFilamentProfileById(row.id);
      return {
        id: row.id,
        name: row.name,
        material_type: row.materialType ?? null,
        last_synced_at: full?.lastSyncedAt ?? null,
      };
    });
    const processes = deps.repo.listSlicerProcessProfilesDetailed().map((row) => ({
      id: row.id,
      name: row.name,
      last_synced_at: null as string | null,
    }));
    return { printers, filaments, processes };
  });

  app.get("/printers/:id/profile-assignment", async (request, reply) => {
    const printerId = (request.params as { id: string }).id;
    const printer = findFleetPrinter(deps.repo, printerId);
    if (!printer) {
      return reply.status(404).send({ detail: "Printer not found" });
    }
    return buildAssignmentView(deps.repo, printerId, printer.max_filament_slots);
  });

  app.put("/printers/:id/profile-assignment", async (request, reply) => {
    const printerId = (request.params as { id: string }).id;
    const printer = findFleetPrinter(deps.repo, printerId);
    if (!printer) {
      return reply.status(404).send({ detail: "Printer not found" });
    }
    const body = request.body as {
      profile_source?: ProfileSourceMode;
      machine_profile_id?: number | null;
      filament_slots?: Array<{ slot_index?: number; filament_profile_id?: number | null }>;
    };
    const profileSource = body.profile_source ?? "auto_match";
    if (profileSource !== "assigned" && profileSource !== "auto_match") {
      return reply.status(400).send({ detail: "Invalid profile_source" });
    }
    const filamentSlots = (body.filament_slots ?? []).map((slot) => ({
      slotIndex: Number(slot.slot_index ?? 0),
      filamentProfileId: slot.filament_profile_id ?? null,
    }));
    try {
      deps.repo.upsertPrinterProfileAssignment({
        printerId,
        machineProfileId: body.machine_profile_id ?? null,
        profileSource,
        filamentSlots,
      });
    } catch (e) {
      return reply.status(400).send({ detail: e instanceof Error ? e.message : String(e) });
    }
    return buildAssignmentView(deps.repo, printerId, printer.max_filament_slots);
  });

  app.put("/printers", async (request, reply) => {
    const body = request.body as { printers?: Array<Record<string, unknown>> };
    const raw = body.printers ?? [];
    try {
      const fleet = raw.map((x) => parsePrinterMachine(x));
      saveFleet(deps.repo, fleet);
      return { printers: loadFleet(deps.repo) };
    } catch (e) {
      return reply.status(400).send({ detail: e instanceof Error ? e.message : String(e) });
    }
  });

  app.post("/printers", async (request, reply) => {
    const body = request.body as {
      name?: string;
      model?: string;
      preset_id?: string;
      bed_width_mm?: number;
      bed_depth_mm?: number;
      bed_height_mm?: number | null;
      max_filament_slots?: number;
    };
    const name = body.name?.trim() || "Printer";
    const presetId = body.preset_id?.trim();
    let machine;
    if (presetId) {
      const preset = loadPrinterPresets().find((row) => row.id === presetId);
      if (!preset) {
        return reply.status(400).send({ detail: "Unknown Printer preset" });
      }
      machine = newMachineFromPreset(preset, name);
    } else {
      if (!body.model?.trim()) {
        return reply.status(400).send({ detail: "model is required" });
      }
      try {
        machine = parsePrinterMachine({
          id: `printer-${crypto.randomUUID().slice(0, 10)}`,
          name,
          model: body.model.trim(),
          bed_width_mm: positiveMm(body.bed_width_mm, 250, "bed_width_mm"),
          bed_depth_mm: positiveMm(body.bed_depth_mm, 210, "bed_depth_mm"),
          bed_height_mm: nullablePositiveMm(body.bed_height_mm, 250, "bed_height_mm"),
          margin_mm: 4,
          max_filament_slots: Math.max(1, Number(body.max_filament_slots ?? 1) || 1),
          loaded_filaments: [{ slot: 1, filament_color_id: null, label: "" }],
          preset_id: null,
        });
      } catch (error) {
        return reply.status(400).send({
          detail: error instanceof Error ? error.message : String(error),
        });
      }
    }
    const fleet = loadFleet(deps.repo);
    fleet.push(machine);
    saveFleet(deps.repo, fleet);
    return machine;
  });

  app.put("/printers/:id/details", async (request, reply) => {
    const id = (request.params as { id: string }).id;
    const fleet = loadFleet(deps.repo);
    const existing = fleet.find((printer) => printer.id === id);
    if (!existing) {
      return reply.status(404).send({ detail: "Printer not found" });
    }
    let details: PrinterDetailsInput;
    try {
      details = parsePrinterDetails(request.body);
    } catch (error) {
      return reply.status(400).send({
        detail: error instanceof Error ? error.message : String(error),
      });
    }
    const existingPresetId = existing.preset_id?.trim() || null;
    const preservesStoredSnapshot =
      details.preset_id !== null &&
      details.preset_id === existingPresetId &&
      samePresetSnapshot(details, existing);
    if (details.preset_id && !preservesStoredSnapshot) {
      const preset = loadPrinterPresets().find((row) => row.id === details.preset_id);
      if (!preset) {
        return reply.status(400).send({ detail: "Unknown Printer preset" });
      }
      const currentDetails = currentPresetDetails(preset, details.name);
      if (details.preset_id === existingPresetId && !samePresetSnapshot(details, currentDetails)) {
        return reply.status(400).send({
          detail: "Preset details must match the stored snapshot or current preset",
        });
      }
      details = currentDetails;
    }
    if (details.margin_mm * 2 >= Math.min(details.bed_width_mm, details.bed_depth_mm)) {
      return reply.status(400).send({
        detail: "margin_mm must be less than half of bed width and depth",
      });
    }
    const occupiedRemovedSlots = existing.loaded_filaments.filter(
      (slot) =>
        slot.slot > details.max_filament_slots &&
        (Boolean(slot.filament_color_id?.trim()) || Boolean(slot.label.trim())),
    );
    if (occupiedRemovedSlots.length > 0) {
      const slots = occupiedRemovedSlots.map((slot) => slot.slot).join(", ");
      return reply.status(409).send({
        detail: `Clear loaded filament ${
          occupiedRemovedSlots.length === 1 ? "slot" : "slots"
        } ${slots} before reducing filament slots`,
      });
    }
    saveFleet(
      deps.repo,
      fleet.map((printer) => (printer.id === id ? { ...printer, ...details } : printer)),
    );
    const saved = findFleetPrinter(deps.repo, id);
    if (!saved) throw new Error("Saved Printer was not found");
    return saved;
  });

  app.delete("/printers/:id", async (request, reply) => {
    const id = (request.params as { id: string }).id;
    const fleet = loadFleet(deps.repo).filter((m) => m.id !== id);
    if (fleet.length === loadFleet(deps.repo).length) {
      return reply.status(404).send({ detail: "Printer not found" });
    }
    saveFleet(deps.repo, fleet);
    return reply.status(204).send();
  });

  const VALID_SLICER_OVERRIDES = new Set(["orca", "prusa", "bambu"]);

  app.put("/printers/:id", async (request, reply) => {
    const id = (request.params as { id: string }).id;
    const body = request.body as { preferred_slicer?: string | null };
    const fleet = loadFleet(deps.repo);
    const idx = fleet.findIndex((m) => m.id === id);
    if (idx === -1) {
      return reply.status(404).send({ detail: "Printer not found" });
    }
    if (
      body.preferred_slicer !== undefined &&
      body.preferred_slicer !== null &&
      !VALID_SLICER_OVERRIDES.has(body.preferred_slicer)
    ) {
      return reply.status(400).send({
        detail: `preferred_slicer must be one of orca, prusa, bambu, or null (got ${body.preferred_slicer})`,
      });
    }
    if (body.preferred_slicer !== undefined) {
      const value =
        body.preferred_slicer === null
          ? null
          : (body.preferred_slicer as "orca" | "prusa" | "bambu");
      fleet[idx] = { ...fleet[idx], preferred_slicer: value };
    }
    saveFleet(deps.repo, fleet);
    return loadFleet(deps.repo)[idx];
  });

  app.get("/printer-presets", async () => ({ presets: loadPrinterPresets() }));
}
