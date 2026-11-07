// @vitest-environment jsdom

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import type { IntegrationSummary } from "../../api/endpoints/integrations";
import type { PrinterDetailsInput, PrinterMachine } from "../../api/endpoints/printers";
import PrintersSettingsCard from "./PrintersSettingsCard";

const api = vi.hoisted(() => ({
  addPrinter: vi.fn(),
  createIntegration: vi.fn(),
  deleteIntegration: vi.fn(),
  deletePrinter: vi.fn(),
  fetchFilamentCatalog: vi.fn(),
  fetchIntegrationStatus: vi.fn(),
  fetchIntegrations: vi.fn(),
  fetchPrinterPlanBindings: vi.fn(),
  fetchPrinterPresets: vi.fn(),
  fetchPrinterProfileAssignment: vi.fn(),
  fetchPrinters: vi.fn(),
  fetchProfiles: vi.fn(),
  fetchSlicerProfileOptions: vi.fn(),
  fetchTlsPin: vi.fn(),
  resetTlsPin: vi.fn(),
  savePrinterFleet: vi.fn(),
  savePrinterPlanBinding: vi.fn(),
  testIntegration: vi.fn(),
  updateIntegration: vi.fn(),
  updatePrinterDetails: vi.fn(),
  updatePrinterSlicer: vi.fn(),
}));

vi.mock("../../api/endpoints/filaments", () => ({
  fetchFilamentCatalog: api.fetchFilamentCatalog,
}));

vi.mock("../../api/endpoints/integrations", () => ({
  createIntegration: api.createIntegration,
  deleteIntegration: api.deleteIntegration,
  fetchIntegrationStatus: api.fetchIntegrationStatus,
  fetchIntegrations: api.fetchIntegrations,
  fetchTlsPin: api.fetchTlsPin,
  resetTlsPin: api.resetTlsPin,
  testIntegration: api.testIntegration,
  updateIntegration: api.updateIntegration,
}));

vi.mock("../../api/endpoints/plans", () => ({
  fetchProfiles: api.fetchProfiles,
}));

vi.mock("../../api/endpoints/printerSettings", () => ({
  fetchPrinterPlanBindings: api.fetchPrinterPlanBindings,
  fetchPrinterProfileAssignment: api.fetchPrinterProfileAssignment,
  savePrinterPlanBinding: api.savePrinterPlanBinding,
}));

vi.mock("../../api/endpoints/printers", () => ({
  addPrinter: api.addPrinter,
  deletePrinter: api.deletePrinter,
  fetchPrinterPresets: api.fetchPrinterPresets,
  fetchPrinters: api.fetchPrinters,
  savePrinterFleet: api.savePrinterFleet,
  updatePrinterDetails: api.updatePrinterDetails,
  updatePrinterSlicer: api.updatePrinterSlicer,
}));

vi.mock("../../api/endpoints/slicers", () => ({
  fetchSlicerProfileOptions: api.fetchSlicerProfileOptions,
}));

const healthState = vi.hoisted(() => ({
  health: { ok: true, capabilities: [] as string[] },
}));

vi.mock("../../hooks/useEngineHealth", () => ({
  useEngineHealth: () => ({ health: healthState.health, error: null, loading: false }),
}));

function renderWithQueryClient(children: ReactNode) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>,
  );
}

afterEach(cleanup);

beforeEach(() => {
  vi.clearAllMocks();
  healthState.health = { ok: true, capabilities: [] };
  api.fetchPrinterPresets.mockResolvedValue([
    {
      id: "preset-voron-250",
      name: "Voron 2.4 250",
      model_slug: "voron-250",
      bed_width_mm: 250,
      bed_depth_mm: 250,
      bed_height_mm: 250,
      max_filament_slots: 1,
    },
    {
      id: "preset-voron-300",
      name: "Voron 2.4 300",
      model_slug: "voron-300",
      bed_width_mm: 300,
      bed_depth_mm: 300,
      bed_height_mm: 300,
      max_filament_slots: 1,
    },
  ]);
  api.fetchPrinterPlanBindings.mockResolvedValue([]);
  api.fetchPrinterProfileAssignment.mockResolvedValue({
    printer_id: "printer-shop-voron",
    profile_source: "auto_match",
    machine_profile_id: null,
    filament_slots: [],
    last_synced_at: null,
    compatible_processes: [],
  });
  api.fetchProfiles.mockResolvedValue([]);
  api.fetchSlicerProfileOptions.mockResolvedValue({
    printers: [],
    filaments: [],
    processes: [],
  });
  api.fetchFilamentCatalog.mockResolvedValue(null);
  api.fetchIntegrationStatus.mockResolvedValue({ state: "idle" });
});

describe("PrintersSettingsCard", () => {
  it.each([false, true])("creates a manual printer before optionally configuring communication: %s", async (connect) => {
    let fleet: PrinterMachine[] = [];
    let hosts: IntegrationSummary[] = [];
    api.fetchPrinters.mockImplementation(async () => fleet);
    api.fetchIntegrations.mockImplementation(async () => hosts);
    api.addPrinter.mockImplementation(async () => {
      const created: PrinterMachine = {
        id: "printer-shop-voron",
        name: "Shop Voron",
        model: "voron-250",
        bed_width_mm: 250,
        bed_depth_mm: 250,
        bed_height_mm: 250,
        margin_mm: 4,
        max_filament_slots: 1,
        loaded_filaments: [
          { slot: 1, filament_color_id: null, label: "" },
        ],
        preset_id: "preset-voron-250",
      };
      fleet = [created];
      return created;
    });
    api.createIntegration.mockImplementation(async () => {
      const created: IntegrationSummary = {
        id: "host-shop-voron",
        type: "moonraker",
        name: "Shop Voron",
        config: {
          base_url: "http://192.168.1.40:7125",
          enabled: true,
        },
        created_at: "2026-08-22T00:00:00.000Z",
        updated_at: "2026-08-22T00:00:00.000Z",
      };
      hosts = [created];
      return created;
    });
    api.savePrinterFleet.mockImplementation(async (next: PrinterMachine[]) => {
      fleet = next;
      return next;
    });

    renderWithQueryClient(<PrintersSettingsCard engineReady />);

    fireEvent.change(await screen.findByLabelText("Name"), {
      target: { value: "Shop Voron" },
    });
    expect(screen.getByRole("checkbox", { name: "Connect to this printer" }).getAttribute("aria-checked")).toBe("false");
    if (connect) fireEvent.click(screen.getByRole("checkbox", { name: "Connect to this printer" }));
    const add = screen.getByRole<HTMLButtonElement>("button", { name: "Add printer" });
    await waitFor(() => expect(add.disabled).toBe(false));
    fireEvent.click(add);

    await waitFor(() => {
      expect(api.addPrinter).toHaveBeenCalledWith({
        kind: "preset",
        name: "Shop Voron",
        preset_id: "preset-voron-250",
      });
    });
    expect(api.createIntegration).not.toHaveBeenCalled();
    expect(await screen.findByText(/planning only/)).toBeTruthy();

    if (!connect) fireEvent.click(screen.getByRole("button", { name: "Add connection" }));
    fireEvent.click(screen.getByRole("button", { name: "Save connection" }));

    await waitFor(() => {
      expect(api.createIntegration).toHaveBeenCalledWith({
        type: "moonraker",
        name: "Shop Voron",
        config: {
          base_url: "http://192.168.1.40:7125",
          enabled: true,
        },
      });
      expect(api.savePrinterFleet).toHaveBeenCalledWith([
        expect.objectContaining({
          id: "printer-shop-voron",
          integration_id: "host-shop-voron",
          device_id: "default",
        }),
      ]);
    });
  });

  it("applies presets atomically and marks manual geometry as custom", async () => {
    let fleet: PrinterMachine[] = [
      {
        id: "printer-shop-voron",
        name: "Shop Voron",
        model: "voron-250",
        bed_width_mm: 250,
        bed_depth_mm: 250,
        bed_height_mm: 250,
        margin_mm: 4,
        max_filament_slots: 1,
        loaded_filaments: [
          { slot: 1, filament_color_id: "catalog-red", label: "Red PLA" },
        ],
        preset_id: "preset-voron-250",
      },
    ];
    api.fetchPrinters.mockImplementation(async () => fleet);
    api.fetchIntegrations.mockResolvedValue([]);
    api.updatePrinterDetails.mockImplementation(
      async (_printerId: string, details: PrinterDetailsInput) => {
        const updated = {
          ...fleet[0]!,
          ...details,
          loaded_filaments: Array.from(
            { length: details.max_filament_slots },
            (_, index) =>
              fleet[0]!.loaded_filaments[index] ?? {
                slot: index + 1,
                filament_color_id: null,
                label: "",
              },
          ),
        };
        fleet = [updated];
        return updated;
      },
    );

    renderWithQueryClient(<PrintersSettingsCard engineReady />);

    fireEvent.click(await screen.findByRole("button", { name: "Edit printer" }));
    expect(
      screen.getByText("Blank is send-only; Plate planning requires a positive height."),
    ).toBeTruthy();
    fireEvent.change(screen.getByLabelText("Edit bed margin (mm)"), {
      target: { value: "125" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save printer details" }));
    expect(
      await screen.findByText(
        "Bed margin must be less than half of bed width and depth.",
      ),
    ).toBeTruthy();
    expect(api.updatePrinterDetails).not.toHaveBeenCalled();

    const preset = screen.getByLabelText<HTMLSelectElement>("Edit printer preset");
    fireEvent.change(preset, { target: { value: "preset-voron-300" } });
    fireEvent.change(screen.getByLabelText("Edit printer name"), {
      target: { value: "Shop Voron 300" },
    });
    expect(preset.value).toBe("preset-voron-300");
    expect(screen.getByLabelText<HTMLInputElement>("Edit printer model").value).toBe(
      "voron-300",
    );
    expect(screen.getByLabelText<HTMLInputElement>("Edit bed width (mm)").value).toBe(
      "300",
    );
    expect(screen.getByLabelText<HTMLInputElement>("Edit bed depth (mm)").value).toBe(
      "300",
    );
    expect(screen.getByLabelText<HTMLInputElement>("Edit bed height (mm)").value).toBe(
      "300",
    );
    expect(screen.getByLabelText<HTMLInputElement>("Edit bed margin (mm)").value).toBe(
      "4",
    );
    expect(screen.getByLabelText<HTMLInputElement>("Edit filament slots").value).toBe("1");
    fireEvent.click(screen.getByRole("button", { name: "Save printer details" }));

    await waitFor(() => {
      expect(api.updatePrinterDetails).toHaveBeenCalledWith(
        "printer-shop-voron",
        {
          name: "Shop Voron 300",
          model: "voron-300",
          bed_width_mm: 300,
          bed_depth_mm: 300,
          bed_height_mm: 300,
          margin_mm: 4,
          max_filament_slots: 1,
          preset_id: "preset-voron-300",
        },
      );
    });
    expect(await screen.findByText("Preset reference: preset-voron-300")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Edit printer" }));
    const nextPreset = screen.getByLabelText<HTMLSelectElement>("Edit printer preset");
    fireEvent.change(screen.getByLabelText("Edit bed width (mm)"), {
      target: { value: "301" },
    });
    expect(nextPreset.value).toBe("custom");
    fireEvent.click(screen.getByRole("button", { name: "Save printer details" }));

    await waitFor(() => {
      expect(api.updatePrinterDetails).toHaveBeenLastCalledWith(
        "printer-shop-voron",
        {
          name: "Shop Voron 300",
          model: "voron-300",
          bed_width_mm: 301,
          bed_depth_mm: 300,
          bed_height_mm: 300,
          margin_mm: 4,
          max_filament_slots: 1,
          preset_id: null,
        },
      );
    });
    expect(await screen.findByText("Custom geometry")).toBeTruthy();

    api.updatePrinterDetails.mockRejectedValueOnce(
      new Error("Clear loaded filament slot 4 before reducing filament slots"),
    );
    fireEvent.click(screen.getByRole("button", { name: "Edit printer" }));
    fireEvent.click(screen.getByRole("button", { name: "Save printer details" }));
    expect(
      await screen.findByText(
        "Clear loaded filament slot 4 before reducing filament slots",
      ),
    ).toBeTruthy();
  });

  it.each([true, false])("retains printer details and honors communication enabled=%s", async (enabled) => {
    const printer: PrinterMachine = {
      id: "printer-shop-voron",
      name: "Shop Voron",
      model: "voron-250",
      bed_width_mm: 250,
      bed_depth_mm: 250,
      bed_height_mm: 250,
      margin_mm: 4,
      max_filament_slots: 1,
      loaded_filaments: [
        { slot: 1, filament_color_id: null, label: "" },
      ],
      integration_id: "host-shop-voron",
      device_id: "default",
      preset_id: "preset-retired",
    };
    api.fetchPrinters.mockResolvedValue([printer]);
    api.fetchIntegrations.mockResolvedValue([
      {
        id: "host-shop-voron",
        type: "moonraker",
        name: "Shop Voron",
        config: { base_url: "http://192.168.1.40:7125", enabled },
        created_at: "2026-08-22T00:00:00.000Z",
        updated_at: "2026-08-22T00:00:00.000Z",
      },
    ]);
    api.updatePrinterDetails.mockImplementation(
      async (_printerId: string, details: PrinterDetailsInput) => ({
        ...printer,
        ...details,
      }),
    );
    api.updateIntegration.mockResolvedValue(undefined);

    renderWithQueryClient(<PrintersSettingsCard engineReady />);

    fireEvent.click(await screen.findByRole("button", { name: "Edit printer" }));
    expect(screen.getByRole("checkbox", { name: "Communication enabled" }).getAttribute("aria-checked")).toBe(String(enabled));
    if (!enabled) {
      expect(api.fetchIntegrationStatus).not.toHaveBeenCalled();
      expect(screen.getByRole("button", { name: "Test connection" }).hasAttribute("disabled")).toBe(true);
    }
    const preset = screen.getByLabelText<HTMLSelectElement>("Edit printer preset");
    const retiredOption = screen.getByRole<HTMLOptionElement>("option", {
      name: "Retired preset (unavailable): preset-retired",
    });
    expect(preset.value).toBe("preset-retired");
    expect(retiredOption.selected).toBe(true);
    fireEvent.change(screen.getByLabelText("Edit printer name"), {
      target: { value: "Main Shop Voron" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save printer details" }));

    await waitFor(() => {
      expect(api.updatePrinterDetails).toHaveBeenCalledWith(
        "printer-shop-voron",
        expect.objectContaining({
          name: "Main Shop Voron",
          preset_id: "preset-retired",
        }),
      );
      expect(api.updateIntegration).toHaveBeenCalledWith("host-shop-voron", {
        name: "Main Shop Voron",
      });
    });
    expect(await screen.findByText("Main Shop Voron")).toBeTruthy();
  });

  it.each([false, true])("shows the Bambu certificate pin only in the desktop rebuild: %s", async (rebuild) => {
    if (rebuild) healthState.health = { ok: true, capabilities: ["tenant_feature_toggles"] };
    api.fetchPrinters.mockResolvedValue([
      {
        id: "printer-x1c",
        name: "X1C",
        model: "x1c",
        bed_width_mm: 256,
        bed_depth_mm: 256,
        bed_height_mm: 256,
        margin_mm: 4,
        max_filament_slots: 1,
        loaded_filaments: [{ slot: 1, filament_color_id: null, label: "" }],
        integration_id: "host-x1c",
        device_id: "default",
        preset_id: null,
      } satisfies PrinterMachine,
    ]);
    api.fetchIntegrations.mockResolvedValue([
      {
        id: "host-x1c",
        type: "bambu",
        name: "X1C",
        config: { host: "192.168.1.60", serial: "FIXTURE01" },
        created_at: "2026-11-07T00:00:00.000Z",
        updated_at: "2026-11-07T00:00:00.000Z",
      },
    ]);
    let pinned = true;
    api.fetchTlsPin.mockImplementation(async () =>
      pinned
        ? { pinned: true, sha256_prefix: "ab12cd34ef56", pinned_at: "2026-11-07T12:00:00Z" }
        : { pinned: false },
    );
    api.resetTlsPin.mockImplementation(async () => {
      pinned = false;
    });

    renderWithQueryClient(<PrintersSettingsCard engineReady />);
    await screen.findByRole("button", { name: "Test connection" });

    if (!rebuild) {
      expect(screen.queryByRole("button", { name: "Reset certificate" })).toBeNull();
      expect(api.fetchTlsPin).not.toHaveBeenCalled();
      return;
    }
    expect(await screen.findByText(/^Certificate pinned · ab12cd34ef56 · /)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Reset certificate" }));
    expect(api.resetTlsPin).not.toHaveBeenCalled();
    fireEvent.click(await screen.findByRole("button", { name: "Reset certificate" }));
    await waitFor(() => expect(api.resetTlsPin).toHaveBeenCalledWith("host-x1c"));
    expect(await screen.findByText("Not pinned. Test connection to pin.")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Reset certificate" })).toBeNull();
  });

  it("hides printer connection controls on the hosted planning host", async () => {
    healthState.health = { ok: true, capabilities: ["hosted_planning"] };
    api.fetchPrinters.mockResolvedValue([]);
    api.fetchIntegrations.mockResolvedValue([]);
    renderWithQueryClient(<PrintersSettingsCard engineReady />);
    await screen.findByLabelText("Name");
    expect(screen.queryByRole("checkbox", { name: "Connect to this printer" })).toBeNull();
    expect(screen.queryByText("Printer status refresh")).toBeNull();
    expect(screen.getAllByText(/Live status and send run on a Compose install/).length).toBeGreaterThan(0);
  });
});
