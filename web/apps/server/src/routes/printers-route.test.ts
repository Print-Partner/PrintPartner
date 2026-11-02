import { afterEach, describe, expect, it } from "vitest";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { buildApp } from "../app.js";
import { loadConfig } from "../config.js";
import { createSelfHostPorts } from "../adapters/self-host/index.js";

/**
 * Route-level checks for PUT /printers/:id — the per-printer slicer override
 * endpoint that Settings' printer cards call.
 */

let cleanup: Array<() => Promise<void>> = [];

afterEach(async () => {
  for (const fn of cleanup) await fn();
  cleanup = [];
});

async function makeApp() {
  const previousDataDir = process.env.PRINT_PARTNER_DATA_DIR;
  const previousApiKey = process.env.PRINT_PARTNER_API_KEY;
  const dir = mkdtempSync(join(tmpdir(), "pp-printers-route-"));
  process.env.PRINT_PARTNER_DATA_DIR = dir;
  delete process.env.PRINT_PARTNER_API_KEY;
  const config = loadConfig();
  const ports = createSelfHostPorts(dir);
  await ports.db.connect();
  const app = await buildApp(config, ports);
  cleanup.push(async () => {
    try {
      await app.close();
      await ports.db.close();
    } finally {
      rmSync(dir, { recursive: true, force: true });
      if (previousDataDir === undefined) delete process.env.PRINT_PARTNER_DATA_DIR;
      else process.env.PRINT_PARTNER_DATA_DIR = previousDataDir;
      if (previousApiKey === undefined) delete process.env.PRINT_PARTNER_API_KEY;
      else process.env.PRINT_PARTNER_API_KEY = previousApiKey;
    }
  });
  return app;
}

async function addPrinter(app: Awaited<ReturnType<typeof makeApp>>, name: string) {
  const res = await app.inject({
    method: "POST",
    url: "/printers",
    payload: { name, model: name, bed_width_mm: 250, bed_depth_mm: 210 },
  });
  return res.json() as { id: string };
}

async function seedPresetSnapshot(
  app: Awaited<ReturnType<typeof makeApp>>,
  input: {
    name: string;
    presetId: string;
    model: string;
    width: number;
    depth: number;
    height: number;
  },
) {
  const created = await app.inject({
    method: "POST",
    url: "/printers",
    payload: {
      name: input.name,
      model: input.model,
      bed_width_mm: input.width,
      bed_depth_mm: input.depth,
      bed_height_mm: input.height,
    },
  });
  const snapshot = { ...created.json(), preset_id: input.presetId };
  await app.inject({
    method: "PUT",
    url: "/printers",
    payload: { printers: [snapshot] },
  });
  return snapshot as Record<string, unknown> & { id: string };
}

describe("POST /printers", () => {
  it("requires an explicit model for a custom Printer", async () => {
    const app = await makeApp();
    const response = await app.inject({
      method: "POST",
      url: "/printers",
      payload: { name: "Unnamed model", bed_width_mm: 250, bed_depth_mm: 210 },
    });

    expect(response.statusCode).toBe(400);
    expect(response.json()).toEqual({ detail: "model is required" });
  });

  it("creates an unbound planning Printer from a preset and attaches a host later", async () => {
    const app = await makeApp();
    const created = await app.inject({
      method: "POST",
      url: "/printers",
      payload: { name: "Shop Voron", preset_id: "preset-voron-250" },
    });

    expect(created.statusCode).toBe(200);
    expect(created.json()).toMatchObject({
      name: "Shop Voron",
      model: "voron-250",
      bed_width_mm: 250,
      bed_depth_mm: 250,
      bed_height_mm: 250,
      preset_id: "preset-voron-250",
    });
    expect(created.json().integration_id ?? null).toBeNull();

    const host = await app.inject({
      method: "POST",
      url: "/api/v1/integrations",
      payload: {
        type: "moonraker",
        name: "Shop Voron",
        config: { base_url: "http://192.168.1.40:7125", enabled: true },
      },
    });
    expect(host.statusCode).toBe(201);

    const machine: Record<string, unknown> = created.json();
    const integration: { id: string } = host.json();
    const attached = await app.inject({
      method: "PUT",
      url: "/printers",
      payload: {
        printers: [{ ...machine, integration_id: integration.id, device_id: "default" }],
      },
    });

    expect(attached.statusCode).toBe(200);
    expect(attached.json()).toMatchObject({
      printers: [
        {
          id: machine.id,
          integration_id: integration.id,
          device_id: "default",
          preset_id: "preset-voron-250",
        },
      ],
    });
  });

  it("creates an unbound planning Printer from custom dimensions", async () => {
    const app = await makeApp();
    const response = await app.inject({
      method: "POST",
      url: "/printers",
      payload: {
        name: "Wide bed",
        model: "Custom 400",
        bed_width_mm: 400,
        bed_depth_mm: 400,
        bed_height_mm: 450,
      },
    });

    expect(response.statusCode).toBe(200);
    expect(response.json()).toMatchObject({
      name: "Wide bed",
      model: "Custom 400",
      bed_width_mm: 400,
      bed_depth_mm: 400,
      bed_height_mm: 450,
      preset_id: null,
    });
    expect(response.json().integration_id ?? null).toBeNull();
  });

  it.each([
    [{ bed_width_mm: 0 }, "bed_width_mm must be greater than 0"],
    [{ bed_depth_mm: -1 }, "bed_depth_mm must be greater than 0"],
    [{ bed_height_mm: 0 }, "bed_height_mm must be null or greater than 0"],
  ])("rejects invalid custom dimensions %j", async (invalid, detail) => {
    const app = await makeApp();
    const response = await app.inject({
      method: "POST",
      url: "/printers",
      payload: {
        name: "Invalid custom Printer",
        model: "Custom",
        bed_width_mm: 250,
        bed_depth_mm: 210,
        bed_height_mm: 250,
        ...invalid,
      },
    });

    expect(response.statusCode).toBe(400);
    expect(response.json()).toEqual({ detail });
  });

  it("rejects an unknown planning Printer preset", async () => {
    const app = await makeApp();
    const response = await app.inject({
      method: "POST",
      url: "/printers",
      payload: { name: "Ghost", preset_id: "preset-does-not-exist" },
    });

    expect(response.statusCode).toBe(400);
    expect(response.json()).toEqual({ detail: "Unknown Printer preset" });
  });
});

describe("PUT /printers/:id", () => {
  it("sets an explicit preferred_slicer override", async () => {
    const app = await makeApp();
    const printer = await addPrinter(app, "Redoubt");

    const res = await app.inject({
      method: "PUT",
      url: `/printers/${printer.id}`,
      payload: { preferred_slicer: "prusa" },
    });
    expect(res.statusCode).toBe(200);
    expect(res.json()).toMatchObject({ id: printer.id, preferred_slicer: "prusa" });

    const list = await app.inject({ method: "GET", url: "/printers" });
    const fleet = list.json() as {
      printers: Array<{ id: string; preferred_slicer?: string | null }>;
    };
    expect(fleet.printers.find((p) => p.id === printer.id)?.preferred_slicer).toBe("prusa");
  });

  it("clears the override back to Auto with null", async () => {
    const app = await makeApp();
    const printer = await addPrinter(app, "Vertigo");
    await app.inject({
      method: "PUT",
      url: `/printers/${printer.id}`,
      payload: { preferred_slicer: "bambu" },
    });

    const res = await app.inject({
      method: "PUT",
      url: `/printers/${printer.id}`,
      payload: { preferred_slicer: null },
    });
    expect(res.statusCode).toBe(200);
    expect(res.json()).toMatchObject({ id: printer.id, preferred_slicer: null });
  });

  it("rejects an invalid slicer value", async () => {
    const app = await makeApp();
    const printer = await addPrinter(app, "Trident");
    const res = await app.inject({
      method: "PUT",
      url: `/printers/${printer.id}`,
      payload: { preferred_slicer: "nope" },
    });
    expect(res.statusCode).toBe(400);
  });

  it("404s for an unknown printer id", async () => {
    const app = await makeApp();
    const res = await app.inject({
      method: "PUT",
      url: "/printers/does-not-exist",
      payload: { preferred_slicer: "orca" },
    });
    expect(res.statusCode).toBe(404);
  });
});

describe("PUT /printers/:id/details", () => {
  it("updates custom details and preserves runtime state", async () => {
    const app = await makeApp();
    const created = await app.inject({
      method: "POST",
      url: "/printers",
      payload: { name: "Shop Voron", preset_id: "preset-voron-250" },
    });
    const printer: Record<string, unknown> = created.json();
    const loadedFilament = {
      slot: 1,
      filament_color_id: "catalog-red",
      label: "Red PLA",
    };
    await app.inject({
      method: "PUT",
      url: "/printers",
      payload: {
        printers: [
          {
            ...printer,
            integration_id: "host-shop-voron",
            device_id: "default",
            preferred_slicer: "prusa",
            loaded_filaments: [loadedFilament],
          },
        ],
      },
    });

    const response = await app.inject({
      method: "PUT",
      url: `/printers/${printer.id}/details`,
      payload: {
        name: "Shop Voron 300",
        model: "Voron 2.4 300",
        bed_width_mm: 300,
        bed_depth_mm: 305,
        bed_height_mm: 310,
        margin_mm: 6,
        max_filament_slots: 2,
        preset_id: null,
      },
    });

    expect(response.statusCode).toBe(200);
    expect(response.json()).toMatchObject({
      id: printer.id,
      name: "Shop Voron 300",
      model: "Voron 2.4 300",
      bed_width_mm: 300,
      bed_depth_mm: 305,
      bed_height_mm: 310,
      margin_mm: 6,
      max_filament_slots: 2,
      integration_id: "host-shop-voron",
      device_id: "default",
      preferred_slicer: "prusa",
      preset_id: null,
      loaded_filaments: [loadedFilament, { slot: 2, filament_color_id: null, label: "" }],
    });
  });

  it("applies a selected preset atomically", async () => {
    const app = await makeApp();
    const created = await app.inject({
      method: "POST",
      url: "/printers",
      payload: { name: "Shop Printer", preset_id: "preset-bambu-x1" },
    });
    const printer: { id: string } = created.json();
    const response = await app.inject({
      method: "PUT",
      url: `/printers/${printer.id}/details`,
      payload: {
        name: "Shop Voron 300",
        model: "stale-model",
        bed_width_mm: 1,
        bed_depth_mm: 1,
        bed_height_mm: 1,
        margin_mm: 99,
        max_filament_slots: 4,
        preset_id: "preset-voron-300",
      },
    });

    expect(response.statusCode).toBe(200);
    expect(response.json()).toMatchObject({
      id: printer.id,
      name: "Shop Voron 300",
      model: "voron-300",
      bed_width_mm: 300,
      bed_depth_mm: 300,
      bed_height_mm: 300,
      margin_mm: 4,
      max_filament_slots: 1,
      preset_id: "preset-voron-300",
    });
  });

  it("preserves a stored snapshot when its preset definition has changed", async () => {
    const app = await makeApp();
    const snapshot = await seedPresetSnapshot(app, {
      name: "Legacy Voron",
      presetId: "preset-voron-250",
      model: "voron-legacy-240",
      width: 240,
      depth: 240,
      height: 240,
    });
    const response = await app.inject({
      method: "PUT",
      url: `/printers/${snapshot.id}/details`,
      payload: {
        name: "Renamed Legacy Voron",
        model: "voron-legacy-240",
        bed_width_mm: 240,
        bed_depth_mm: 240,
        bed_height_mm: 240,
        margin_mm: 4,
        max_filament_slots: 1,
        preset_id: "preset-voron-250",
      },
    });

    expect(response.statusCode).toBe(200);
    expect(response.json()).toMatchObject({
      id: snapshot.id,
      name: "Renamed Legacy Voron",
      model: "voron-legacy-240",
      bed_width_mm: 240,
      bed_depth_mm: 240,
      bed_height_mm: 240,
      preset_id: "preset-voron-250",
    });
  });

  it("preserves a stored snapshot when its preset no longer exists", async () => {
    const app = await makeApp();
    const snapshot = await seedPresetSnapshot(app, {
      name: "Retired Printer",
      presetId: "preset-retired",
      model: "retired-260",
      width: 260,
      depth: 260,
      height: 280,
    });
    const response = await app.inject({
      method: "PUT",
      url: `/printers/${snapshot.id}/details`,
      payload: {
        name: "Renamed Retired Printer",
        model: "retired-260",
        bed_width_mm: 260,
        bed_depth_mm: 260,
        bed_height_mm: 280,
        margin_mm: 4,
        max_filament_slots: 1,
        preset_id: "preset-retired",
      },
    });

    expect(response.statusCode).toBe(200);
    expect(response.json()).toMatchObject({
      id: snapshot.id,
      name: "Renamed Retired Printer",
      model: "retired-260",
      bed_width_mm: 260,
      bed_depth_mm: 260,
      bed_height_mm: 280,
      preset_id: "preset-retired",
    });
  });

  it("resets a stored snapshot when current preset values are submitted", async () => {
    const app = await makeApp();
    const snapshot = await seedPresetSnapshot(app, {
      name: "Legacy Voron",
      presetId: "preset-voron-250",
      model: "voron-legacy-240",
      width: 240,
      depth: 240,
      height: 240,
    });
    const response = await app.inject({
      method: "PUT",
      url: `/printers/${snapshot.id}/details`,
      payload: {
        name: "Reset Voron",
        model: "voron-250",
        bed_width_mm: 250,
        bed_depth_mm: 250,
        bed_height_mm: 250,
        margin_mm: 4,
        max_filament_slots: 1,
        preset_id: "preset-voron-250",
      },
    });

    expect(response.statusCode).toBe(200);
    expect(response.json()).toMatchObject({
      id: snapshot.id,
      name: "Reset Voron",
      model: "voron-250",
      bed_width_mm: 250,
      bed_depth_mm: 250,
      bed_height_mm: 250,
      preset_id: "preset-voron-250",
    });
  });

  it("rejects a slot shrink that would discard a loaded filament", async () => {
    const app = await makeApp();
    const created = await app.inject({
      method: "POST",
      url: "/printers",
      payload: { name: "Bambu", preset_id: "preset-bambu-x1" },
    });
    const printer: Record<string, unknown> = created.json();
    const loadedFilaments = [
      { slot: 1, filament_color_id: null, label: "" },
      { slot: 2, filament_color_id: null, label: "" },
      { slot: 3, filament_color_id: null, label: "" },
      { slot: 4, filament_color_id: "catalog-red", label: "Red PLA" },
    ];
    await app.inject({
      method: "PUT",
      url: "/printers",
      payload: { printers: [{ ...printer, loaded_filaments: loadedFilaments }] },
    });

    const response = await app.inject({
      method: "PUT",
      url: `/printers/${printer.id}/details`,
      payload: {
        name: "Bambu",
        model: "voron-300",
        bed_width_mm: 300,
        bed_depth_mm: 300,
        bed_height_mm: 300,
        margin_mm: 4,
        max_filament_slots: 1,
        preset_id: "preset-voron-300",
      },
    });

    expect(response.statusCode).toBe(409);
    expect(response.json()).toEqual({
      detail: "Clear loaded filament slot 4 before reducing filament slots",
    });
    const list = await app.inject({ method: "GET", url: "/printers" });
    expect(list.json()).toMatchObject({
      printers: [
        {
          id: printer.id,
          max_filament_slots: 4,
          loaded_filaments: loadedFilaments,
          preset_id: "preset-bambu-x1",
        },
      ],
    });
  });

  it.each([
    [{ name: "" }, "name is required"],
    [{ model: "" }, "model is required"],
    [{ bed_width_mm: 0 }, "bed_width_mm must be greater than 0"],
    [{ bed_depth_mm: -1 }, "bed_depth_mm must be greater than 0"],
    [{ bed_height_mm: 0 }, "bed_height_mm must be null or greater than 0"],
    [{ margin_mm: -1 }, "margin_mm must be 0 or greater"],
    [{ margin_mm: 105 }, "margin_mm must be less than half of bed width and depth"],
    [{ max_filament_slots: 5 }, "max_filament_slots must be an integer from 1 to 4"],
  ])("rejects invalid editable details %j", async (invalid, detail) => {
    const app = await makeApp();
    const printer = await addPrinter(app, "Validation Printer");
    const response = await app.inject({
      method: "PUT",
      url: `/printers/${printer.id}/details`,
      payload: {
        name: "Validation Printer",
        model: "Validation Printer",
        bed_width_mm: 250,
        bed_depth_mm: 210,
        bed_height_mm: 250,
        margin_mm: 4,
        max_filament_slots: 1,
        preset_id: null,
        ...invalid,
      },
    });

    expect(response.statusCode).toBe(400);
    expect(response.json()).toEqual({ detail });
  });

  it("rejects an unknown preset", async () => {
    const app = await makeApp();
    const printer = await addPrinter(app, "Unknown preset");
    const response = await app.inject({
      method: "PUT",
      url: `/printers/${printer.id}/details`,
      payload: {
        name: "Unknown preset",
        model: "Custom",
        bed_width_mm: 250,
        bed_depth_mm: 210,
        bed_height_mm: null,
        margin_mm: 4,
        max_filament_slots: 1,
        preset_id: "preset-does-not-exist",
      },
    });

    expect(response.statusCode).toBe(400);
    expect(response.json()).toEqual({ detail: "Unknown Printer preset" });
  });

  it("returns 404 for an unknown Printer", async () => {
    const app = await makeApp();
    const response = await app.inject({
      method: "PUT",
      url: "/printers/does-not-exist/details",
      payload: {
        name: "Missing",
        model: "Missing",
        bed_width_mm: 250,
        bed_depth_mm: 210,
        bed_height_mm: 250,
        margin_mm: 4,
        max_filament_slots: 1,
        preset_id: null,
      },
    });

    expect(response.statusCode).toBe(404);
    expect(response.json()).toEqual({ detail: "Printer not found" });
  });
});
