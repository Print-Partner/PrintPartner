import { acceptPlanForTest, editAcceptedPartsForTest } from "../test/accept-plan.js";
import { describe, expect, it } from "vitest";
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { getDb, SqliteDatabase } from "../db/client.js";
import { AppRepository } from "../db/repository.js";
import {
  createPrinterCheckoffLink,
  getPrinterCheckoffLink,
  listAwaitingVerifyPrinterCheckoffLinks,
} from "./printer-checkoff-store.js";
import { reconcilePrinterCheckoff } from "./printer-checkoff.js";
import { verifyPrinterCheckoff } from "./printer-checkoff-verify.js";
import { summarizePrintOutcomes } from "./printer-outcomes-store.js";
import type Database from "better-sqlite3";
import { drizzle } from "drizzle-orm/node-postgres";
import type { Pool } from "pg";
import * as pgSchema from "../db/schema-pg.js";
import type { SchemaTables } from "../db/repository.js";
import { registerPostgresSyncQuery, unregisterPostgresSyncQuery } from "../db/sync-db-bridge.js";

function setupPlan() {
  const dir = mkdtempSync(join(tmpdir(), "pp-verify-"));
  const sqlite = new SqliteDatabase(dir);
  sqlite.connect();
  const repo = new AppRepository(getDb(sqlite), undefined, sqlite.reposDir);

  const source = repo.createSource({ name: "Repo", url: "https://github.com/a/b" });
  const repoPath = join(dir, "repos", String(source.id));
  mkdirSync(join(repoPath, "parts"), { recursive: true });
  writeFileSync(join(repoPath, "parts", "bracket.stl"), "solid");
  writeFileSync(join(repoPath, "parts", "frame.stl"), "solid");
  repo.updateSource(source.id, { local_path: repoPath });
  repo.updateImportRules(source.id, ["parts/"]);

  const plan = repo.createProfile("VerifyPlan", source.id);
  acceptPlanForTest(repo, plan.id);
  const parts = repo.listParts(plan.id).parts;
  const priorBracket = parts.find((p) => p.filename === "bracket.stl")!;
  const priorFrame = parts.find((p) => p.filename === "frame.stl")!;
  expect(priorBracket).toBeTruthy();
  expect(priorFrame).toBeTruthy();
  const remapped = editAcceptedPartsForTest(repo, plan.id, [
    { projectionPartId: priorBracket.id, quantityOverride: 2 },
    { projectionPartId: priorFrame.id, quantityOverride: 1 },
  ]);
  const bracket = repo.getPartRow(remapped.get(priorBracket.id)!)!;
  const frame = repo.getPartRow(remapped.get(priorFrame.id)!)!;

  return { dir, sqlite, repo, plan, bracket, frame };
}

function acceptedPrintUnits(repo: AppRepository, profileId: number, partId: number): boolean[] {
  const accepted = repo.readAcceptedPlanOperationalSnapshot(profileId);
  if (accepted.kind !== "ready") throw new Error("accepted fixture is unavailable");
  return (
    accepted.snapshot.parts
      .find((part) => part.projectionPartId === partId)
      ?.units.map((unit) => unit.completed) ?? []
  );
}

describe("printer checkoff verify-first flow", () => {
  it("returns unavailable before any PostgreSQL link or accepted-state query", () => {
    const postgres = drizzle({} as Pool, { schema: pgSchema });
    const statements: string[] = [];
    registerPostgresSyncQuery(postgres, ({ sql: query }) => {
      statements.push(query);
      return { rows: [], rowCount: 0 };
    });
    const repo = new AppRepository(
      postgres,
      "default",
      "/tmp/unused-printer-verify",
      pgSchema as unknown as SchemaTables,
    );
    try {
      expect(
        verifyPrinterCheckoff(repo, "link-1", [{ part_id: 1, unit_index: 0, result: "confirmed" }]),
      ).toEqual({ error: "Accepted Plan update is unavailable", status: 503 });
      expect(statements).toEqual([]);
    } finally {
      unregisterPostgresSyncQuery(postgres);
    }
  });

  it("complete queues awaiting_verify without ticking Progress", () => {
    const { dir, sqlite, repo, plan, bracket, frame } = setupPlan();
    const beforeBracket = acceptedPrintUnits(repo, plan.id, bracket.id);
    const beforeFrame = acceptedPrintUnits(repo, plan.id, frame.id);
    expect(beforeBracket.every((unit) => !unit)).toBe(true);

    const link = createPrinterCheckoffLink(repo, {
      profile_id: plan.id,
      integration_id: "int-trident",
      printer_id: "printer-1",
      host_name: "Trident",
      filename: "plate.gcode",
      units: [
        { part_id: bracket.id, unit_index: 0 },
        { part_id: frame.id, unit_index: 0 },
      ],
    });
    expect(link?.state).toBe("watching");

    const updates = reconcilePrinterCheckoff(repo, "int-trident", {
      state: "complete",
      filename: "plate.gcode",
    });
    expect(updates).toEqual([
      expect.objectContaining({
        link_id: link!.id,
        event: "awaiting_verify",
        host_outcome: "success",
        units_pending: 2,
      }),
    ]);

    const queued = getPrinterCheckoffLink(repo, link!.id);
    expect(queued?.state).toBe("awaiting_verify");
    expect(queued?.host_outcome).toBe("success");
    expect(listAwaitingVerifyPrinterCheckoffLinks(repo, plan.id)).toHaveLength(1);

    expect(acceptedPrintUnits(repo, plan.id, bracket.id)).toEqual(beforeBracket);
    expect(acceptedPrintUnits(repo, plan.id, frame.id)).toEqual(beforeFrame);

    sqlite.close();
    rmSync(dir, { recursive: true, force: true });
  });

  it("confirm patches units; reject leaves unprinted and logs reason", () => {
    const { dir, sqlite, repo, plan, bracket, frame } = setupPlan();
    const link = createPrinterCheckoffLink(repo, {
      profile_id: plan.id,
      integration_id: "int-trident",
      printer_id: "printer-1",
      host_name: "Trident",
      filename: "plate.gcode",
      units: [
        { part_id: bracket.id, unit_index: 0 },
        { part_id: frame.id, unit_index: 0 },
      ],
    });
    reconcilePrinterCheckoff(repo, "int-trident", {
      state: "complete",
      filename: "plate.gcode",
    });

    const confirmed = verifyPrinterCheckoff(repo, link!.id, [
      { part_id: bracket.id, unit_index: 0, result: "confirmed" },
    ]);
    expect("error" in confirmed).toBe(false);
    if ("error" in confirmed) return;
    expect(confirmed.units_confirmed).toBeGreaterThanOrEqual(1);
    expect(confirmed.link.state).toBe("awaiting_verify");

    expect(acceptedPrintUnits(repo, plan.id, bracket.id)[0]).toBe(true);
    expect(acceptedPrintUnits(repo, plan.id, frame.id)[0]).toBe(false);

    const rejected = verifyPrinterCheckoff(repo, link!.id, [
      {
        part_id: frame.id,
        unit_index: 0,
        result: "rejected",
        reason: "bed_adhesion",
        note: "peeled corner",
      },
    ]);
    expect("error" in rejected).toBe(false);
    if ("error" in rejected) return;
    expect(rejected.units_rejected).toBe(1);
    expect(rejected.link.state).toBe("verified");

    expect(acceptedPrintUnits(repo, plan.id, frame.id)[0]).toBe(false);

    const summary = summarizePrintOutcomes(repo, plan.id);
    expect(summary.total_confirmed).toBe(1);
    expect(summary.total_rejected).toBe(1);
    expect(summary.by_reason.bed_adhesion).toBe(1);

    sqlite.close();
    rmSync(dir, { recursive: true, force: true });
  });

  it("verifies a current unit while preserving a legal surplus Progress row", () => {
    const { dir, sqlite, repo, plan, bracket } = setupPlan();
    const raw = (sqlite as unknown as { sqlite: Database.Database }).sqlite;
    raw
      .prepare(
        `INSERT INTO print_progress (tenant_id, part_id, unit_index, completed, assembled)
       VALUES ('default', ?, 7, 1, 1)`,
      )
      .run(bracket.id);
    const surplus = () =>
      raw
        .prepare(
          `SELECT tenant_id, part_id, unit_index, completed, assembled
             FROM print_progress WHERE part_id = ? AND unit_index = 7`,
        )
        .get(bracket.id);
    const before = surplus();
    const link = createPrinterCheckoffLink(repo, {
      profile_id: plan.id,
      integration_id: "int-trident",
      printer_id: "printer-1",
      host_name: "Trident",
      filename: "plate.gcode",
      units: [{ part_id: bracket.id, unit_index: 0 }],
    });
    reconcilePrinterCheckoff(repo, "int-trident", {
      state: "complete",
      filename: "plate.gcode",
    });

    expect(
      verifyPrinterCheckoff(repo, link!.id, [
        { part_id: bracket.id, unit_index: 0, result: "confirmed" },
      ]),
    ).toMatchObject({ units_confirmed: 1 });
    expect(surplus()).toEqual(before);
    sqlite.close();
    rmSync(dir, { recursive: true, force: true });
  });

  it("rolls back Progress, link state, and outcomes when outcome insertion fails", () => {
    const { dir, sqlite, repo, plan, bracket } = setupPlan();
    const raw = (sqlite as unknown as { sqlite: Database.Database }).sqlite;
    const link = createPrinterCheckoffLink(repo, {
      profile_id: plan.id,
      integration_id: "int-trident",
      printer_id: "printer-1",
      host_name: "Trident",
      filename: "plate.gcode",
      units: [{ part_id: bracket.id, unit_index: 0 }],
    });
    reconcilePrinterCheckoff(repo, "int-trident", {
      state: "complete",
      filename: "plate.gcode",
    });
    const beforeLink = getPrinterCheckoffLink(repo, link!.id);
    raw.exec(`CREATE TRIGGER fail_print_outcome_insert
      BEFORE INSERT ON print_job_parts
      BEGIN
        SELECT RAISE(ABORT, 'private outcome failure');
      END`);

    expect(() =>
      verifyPrinterCheckoff(repo, link!.id, [
        { part_id: bracket.id, unit_index: 0, result: "confirmed" },
      ]),
    ).toThrowError(/private outcome failure/);
    expect(acceptedPrintUnits(repo, plan.id, bracket.id)).toEqual([false, false]);
    expect(getPrinterCheckoffLink(repo, link!.id)).toEqual(beforeLink);
    expect(summarizePrintOutcomes(repo, plan.id).total_confirmed).toBe(0);
    sqlite.close();
    rmSync(dir, { recursive: true, force: true });
  });

  it("returns the exact stable error for an unmapped legacy coordinate", () => {
    const { dir, sqlite, repo, plan, bracket } = setupPlan();
    const link = createPrinterCheckoffLink(repo, {
      profile_id: plan.id,
      integration_id: "int-trident",
      printer_id: "printer-1",
      host_name: "Trident",
      filename: "plate.gcode",
      units: [{ part_id: bracket.id + 10_000, unit_index: 0 }],
    });
    reconcilePrinterCheckoff(repo, "int-trident", {
      state: "complete",
      filename: "plate.gcode",
    });

    expect(
      verifyPrinterCheckoff(repo, link!.id, [
        { part_id: bracket.id + 10_000, unit_index: 0, result: "confirmed" },
      ]),
    ).toEqual({
      error: "Legacy Checkoff link no longer maps to the accepted Plan",
      status: 409,
    });
    sqlite.close();
    rmSync(dir, { recursive: true, force: true });
  });

  it("reject without reason is rejected by API sanitizer", () => {
    const { dir, sqlite, repo, plan, bracket } = setupPlan();
    const link = createPrinterCheckoffLink(repo, {
      profile_id: plan.id,
      integration_id: "int-trident",
      printer_id: "printer-1",
      host_name: "Trident",
      filename: "plate.gcode",
      units: [{ part_id: bracket.id, unit_index: 0 }],
    });
    reconcilePrinterCheckoff(repo, "int-trident", {
      state: "complete",
      filename: "plate.gcode",
    });

    const bad = verifyPrinterCheckoff(repo, link!.id, [
      { part_id: bracket.id, unit_index: 0, result: "rejected" },
    ]);
    expect(bad).toEqual(
      expect.objectContaining({
        status: 400,
        error: expect.stringMatching(/reason/i),
      }),
    );

    sqlite.close();
    rmSync(dir, { recursive: true, force: true });
  });

  it("rejects a confirmation that would imply an unconfirmed lower accepted unit", () => {
    const { dir, sqlite, repo, plan, bracket } = setupPlan();
    const link = createPrinterCheckoffLink(repo, {
      profile_id: plan.id,
      integration_id: "int-trident",
      printer_id: "printer-1",
      host_name: "Trident",
      filename: "plate.gcode",
      units: [{ part_id: bracket.id, unit_index: 1 }],
    });
    reconcilePrinterCheckoff(repo, "int-trident", {
      state: "complete",
      filename: "plate.gcode",
    });

    expect(
      verifyPrinterCheckoff(repo, link!.id, [
        { part_id: bracket.id, unit_index: 1, result: "confirmed" },
      ]),
    ).toEqual({
      error: "Confirm must include lower incomplete units first (Progress fills from the left)",
      status: 400,
    });
    expect(acceptedPrintUnits(repo, plan.id, bracket.id)).toEqual([false, false]);
    expect(getPrinterCheckoffLink(repo, link!.id)?.state).toBe("awaiting_verify");

    sqlite.close();
    rmSync(dir, { recursive: true, force: true });
  });

  it("host cancel marks host_failed without Progress ticks", () => {
    const { dir, sqlite, repo, plan, bracket } = setupPlan();
    const link = createPrinterCheckoffLink(repo, {
      profile_id: plan.id,
      integration_id: "int-trident",
      printer_id: "printer-1",
      host_name: "Trident",
      filename: "plate.gcode",
      units: [{ part_id: bracket.id, unit_index: 0 }],
      started: true,
    });
    // Observe active print first
    reconcilePrinterCheckoff(repo, "int-trident", {
      state: "printing",
      filename: "plate.gcode",
      progress: 20,
    });
    const updates = reconcilePrinterCheckoff(repo, "int-trident", {
      state: "idle",
    });
    expect(updates[0]?.event).toBe("host_failed");
    expect(getPrinterCheckoffLink(repo, link!.id)?.state).toBe("host_failed");
    expect(acceptedPrintUnits(repo, plan.id, bracket.id)[0]).toBe(false);

    sqlite.close();
    rmSync(dir, { recursive: true, force: true });
  });

  it("unlabeled-only link persists and reloads with empty units", () => {
    const { dir, sqlite, repo, plan } = setupPlan();
    const link = createPrinterCheckoffLink(repo, {
      profile_id: plan.id,
      integration_id: "int-trident",
      printer_id: "printer-1",
      host_name: "Trident",
      filename: "mystery.gcode",
      units: [],
      unlabeled_names: ["Object_A", "Object_B"],
    });
    expect(link).not.toBeNull();
    expect(link!.units).toEqual([]);
    expect(link!.unlabeled_names).toEqual(["Object_A", "Object_B"]);
    expect(link!.state).toBe("watching");

    const loaded = getPrinterCheckoffLink(repo, link!.id);
    expect(loaded?.unlabeled_names).toEqual(["Object_A", "Object_B"]);
    expect(loaded?.units).toEqual([]);

    const planOnly = createPrinterCheckoffLink(repo, {
      profile_id: plan.id,
      integration_id: "int-trident",
      printer_id: "printer-1",
      host_name: "Trident",
      filename: "empty.gcode",
      units: [],
    });
    expect(planOnly).not.toBeNull();
    expect(planOnly!.units).toEqual([]);
    expect(planOnly!.unlabeled_names).toBeUndefined();
    expect(planOnly!.profile_id).toBe(plan.id);
    expect(getPrinterCheckoffLink(repo, planOnly!.id)?.profile_id).toBe(plan.id);

    sqlite.close();
    rmSync(dir, { recursive: true, force: true });
  });
});
