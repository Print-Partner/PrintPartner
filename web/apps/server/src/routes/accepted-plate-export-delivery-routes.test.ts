import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";
import { strFromU8, unzipSync } from "fflate";
import {
  parseAcceptedPlateExportJobResult,
  parseAcceptedPlateWorkspace,
  type JobSnapshot,
} from "@print-partner/contracts";
import { createSelfHostPorts } from "../adapters/self-host/index.js";
import { buildApp } from "../app.js";
import { loadConfig } from "../config.js";
import { acceptedPlanBasis } from "../db/accepted-plan-progress.js";
import { InProcessJobRunner } from "../services/job-runner.js";
import { saveFleet } from "../services/printer-fleet.js";

vi.mock("../services/webhook-store.js", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../services/webhook-store.js")>()),
  dispatchWebhooks: vi.fn(async () => undefined),
}));

import { dispatchWebhooks } from "../services/webhook-store.js";
import { getLogger } from "../services/logger.js";

const cleanups: Array<() => Promise<void>> = [];

afterEach(async () => {
  for (const cleanup of cleanups.splice(0).reverse()) await cleanup();
  vi.restoreAllMocks();
  vi.clearAllMocks();
});

async function waitForJob(
  app: Awaited<ReturnType<typeof buildApp>>,
  jobId: string,
): Promise<JobSnapshot> {
  for (let attempt = 0; attempt < 200; attempt += 1) {
    const response = await app.inject({ method: "GET", url: `/jobs/${jobId}` });
    const snapshot: JobSnapshot = response.json();
    if (snapshot.status === "done" || snapshot.status === "error") return snapshot;
    await new Promise<void>((resolve) => setTimeout(resolve, 5));
  }
  throw new Error("accepted Plate export job did not finish");
}

function modelXml(bytes: Uint8Array): string {
  const model = unzipSync(bytes)["3D/3dmodel.model"];
  if (!model) throw new Error("Direct export is missing its 3MF model");
  return strFromU8(model);
}

async function fixture(
  jobOptions?: ConstructorParameters<typeof InProcessJobRunner>[1],
  options: Readonly<{ publishPlates?: boolean; filename?: string }> = {},
) {
  const root = mkdtempSync(join(tmpdir(), "pp-accepted-export-routes-"));
  const exchangeDir = join(root, "exchange");
  mkdirSync(exchangeDir);
  const ports = createSelfHostPorts(root);
  await ports.db.connect();
  const repo = ports.repository;
  const source = repo.createSource({
    name: "Accepted export source",
    url: "https://example.test/accepted-export-source",
    source_kind: "github",
  });
  const observed = repo.getProjectRow(source.id);
  if (!observed) throw new Error("Source is missing");
  const locator = `${source.id}/revisions/accepted`;
  const snapshotRoot = join(root, "repos", locator);
  mkdirSync(snapshotRoot, { recursive: true });
  writeFileSync(
    join(snapshotRoot, options.filename ?? "part.stl"),
    `solid accepted
facet normal 0 0 1
outer loop
vertex 0 0 0
vertex 30 0 0
vertex 0 20 10
endloop
endfacet
endsolid accepted`,
  );
  const sourceRevision = repo.recordSourceRevision({
    sourceId: source.id,
    upstreamRevisionKey: "accepted",
    manifestDigest: "d".repeat(64),
    snapshotLocator: locator,
    syncedAt: "2026-08-21T12:00:00.000Z",
    completeness: "complete",
  });
  repo.activateSourceRevision({
    sourceId: source.id,
    revisionId: sourceRevision.id,
    observed,
    sourceVersion: sourceRevision.upstream_revision_key,
  });
  const profile = repo.createProfile("Accepted export Build", source.id);
  const draft = repo.recomputePlanDraft({
    profileId: profile.id,
    actor: "test:user",
    idempotencyKey: "accepted-export-draft",
  });
  if (draft.kind !== "created") throw new Error("Plan draft was not created");
  const reconciled = repo.savePlanDraftRequiredUnitReconciliation({
    profileId: profile.id,
    draftId: draft.draft.id,
    expectedSnapshotDigest: draft.draft.snapshotDigest,
    decisions: [],
    actorId: "test:user",
    idempotencyKey: "accepted-export-reconciliation",
  });
  if (reconciled.kind !== "saved") throw new Error("Plan reconciliation failed");
  const applied = repo.applyPlanChanges({
    profileId: profile.id,
    draftId: draft.draft.id,
    expectedSnapshotDigest: reconciled.draft.snapshotDigest,
    expectedLifecycleVersion: 0,
    expectedBase: { kind: "empty", planVersion: 0 },
    actorId: "test:user",
    idempotencyKey: "accepted-export-apply",
  });
  if (applied.kind !== "applied") throw new Error("Plan was not applied");
  const accepted = repo.readAcceptedPlanOperationalSnapshot(profile.id);
  if (accepted.kind !== "ready") throw new Error("Accepted Plan is unavailable");
  const unit = accepted.snapshot.parts
    .filter((part) => part.included)
    .flatMap((part) => part.units)
    .find((candidate) => candidate.required);
  if (!unit) throw new Error("Required unit is missing");
  let plateRevisionId = 0;
  if (options.publishPlates !== false) {
    const published = repo.publishAcceptedPlates({
      profileId: profile.id,
      expected: acceptedPlanBasis(accepted.snapshot),
      expectedPlateRevisionId: null,
      plates: [
        {
          plateId: `plate_${"c".repeat(32)}`,
          printerId: "printer-one",
          printerName: "Printer One",
          printerModel: "Model One",
          bedWidthUm: 250_000,
          bedDepthUm: 210_000,
          bedHeightUm: 200_000,
          marginUm: 4_000,
          units: [
            {
              token: unit.token,
              xUm: 4_000,
              yUm: 4_000,
              widthUm: 30_000,
              depthUm: 20_000,
              heightUm: 10_000,
            },
          ],
        },
      ],
    });
    if (published.kind !== "published") throw new Error("Plate was not published");
    plateRevisionId = published.plateRevisionId;
  }
  const slicer = repo.upsertSlicerInstance({
    name: "Orca",
    kind: "orca",
    dialect: "orca_json",
    guiUrl: "http://127.0.0.1:18888",
    watchPath: "",
    enabled: true,
  });
  const jobs = new InProcessJobRunner(
    {
      getRepo: () => repo,
      reposDir: join(root, "repos"),
      exportsDir: join(root, "exports"),
      dataDir: root,
    },
    jobOptions,
  );
  ports.jobs = jobs;
  const app = await buildApp({ ...loadConfig(), dataDir: root, exchangeDir }, ports);
  cleanups.push(async () => {
    await app.close();
    ports.db.close();
    rmSync(root, { recursive: true, force: true });
  });
  return {
    app,
    root,
    exchangeDir,
    repo,
    jobs,
    profile,
    plateRevisionId,
    slicer,
    token: unit.token,
  };
}

describe("accepted Plate export delivery routes", () => {
  it.each(["completed", "selected"])(
    "prepares and exports only two chosen copies after a %s copy had its own Plate",
    async (priorBatch) => {
      const { app, repo, profile } = await fixture(undefined, {
        publishPlates: false,
        filename: "part_x3.stl",
      });
      saveFleet(repo, [
        {
          id: "printer-one",
          name: "Printer One",
          model: "Model One",
          bed_width_mm: 250,
          bed_depth_mm: 210,
          bed_height_mm: 200,
          margin_mm: 4,
          max_filament_slots: 1,
          loaded_filaments: [],
        },
      ]);
      const read = repo.readAcceptedPlanOperationalSnapshot(profile.id);
      if (read.kind !== "ready") throw new Error("Expected accepted Plan");
      const part = read.snapshot.parts[0];
      if (!part) throw new Error("Expected accepted part");
      const marked = await app.inject({
        method: "PATCH",
        url: `/parts/${part.projectionPartId}/progress`,
        payload: { unit_index: 0, completed: true },
      });
      expect(marked.statusCode).toBe(200);
      const setup = parseAcceptedPlateWorkspace(
        (await app.inject({ method: "GET", url: `/plans/${profile.id}/plates` })).json(),
      );
      if (setup.kind !== "setup") throw new Error("Expected initial Plate setup");
      expect(setup.units).toHaveLength(3);
      const completed = setup.units.filter((unit) => unit.completed);
      const selected = setup.units.filter((unit) => !unit.completed);
      expect(completed).toHaveLength(1);
      expect(selected).toHaveLength(2);
      const previousUnits = priorBatch === "completed" ? completed : selected.slice(0, 1);
      const first = await app.inject({
        method: "POST",
        url: `/plans/${profile.id}/plates/initialize`,
        payload: {
          expected: setup.basis,
          expected_plate_revision_id: null,
          assignments: previousUnits.map((unit) => ({
            token: unit.token,
            printer_id: "printer-one",
          })),
        },
      });
      expect(first.statusCode).toBe(200);
      const previous = parseAcceptedPlateWorkspace(first.json());
      if (previous.kind !== "ready") throw new Error("Expected prior Plate");
      const prepared = await app.inject({
        method: "POST",
        url: `/plans/${profile.id}/plates/initialize`,
        payload: {
          expected: setup.basis,
          expected_plate_revision_id: previous.plate_revision_id,
          assignments: selected.map((unit) => ({ token: unit.token, printer_id: "printer-one" })),
        },
      });
      expect(prepared.statusCode).toBe(200);
      const workspace = parseAcceptedPlateWorkspace(prepared.json());
      if (workspace.kind !== "ready") throw new Error("Expected selected Plate");
      expect(workspace.plate_revision_id).not.toBe(previous.plate_revision_id);
      expect(workspace.plate_revision_number).toBe(previous.plate_revision_number + 1);
      expect(
        workspace.plates.flatMap((plate) => plate.units.map((unit) => unit.token)).sort(),
      ).toEqual(selected.map((unit) => unit.token).sort());
      expect(workspace.unassigned.map((unit) => unit.token)).toEqual(
        completed.map((unit) => unit.token),
      );
      const started = await app.inject({
        method: "POST",
        url: "/jobs/export-accepted-plate-3mf",
        payload: {
          profile_id: profile.id,
          expected_plate_revision_id: workspace.plate_revision_id,
        },
      });
      expect(started.statusCode).toBe(200);
      const job = await waitForJob(app, started.json().job_id);
      expect(job.status).toBe("done");
      const exported = parseAcceptedPlateExportJobResult(job.result);
      const objects: string[] = [];
      for (const plate of exported.plates) {
        const downloaded = await app.inject({ method: "GET", url: plate.download_url });
        expect(downloaded.statusCode).toBe(200);
        const xml = modelXml(downloaded.rawPayload);
        objects.push(
          ...[...xml.matchAll(/<object\b[^>]*partnumber="([^"]+)"/g)].map((match) => match[1]),
        );
      }
      expect(objects.sort()).toEqual(selected.map((unit) => unit.token).sort());
    },
  );

  it("exports one unarranged named-object 3MF without published Plates", async () => {
    const { app, profile, token } = await fixture(undefined, { publishPlates: false });
    const started = await app.inject({
      method: "POST",
      url: "/jobs/export-direct-3mf",
      payload: { profile_id: profile.id, tokens: [token] },
    });
    expect(started.statusCode).toBe(200);
    const snapshot = await waitForJob(app, started.json().job_id);
    expect(snapshot.status).toBe("done");
    expect(snapshot.kind).toBe("export-direct-3mf");
    expect(snapshot.result).toMatchObject({
      format: "direct-export-3mf-job-v1",
      profile_id: profile.id,
      download_url: expect.stringMatching(/^\/exports\//),
    });
    const downloadUrl = (snapshot.result as { download_url: string }).download_url;
    const downloaded = await app.inject({ method: "GET", url: downloadUrl });
    expect(downloaded.statusCode).toBe(200);
    const xml = modelXml(downloaded.rawPayload);
    expect(xml).toContain(`name="part__${token}"`);
    expect(xml).toContain(`partnumber="${token}"`);
    expect(xml).toContain('vertex x="0" y="0" z="0"');
  });

  it("mirrors the published Plate layout in the direct 3MF", async () => {
    const { app, profile, token } = await fixture();
    const started = await app.inject({
      method: "POST",
      url: "/jobs/export-direct-3mf",
      payload: { profile_id: profile.id, tokens: [token] },
    });
    expect(started.statusCode).toBe(200);
    const snapshot = await waitForJob(app, started.json().job_id);
    expect(snapshot.status).toBe("done");
    expect(snapshot.kind).toBe("export-direct-3mf");
    const downloaded = await app.inject({
      method: "GET",
      url: (snapshot.result as { download_url: string }).download_url,
    });
    const xml = modelXml(downloaded.rawPayload);
    expect(xml).toContain(`partnumber="${token}"`);
    // The fixture's Plate places this unit 4mm in from the bed origin.
    expect(xml).toContain('vertex x="4" y="4" z="0"');
    expect(xml).not.toContain('vertex x="0" y="0" z="0"');
  });

  it("returns root and v1 jobs with byte-exact download URLs", async () => {
    const { app, root, profile, plateRevisionId } = await fixture();
    const completedJobIds: string[] = [];
    for (const url of [
      "/jobs/export-accepted-plate-3mf",
      "/api/v1/jobs/export-accepted-plate-3mf",
    ]) {
      const started = await app.inject({
        method: "POST",
        url,
        payload: {
          profile_id: profile.id,
          expected_plate_revision_id: plateRevisionId,
        },
      });
      expect(started.statusCode).toBe(200);
      const snapshot = await waitForJob(app, started.json().job_id);
      expect(snapshot.status).toBe("done");
      completedJobIds.push(snapshot.job_id);
      const statusUrl = url.startsWith("/api/v1")
        ? `/api/v1/jobs/${started.json().job_id}`
        : `/jobs/${started.json().job_id}`;
      expect((await app.inject({ method: "GET", url: statusUrl })).statusCode).toBe(200);
      const result = parseAcceptedPlateExportJobResult(snapshot.result);
      expect(result).toMatchObject({
        format: "accepted-plate-export-job-v1",
        profile_id: profile.id,
        plate_revision_id: plateRevisionId,
        download_url: result.plates[0]?.download_url,
      });
      expect(JSON.stringify(result)).not.toMatch(
        /pp-accepted-export-routes|\/private\/var|\/tmp\//,
      );
      for (const downloadUrl of [
        result.download_url,
        result.manifest_download_url,
        result.bundle_download_url,
        ...result.plates.map((plate) => plate.download_url),
      ]) {
        const downloaded = await app.inject({ method: "GET", url: downloadUrl });
        expect(downloaded.statusCode).toBe(200);
        expect(downloaded.rawPayload).toEqual(
          readFileSync(
            join(
              root,
              "exports",
              "tenant-default",
              ...downloadUrl.replace(/^\/exports\//, "").split("/"),
            ),
          ),
        );
      }
    }

    const artifacts = await app.inject({
      method: "GET",
      url: `/api/v1/plans/${profile.id}/artifacts`,
    });
    expect(artifacts.statusCode).toBe(200);
    expect(artifacts.json().artifacts).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          kind: "export-accepted-plate-3mf",
          download_url: expect.stringMatching(/^\/exports\//),
        }),
      ]),
    );
    expect(dispatchWebhooks).toHaveBeenCalledWith(
      expect.anything(),
      "plan.exported",
      expect.objectContaining({
        kind: "export-accepted-plate-3mf",
        profile_id: profile.id,
        download_url: expect.stringMatching(/^\/exports\//),
      }),
    );
    const exportedCalls = vi
      .mocked(dispatchWebhooks)
      .mock.calls.filter(([, event]) => event === "plan.exported");
    expect(exportedCalls).toHaveLength(completedJobIds.length);
    expect(exportedCalls.map(([, , payload]) => payload.job_id).sort()).toEqual(
      completedJobIds.sort(),
    );
    expect((await app.inject({ method: "GET", url: "/api/v2/jobs/not-a-job" })).statusCode).toBe(
      404,
    );
  });

  it("logs only coarse context for an unexpected accepted Plate job failure", async () => {
    const { app, repo, profile, plateRevisionId } = await fixture();
    const privateValues = [
      "/private/accepted/customer-plan.stl",
      "d".repeat(64),
      "ppu_private_token",
      "private-stack-frame",
    ];
    const failure = new Error(privateValues.slice(0, 3).join(" "));
    failure.stack = privateValues[3];
    repo.readAcceptedPlateExportInput = () => {
      throw failure;
    };
    const log = vi.spyOn(getLogger(), "log").mockImplementation(() => undefined);

    const started = await app.inject({
      method: "POST",
      url: "/jobs/export-accepted-plate-3mf",
      payload: {
        profile_id: profile.id,
        expected_plate_revision_id: plateRevisionId,
      },
    });
    const snapshot = await waitForJob(app, started.json().job_id);
    const statusResponse = await app.inject({ method: "GET", url: `/jobs/${snapshot.job_id}` });

    expect(snapshot).toMatchObject({
      status: "error",
      result: null,
      error: "Accepted Plate export failed.",
    });
    expect(log).toHaveBeenCalledWith("error", "Accepted Plate export failed unexpectedly", {
      operation: "accepted_plate_export",
      failure: "unexpected",
      profileId: profile.id,
      expectedPlateRevisionId: plateRevisionId,
    });
    expect(log).toHaveBeenCalledTimes(1);
    expect(dispatchWebhooks).toHaveBeenCalledWith(
      expect.anything(),
      "job.error",
      expect.objectContaining({
        job_id: snapshot.job_id,
        kind: "export-accepted-plate-3mf",
        error: "Accepted Plate export failed.",
      }),
    );
    const observable = JSON.stringify({
      response: statusResponse.json(),
      snapshot,
      webhooks: vi
        .mocked(dispatchWebhooks)
        .mock.calls.map(([, event, payload]) => ({ event, payload })),
      logs: log.mock.calls,
    });
    for (const value of privateValues) expect(observable).not.toContain(value);
  });

  it("validates identifiers and maps revision and filesystem failures to safe job errors", async () => {
    const { app, root, profile, plateRevisionId } = await fixture();
    for (const payload of [
      { profile_id: 0, expected_plate_revision_id: plateRevisionId },
      { profile_id: profile.id, expected_plate_revision_id: "19" },
    ]) {
      const response = await app.inject({
        method: "POST",
        url: "/jobs/export-accepted-plate-3mf",
        payload,
      });
      expect(response.statusCode).toBe(400);
      expect(response.json()).not.toHaveProperty("job_id");
    }
    const missing = await app.inject({
      method: "POST",
      url: "/jobs/export-accepted-plate-3mf",
      payload: { profile_id: 999_999, expected_plate_revision_id: plateRevisionId },
    });
    expect(missing.statusCode).toBe(404);
    expect(missing.json()).toMatchObject({ code: "profile_not_found" });

    const stale = await app.inject({
      method: "POST",
      url: "/jobs/export-accepted-plate-3mf",
      payload: { profile_id: profile.id, expected_plate_revision_id: plateRevisionId + 1 },
    });
    const staleJob = await waitForJob(app, stale.json().job_id);
    expect(staleJob).toMatchObject({
      status: "error",
      result: null,
      error: "Plate layout changed. Refresh and export again.",
    });

    const tenantExports = join(root, "exports", "tenant-default");
    rmSync(tenantExports, { recursive: true });
    writeFileSync(tenantExports, "blocked");
    const failed = await app.inject({
      method: "POST",
      url: "/jobs/export-accepted-plate-3mf",
      payload: { profile_id: profile.id, expected_plate_revision_id: plateRevisionId },
    });
    const failedJob = await waitForJob(app, failed.json().job_id);
    expect(failedJob).toMatchObject({
      status: "error",
      result: null,
      error: "Accepted Plate export failed.",
    });
    expect(JSON.stringify(failedJob)).not.toContain(root);
  });

  it("keeps the accepted Plate download after pruning the completed job snapshot", async () => {
    const { app, profile, plateRevisionId } = await fixture({
      completedJobMax: 1,
      completedJobRetentionMs: 60_000,
    });
    const payload = {
      profile_id: profile.id,
      expected_plate_revision_id: plateRevisionId,
    };
    const first = await app.inject({
      method: "POST",
      url: "/jobs/export-accepted-plate-3mf",
      payload,
    });
    const firstJob = await waitForJob(app, first.json().job_id);
    const firstResult = parseAcceptedPlateExportJobResult(firstJob.result);
    const second = await app.inject({
      method: "POST",
      url: "/jobs/export-accepted-plate-3mf",
      payload,
    });
    await waitForJob(app, second.json().job_id);

    expect(
      (await app.inject({ method: "GET", url: `/jobs/${first.json().job_id}` })).statusCode,
    ).toBe(404);
    expect((await app.inject({ method: "GET", url: firstResult.download_url })).statusCode).toBe(
      200,
    );
  });

  it("rejects missing or disabled slicers and an unavailable exchange before export", async () => {
    const { app, root, exchangeDir, repo, profile, plateRevisionId, slicer } = await fixture();
    const payload = {
      profile_id: profile.id,
      expected_plate_revision_id: plateRevisionId,
    };
    const finalDirectory = join(
      root,
      "exports",
      "tenant-default",
      "accepted-plates",
      `profile-${profile.id}`,
      `revision-${plateRevisionId}`,
    );
    const missing = await app.inject({
      method: "POST",
      url: "/slicer-instances/missing/open-accepted-plates",
      payload,
    });
    expect(missing.statusCode).toBe(404);
    expect(missing.json()).toMatchObject({ code: "slicer_instance_not_found" });
    expect(existsSync(finalDirectory)).toBe(false);

    repo.upsertSlicerInstance({
      id: slicer.id,
      name: slicer.name,
      kind: slicer.kind,
      dialect: slicer.dialect,
      guiUrl: slicer.guiUrl,
      watchPath: slicer.watchPath,
      enabled: false,
    });
    const disabled = await app.inject({
      method: "POST",
      url: `/slicer-instances/${slicer.id}/open-accepted-plates`,
      payload,
    });
    expect(disabled.statusCode).toBe(400);
    expect(disabled.json()).toMatchObject({ code: "slicer_instance_disabled" });
    expect(existsSync(finalDirectory)).toBe(false);

    repo.upsertSlicerInstance({
      id: slicer.id,
      name: slicer.name,
      kind: slicer.kind,
      dialect: slicer.dialect,
      guiUrl: slicer.guiUrl,
      watchPath: slicer.watchPath,
      enabled: true,
    });
    rmSync(exchangeDir, { recursive: true });
    const unavailable = await app.inject({
      method: "POST",
      url: `/slicer-instances/${slicer.id}/open-accepted-plates`,
      payload,
    });
    expect(unavailable.statusCode).toBe(400);
    expect(unavailable.json()).toMatchObject({ code: "slicer_exchange_unavailable" });
    expect(existsSync(finalDirectory)).toBe(false);
  });

  it("logs only coarse context for an unexpected accepted Plate handoff failure", async () => {
    const { app, repo, profile, plateRevisionId, slicer } = await fixture();
    const privateValues = [
      "/private/slicer/customer-plan.3mf",
      "e".repeat(64),
      "ppu_handoff_private_token",
      "handoff-private-stack",
    ];
    const failure = new Error(privateValues.slice(0, 3).join(" "));
    failure.stack = privateValues[3];
    repo.readAcceptedPlateExportInput = () => {
      throw failure;
    };
    const capturedErrors: unknown[][] = [];
    app.addHook("onRequest", (request, _reply, done) => {
      request.log.error = (...args: unknown[]) => {
        capturedErrors.push(args);
      };
      done();
    });

    const response = await app.inject({
      method: "POST",
      url: `/slicer-instances/${slicer.id}/open-accepted-plates`,
      payload: {
        profile_id: profile.id,
        expected_plate_revision_id: plateRevisionId,
      },
    });

    expect(response.statusCode).toBe(500);
    expect(response.json()).toEqual({
      detail: "Accepted Plate handoff failed.",
      code: "internal_error",
    });
    expect(capturedErrors).toEqual([
      [
        {
          operation: "accepted_plate_slicer_handoff",
          failure: "unexpected",
          profileId: profile.id,
          expectedPlateRevisionId: plateRevisionId,
          slicerInstanceId: slicer.id,
        },
        "Accepted Plate slicer handoff failed unexpectedly",
      ],
    ]);
    const observable = JSON.stringify({ response: response.json(), logs: capturedErrors });
    for (const value of privateValues) expect(observable).not.toContain(value);
  });

  it("stages the downloaded Plate bytes without returning server paths", async () => {
    const { app, exchangeDir, jobs, profile, plateRevisionId, slicer } = await fixture();
    const response = await app.inject({
      method: "POST",
      url: `/slicer-instances/${slicer.id}/open-accepted-plates`,
      payload: {
        profile_id: profile.id,
        expected_plate_revision_id: plateRevisionId,
      },
    });

    expect(response.statusCode).toBe(200);
    const result = response.json();
    expect(result).toMatchObject({
      plate_revision_id: plateRevisionId,
      inbox_relative_path: `pp-inbox/${slicer.id}/profile-${profile.id}/revision-${plateRevisionId}`,
      staged: [{ ordinal: 1, filename: "0001.3mf" }],
      local_app: { scheme_attempt: null },
    });
    expect(result).not.toHaveProperty("inbox_dir");
    expect(JSON.stringify(result)).not.toContain(exchangeDir);
    const downloaded = await app.inject({ method: "GET", url: result.download_url });
    const staged = readFileSync(join(exchangeDir, result.inbox_relative_path, "0001.3mf"));
    expect(downloaded.rawPayload).toEqual(staged);

    const retry = await app.inject({
      method: "POST",
      url: `/api/v1/slicer-instances/${slicer.id}/open-accepted-plates`,
      payload: { profile_id: profile.id, expected_plate_revision_id: plateRevisionId },
    });
    expect(retry.statusCode).toBe(200);
    expect(retry.json()).toEqual(result);

    const history = (
      await app.inject({ method: "GET", url: `/api/v1/jobs?profile_id=${profile.id}` })
    ).json<{ jobs: JobSnapshot[] }>().jobs;
    const exported = history.filter(
      (job) => job.kind === "export-accepted-plate-3mf" && job.status === "done",
    );
    expect(exported).toHaveLength(1);
    const receipt = parseAcceptedPlateExportJobResult(exported[0]!.result);
    expect(receipt).toMatchObject({
      profile_id: profile.id,
      plate_revision_id: plateRevisionId,
      plate_revision_number: result.plate_revision_number,
      layout_digest: result.layout_digest,
      download_url: result.download_url,
      plates: [{ ordinal: 1, filename: "0001.3mf", download_url: result.download_url }],
    });
    for (const url of [receipt.manifest_download_url, receipt.bundle_download_url]) {
      expect((await app.inject({ method: "GET", url })).statusCode).toBe(200);
    }
    const concurrent = await Promise.all(
      Array.from({ length: 3 }, () =>
        app.inject({
          method: "POST",
          url: `/slicer-instances/${slicer.id}/open-accepted-plates`,
          payload: { profile_id: profile.id, expected_plate_revision_id: plateRevisionId },
        }),
      ),
    );
    expect(concurrent.map((response) => response.statusCode)).toEqual([200, 200, 200]);
    expect(
      (await app.inject({ method: "GET", url: `/api/v1/jobs?profile_id=${profile.id}` })).json()
        .jobs,
    ).toEqual(history);
    expect(dispatchWebhooks).not.toHaveBeenCalled();
    expect(jobs.listJobs({ profile_id: profile.id }, "other-tenant")).toEqual([]);
    expect(await jobs.get(exported[0]!.job_id, "other-tenant")).toBeNull();

    const stagedPath = join(exchangeDir, result.inbox_relative_path, "0001.3mf");
    writeFileSync(stagedPath, "tampered");
    const conflict = await app.inject({
      method: "POST",
      url: `/slicer-instances/${slicer.id}/open-accepted-plates`,
      payload: { profile_id: profile.id, expected_plate_revision_id: plateRevisionId },
    });
    expect(conflict.statusCode).toBe(409);
    expect(conflict.json()).toEqual({
      detail: "The slicer inbox for this Plate revision failed integrity verification.",
      code: "output_conflict",
    });
    expect(readFileSync(stagedPath, "utf8")).toBe("tampered");
    expect(
      (await app.inject({ method: "GET", url: `/api/v1/jobs?profile_id=${profile.id}` })).json()
        .jobs,
    ).toEqual(history);
  });

  it("reuses a completed download export when the same accepted Plates are staged", async () => {
    const { app, profile, plateRevisionId, slicer } = await fixture();
    const started = await app.inject({
      method: "POST",
      url: "/jobs/export-accepted-plate-3mf",
      payload: { profile_id: profile.id, expected_plate_revision_id: plateRevisionId },
    });
    const downloaded = await waitForJob(app, started.json().job_id);
    expect(downloaded.status).toBe("done");
    const before = (
      await app.inject({ method: "GET", url: `/api/v1/jobs?profile_id=${profile.id}` })
    ).json().jobs;
    const staged = await app.inject({
      method: "POST",
      url: `/slicer-instances/${slicer.id}/open-accepted-plates`,
      payload: { profile_id: profile.id, expected_plate_revision_id: plateRevisionId },
    });
    expect(staged.statusCode).toBe(200);
    expect(
      (await app.inject({ method: "GET", url: `/api/v1/jobs?profile_id=${profile.id}` })).json()
        .jobs,
    ).toEqual(before);
  });

  it("does not complete an export when managed staging conflicts or the requested Plate revision is stale", async () => {
    const { app, exchangeDir, profile, plateRevisionId, slicer } = await fixture();
    const inbox = join(
      exchangeDir,
      "pp-inbox",
      slicer.id,
      `profile-${profile.id}`,
      `revision-${plateRevisionId}`,
    );
    mkdirSync(inbox, { recursive: true });
    writeFileSync(join(inbox, "0001.3mf"), "conflicting exchange bytes");
    const conflict = await app.inject({
      method: "POST",
      url: `/slicer-instances/${slicer.id}/open-accepted-plates`,
      payload: { profile_id: profile.id, expected_plate_revision_id: plateRevisionId },
    });
    expect(conflict.statusCode).toBe(409);
    expect(conflict.json().code).toBe("output_conflict");
    const stale = await app.inject({
      method: "POST",
      url: `/slicer-instances/${slicer.id}/open-accepted-plates`,
      payload: { profile_id: profile.id, expected_plate_revision_id: plateRevisionId + 1 },
    });
    expect(stale.statusCode).toBe(409);
    expect(stale.json().code).toBe("plate_revision_changed");
    expect(
      (await app.inject({ method: "GET", url: `/api/v1/jobs?profile_id=${profile.id}` })).json()
        .jobs,
    ).toEqual([]);
  });
});
