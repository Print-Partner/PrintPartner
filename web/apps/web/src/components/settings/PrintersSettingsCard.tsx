import { useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useMemo, useState, type ReactNode } from "react";
import { Printer } from "lucide-react";
import {
  createIntegration,
  deleteIntegration,
  testIntegration,
  updateIntegration,
  type IntegrationSummary,
} from "../../api/endpoints/integrations";
import { HOSTED_PLANNING_COMPOSE_NOTE, isHostedPlanning } from "@print-partner/contracts";
import {
  addPrinter,
  deletePrinter,
  fetchPrinterPresets,
  savePrinterFleet,
  updatePrinterDetails,
  updatePrinterSlicer,
  type PrinterDetailsInput,
  type PrinterMachine,
  type PrinterPreset,
} from "../../api/endpoints/printers";
import {
  fetchPrinterPlanBindings,
  savePrinterPlanBinding,
  type PrinterPlanBinding,
} from "../../api/endpoints/printerSettings";
import { Button } from "../ui/button";
import ConfirmDialog from "../ConfirmDialog";
import { Checkbox } from "../ui/checkbox";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "../ui/card";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "../ui/select";
import BambuCertificatePin from "./BambuCertificatePin";
import PrinterProfileAssignmentSection from "./PrinterProfileAssignmentSection";
import SlotFilamentPicker from "./SlotFilamentPicker";
import PrinterDetailsEditor, {
  CUSTOM_PRESET_ID,
  type PrinterDetailsTextField,
} from "./PrinterDetailsEditor";
import { cn } from "@/lib/utils";
import {
  PRINTER_STATUS_POLL_SECONDS_OPTIONS,
  readPrinterStatusPollSeconds,
  writePrinterStatusPollSeconds,
  type PrinterStatusPollSeconds,
} from "../../lib/persistedPrinterStatusPoll";
import {
  DEFAULT_PRINTER_HOST_URLS,
  PRINTER_HOST_TYPE_LABELS,
  SLICER_OVERRIDE_LABELS,
  SLICER_OVERRIDES,
  isPrinterHostType,
  linkedPrinters,
  orphanPrinters,
  parsePrinterDetailsDraft,
  pickDefaultPresetId,
  printerDetailsDraft,
  printerHostConnectionReady,
  printerSettingsCanAdd,
  statusPillLabel,
  type HostType,
  type PrinterDetailsDraft,
  type SlicerOverride,
} from "../../lib/printerSettingsModel";
import { printerStatusTone } from "../../lib/printerLiveStrip";
import { statusTone } from "../../lib/statusTone";
import { useEngineHealth } from "../../hooks/useEngineHealth";
import { desktopRebuildEnabled } from "../../lib/desktopRebuild";
import { usePrinterStatuses } from "../../queries/printerStatuses";
import {
  invalidatePrinterFleet,
  useIntegrationsQuery,
  usePrintersQuery,
} from "../../queries/printerFleet";
import { queryKeys } from "../../queries/keys";
import { useProfilesQuery } from "../../queries/profiles";
import { useFilamentCatalogQuery } from "../../queries/filaments";

type Props = {
  engineReady: boolean;
};

const DEFAULT_PRESET_ID = "preset-prusa-mk4";
const NO_PRINTERS: PrinterMachine[] = [];

const INPUT_CLASS =
  "rounded-md border border-input bg-background px-2 py-1.5 text-sm w-full";

/**
 * Removing a printer also deletes the host integration it connects through, and
 * a button that only says "Remove" does not tell anyone that. Say it here,
 * where it is the last thing read before the printer goes.
 */
function removePrinterConsequence(printer: PrinterMachine): ReactNode {
  const hasHost = Boolean(printer.integration_id?.trim());
  return (
    <>
      “{printer.name}” is removed from the fleet.
      {hasHost
        ? " Its saved connection to the printer host is deleted with it, so you would have to set that connection up again."
        : ""}
    </>
  );
}

export default function PrintersSettingsCard({ engineReady }: Props) {
  const { health } = useEngineHealth();
  const hostedPlanning = isHostedPlanning(health);
  const desktopRebuild = desktopRebuildEnabled(health?.capabilities);
  const queryClient = useQueryClient();
  const printersQuery = usePrintersQuery(engineReady);
  const integrationsQuery = useIntegrationsQuery(engineReady);
  const printers = printersQuery.data ?? NO_PRINTERS;
  const hosts = useMemo(
    () => (integrationsQuery.data ?? []).filter((i) => isPrinterHostType(i.type)),
    [integrationsQuery.data],
  );
  const setPrinters = (next: PrinterMachine[] | ((prev: PrinterMachine[]) => PrinterMachine[])) => {
    queryClient.setQueryData<PrinterMachine[]>(queryKeys.printers, (prev) =>
      typeof next === "function" ? next(prev ?? []) : next,
    );
  };
  const setIntegrations = (update: (prev: IntegrationSummary[]) => IntegrationSummary[]) => {
    queryClient.setQueryData<IntegrationSummary[]>(queryKeys.integrations, (prev) =>
      update(prev ?? []),
    );
  };
  const [presets, setPresets] = useState<PrinterPreset[]>([]);
  const [actionError, setLoadError] = useState<string | null>(null);
  const fleetError = printersQuery.error ?? integrationsQuery.error;
  const loadError = actionError ??
    (fleetError ? fleetError instanceof Error ? fleetError.message : String(fleetError) : null);
  const [message, setMessage] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [testingId, setTestingId] = useState<string | null>(null);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [detailsDraft, setDetailsDraft] = useState<PrinterDetailsDraft>({
    name: "",
    model: "",
    bedWidth: "",
    bedDepth: "",
    bedHeight: "",
    margin: "",
    maxFilamentSlots: "",
    presetId: null,
  });
  const [planBindings, setPlanBindings] = useState<PrinterPlanBinding[]>([]);
  const profiles = useProfilesQuery(engineReady).data ?? [];
  const catalog = useFilamentCatalogQuery({ enabled: engineReady }).data ?? null;

  const [hostType, setHostType] = useState<HostType>("moonraker");
  const [newName, setNewName] = useState("");
  const [connectNewPrinter, setConnectNewPrinter] = useState(false);
  const [newUrl, setNewUrl] = useState(DEFAULT_PRINTER_HOST_URLS.moonraker);
  const [apiKey, setApiKey] = useState("");
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [bambuHost, setBambuHost] = useState("192.168.1.60");
  const [accessCode, setAccessCode] = useState("");
  const [serial, setSerial] = useState("");
  const [presetId, setPresetId] = useState("");
  const [customWidth, setCustomWidth] = useState("250");
  const [customDepth, setCustomDepth] = useState("250");
  const [customHeight, setCustomHeight] = useState("250");
  const [connectingId, setConnectingId] = useState<string | null>(null);
  const [pollSeconds, setPollSeconds] = useState<PrinterStatusPollSeconds>(() =>
    readPrinterStatusPollSeconds(),
  );

  const hostsById = useMemo(() => new Map(hosts.map((h) => [h.id, h])), [hosts]);
  const statusIntegrationIds = useMemo(
    () => printers.flatMap((printer) => {
      const integrationId = printer.integration_id?.trim();
      const host = integrationId ? hostsById.get(integrationId) : undefined;
      return printer.enabled !== false && integrationId && host && host.config.enabled !== false
        ? [integrationId]
        : [];
    }),
    [printers, hostsById],
  );
  const { statusByIntegration, refresh: refreshStatus } = usePrinterStatuses(
    hostedPlanning ? [] : statusIntegrationIds,
    engineReady && !hostedPlanning,
  );

  const loadSettings = useCallback(async () => {
    if (!engineReady) return;
    setLoadError(null);
    try {
      const [presetRows, bindings] = await Promise.all([
        fetchPrinterPresets(),
        fetchPrinterPlanBindings(),
      ]);
      setPresets(presetRows);
      setPresetId((prev) => prev || pickDefaultPresetId(presetRows, DEFAULT_PRESET_ID));
      setPlanBindings(bindings);
    } catch (e) {
      setLoadError(e instanceof Error ? e.message : String(e));
    }
  }, [engineReady]);

  useEffect(() => {
    void loadSettings();
  }, [loadSettings]);

  const refresh = async () => {
    await Promise.all([
      invalidatePrinterFleet(queryClient),
      fetchPrinterPlanBindings().then(setPlanBindings, (e: unknown) => {
        setLoadError(e instanceof Error ? e.message : String(e));
      }),
    ]);
  };

  // URL defaults follow host type only.
  useEffect(() => {
    if (hostType === "moonraker") {
      setNewUrl((prev) =>
        prev === DEFAULT_PRINTER_HOST_URLS.prusalink || prev.includes("prusa")
          ? DEFAULT_PRINTER_HOST_URLS.moonraker
          : prev,
      );
    } else if (hostType === "prusalink") {
      setNewUrl((prev) =>
        prev === DEFAULT_PRINTER_HOST_URLS.moonraker || prev.includes(":7125")
          ? DEFAULT_PRINTER_HOST_URLS.prusalink
          : prev,
      );
    }
  }, [hostType]);

  useEffect(() => {
    if (!presets.length) return;
    setPresetId((prev) => prev || pickDefaultPresetId(presets, DEFAULT_PRESET_ID));
  }, [presets]);

  const createHost = async (
    name: string,
  ): Promise<{ created: IntegrationSummary; deviceId: string }> => {
    if (hostType === "bambu") {
      const host = bambuHost.trim();
      const trimmedAccessCode = accessCode.trim();
      const deviceSerial = serial.trim();
      if (!host || !trimmedAccessCode || !deviceSerial) {
        throw new Error("Enter printer IP, LAN access code, and serial.");
      }
      const created = await createIntegration({
        type: "bambu",
        name,
        config: {
          host,
          access_code: trimmedAccessCode,
          serial: deviceSerial,
          enabled: true,
        },
      });
      setAccessCode("");
      setSerial("");
      return { created, deviceId: deviceSerial };
    }

    const baseUrl = newUrl.trim();
    if (!baseUrl) {
      throw new Error("Enter the printer base URL.");
    }
    const config: Record<string, unknown> = { base_url: baseUrl, enabled: true };
    if (hostType === "moonraker") {
      if (apiKey.trim()) config.api_key = apiKey.trim();
    } else {
      config.username = username.trim();
      if (password.trim()) config.password = password.trim();
    }
    const created = await createIntegration({ type: hostType, name, config });
    setApiKey("");
    setUsername("");
    setPassword("");
    return { created, deviceId: "default" };
  };

  const onAddPrinter = async () => {
    const name = newName.trim();
    if (!name) return;
    const isCustom = presetId === CUSTOM_PRESET_ID;
    if (!isCustom && !presets.some((p) => p.id === presetId)) {
      setLoadError("Choose a bed size preset.");
      return;
    }
    const width = Number(customWidth);
    const depth = Number(customDepth);
    const height = Number(customHeight);
    if (isCustom && (!(width > 0) || !(depth > 0) || !(height > 0))) {
      setLoadError("Enter custom bed width, depth, and height.");
      return;
    }
    setBusy(true);
    setMessage(null);
    setLoadError(null);
    try {
      const created = await addPrinter(
        isCustom
          ? {
              kind: "custom",
              name,
              model: name,
              bed_width_mm: width,
              bed_depth_mm: depth,
              bed_height_mm: height,
            }
          : { kind: "preset", name, preset_id: presetId },
      );
      setPrinters((prev) => [...prev, created]);
      setNewName("");
      setMessage(
        connectNewPrinter
          ? `${name} added. Configure and save its connection below to enable communication.`
          : `${name} added for manual use. No printer connection is enabled.`,
      );
      if (connectNewPrinter) setConnectingId(created.id);
      setConnectNewPrinter(false);
      await refresh();
    } catch (e) {
      setLoadError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const onAttachConnection = async (printer: PrinterMachine) => {
    setBusy(true);
    setMessage(null);
    setLoadError(null);
    try {
      const { created, deviceId } = await createHost(printer.name);
      const next = printers.map((candidate) =>
        candidate.id === printer.id
          ? { ...candidate, integration_id: created.id, device_id: deviceId }
          : candidate,
      );
      const saved = await savePrinterFleet(next);
      setPrinters(saved);
      setIntegrations((prev) => [...prev, created]);
      setConnectingId(null);
      setMessage(
        hostType === "bambu"
          ? `Connection added to ${printer.name}. Use Bambu Connect from Production.`
          : `Connection added to ${printer.name}. Send from Production when it is idle.`,
      );
      await refresh();
    } catch (e) {
      setLoadError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const onTest = async (integrationId: string) => {
    setTestingId(integrationId);
    setMessage(null);
    setLoadError(null);
    try {
      const result = await testIntegration(integrationId);
      setMessage(result.ok ? result.message ?? "Connected." : result.message ?? "Test failed.");
      void refreshStatus(integrationId);
      void queryClient.invalidateQueries({ queryKey: queryKeys.tlsPin(integrationId) });
    } catch (e) {
      setLoadError(e instanceof Error ? e.message : String(e));
    } finally {
      setTestingId(null);
    }
  };

  const onToggleEnabled = async (printer: PrinterMachine, enabled: boolean) => {
    const integrationId = printer.integration_id?.trim();
    if (!integrationId) return;
    setBusy(true);
    setLoadError(null);
    try {
      await updateIntegration(integrationId, { config: { enabled } });
      await refresh();
      if (enabled) void refreshStatus(integrationId);
    } catch (e) {
      setLoadError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const onRemove = async (printer: PrinterMachine) => {
    setBusy(true);
    setLoadError(null);
    setMessage(null);
    try {
      const integrationId = printer.integration_id?.trim();
      await deletePrinter(printer.id);
      if (integrationId) {
        try {
          await deleteIntegration(integrationId);
        } catch {
          /* host may already be gone */
        }
      }
      setMessage("Printer removed.");
      await refresh();
    } catch (e) {
      setLoadError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const onStartEdit = (printer: PrinterMachine) => {
    setConnectingId(null);
    setEditingId(printer.id);
    setDetailsDraft(printerDetailsDraft(printer));
    setLoadError(null);
    setMessage(null);
  };

  const onDetailsDraftChange = (field: PrinterDetailsTextField, value: string) => {
    setDetailsDraft((prev) => ({
      ...prev,
      [field]: value,
      presetId: field === "name" ? prev.presetId : null,
    }));
  };

  const onEditPresetChange = (nextPresetId: string | null) => {
    if (!nextPresetId) {
      setDetailsDraft((prev) => ({ ...prev, presetId: null }));
      return;
    }
    const preset = presets.find((candidate) => candidate.id === nextPresetId);
    if (!preset) return;
    setDetailsDraft((prev) => ({
      ...prev,
      model: preset.model_slug ?? preset.name,
      bedWidth: String(preset.bed_width_mm),
      bedDepth: String(preset.bed_depth_mm),
      bedHeight: preset.bed_height_mm == null ? "" : String(preset.bed_height_mm),
      margin: "4",
      maxFilamentSlots: String(preset.max_filament_slots),
      presetId: preset.id,
    }));
  };

  const onSaveDetails = async (printer: PrinterMachine) => {
    let details: PrinterDetailsInput;
    try {
      details = parsePrinterDetailsDraft(detailsDraft);
    } catch (error) {
      setLoadError(error instanceof Error ? error.message : String(error));
      return;
    }
    setBusy(true);
    setLoadError(null);
    setMessage(null);
    try {
      const updated = await updatePrinterDetails(printer.id, details);
      setPrinters((prev) =>
        prev.map((candidate) => (candidate.id === printer.id ? updated : candidate)),
      );
      setEditingId(null);
      setMessage("Printer details saved.");
      const integrationId = printer.integration_id?.trim();
      if (integrationId && updated.name !== printer.name) {
        try {
          await updateIntegration(integrationId, { name: updated.name });
          setIntegrations((prev) =>
            prev.map((host) =>
              host.id === integrationId ? { ...host, name: updated.name } : host,
            ),
          );
        } catch (error) {
          setLoadError(
            `Printer details saved, but the host name could not be updated: ${
              error instanceof Error ? error.message : String(error)
            }`,
          );
        }
      }
    } catch (e) {
      setLoadError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const onSlotColorChange = async (
    printerId: string,
    slot: number,
    filamentColorId: string | null,
    label: string,
  ) => {
    const next = printers.map((p) => {
      if (p.id !== printerId) return p;
      return {
        ...p,
        loaded_filaments: p.loaded_filaments.map((lf) =>
          lf.slot === slot
            ? {
                ...lf,
                filament_color_id: filamentColorId?.trim() || null,
                label: label.trim(),
              }
            : lf,
        ),
      };
    });
    setPrinters(next);
    setBusy(true);
    setLoadError(null);
    try {
      const saved = await savePrinterFleet(next);
      setPrinters(saved);
    } catch (e) {
      setLoadError(e instanceof Error ? e.message : String(e));
      await refresh();
    } finally {
      setBusy(false);
    }
  };

  const onSlicerOverrideChange = async (
    printer: PrinterMachine,
    value: SlicerOverride | "auto",
  ) => {
    const next = value === "auto" ? null : value;
    setBusy(true);
    setLoadError(null);
    try {
      const updated = await updatePrinterSlicer(printer.id, next);
      setPrinters((prev) => prev.map((p) => (p.id === printer.id ? updated : p)));
    } catch (e) {
      setLoadError(e instanceof Error ? e.message : String(e));
      await refresh();
    } finally {
      setBusy(false);
    }
  };

  const canAdd = printerSettingsCanAdd({
    engineReady,
    busy,
    name: newName,
    presetId,
    customPresetId: CUSTOM_PRESET_ID,
    customWidth,
    customDepth,
    customHeight,
  });
  const connectionReady = printerHostConnectionReady({
    hostType,
    url: newUrl,
    password,
    bambuHost,
    accessCode,
    serial,
  });

  const inputClass = INPUT_CLASS;

  const namePlaceholder = "Shop Printer";

  const linkedPrinterList = linkedPrinters(printers, hostsById);
  const orphanPrinterList = orphanPrinters(printers, hostsById);

  return (
    <Card>
      <CardHeader accent>
        <div className="flex items-start gap-3">
          <span className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-accent-brand/10 text-accent-brand">
            <Printer className="h-4 w-4" aria-hidden />
          </span>
          <div>
            <CardTitle level={3} className="text-base">Printers</CardTitle>
            <CardDescription>
              {hostedPlanning
                ? `Printers here are for packing, export, and Checkoff. ${HOSTED_PLANNING_COMPOSE_NOTE}`
                : "Create a printer from a bed preset or custom size. Planning and local 3MF work without a connection. Add a host later for status and sending."}
            </CardDescription>
          </div>
        </div>
      </CardHeader>
      <CardContent className="space-y-4">
        {loadError && <p className="text-sm text-destructive">{loadError}</p>}
        {message && <p className="text-sm text-muted-foreground">{message}</p>}

        {hostedPlanning ? null : (
        <label className="block max-w-md text-sm">
          <span className="mb-1 block font-medium">Printer status refresh</span>
          <span className="mb-1.5 block text-xs text-muted-foreground">
            How often Checkoff, Production, and the Printers page ask linked hosts for status
            while the page is open.
          </span>
          <Select
            value={String(pollSeconds)}
            onValueChange={(v) => {
              const next = Number(v) as PrinterStatusPollSeconds;
              setPollSeconds(next);
              writePrinterStatusPollSeconds(next);
            }}
          >
            <SelectTrigger className="min-h-10 w-full">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {PRINTER_STATUS_POLL_SECONDS_OPTIONS.map((s) => (
                <SelectItem key={s} value={String(s)}>
                  Every {s} seconds
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </label>
        )}

        <div className="space-y-2 rounded-md border border-border p-3">
          <p className="text-sm font-medium">Add printer</p>
          <p className="text-xs text-muted-foreground">
            Printers are manual by default. You can plan, export files, and record prints without connecting.
          </p>
          <label className="block text-sm">
            <span className="mb-1 block text-muted-foreground">Name</span>
            <input
              className={inputClass}
              value={newName}
              onChange={(e) => setNewName(e.target.value)}
              placeholder={namePlaceholder}
              disabled={!engineReady || busy}
            />
          </label>

          <label className="block text-sm">
            <span className="mb-1 block text-muted-foreground">
              Bed size (for 3MF packing)
            </span>
            <Select
              value={presetId}
              onValueChange={setPresetId}
              disabled={!engineReady || busy || presets.length === 0}
            >
              <SelectTrigger className="min-h-10 w-full max-w-none sm:max-w-md">
                <SelectValue placeholder="Choose bed size" />
              </SelectTrigger>
              <SelectContent>
                {presets.map((p) => (
                  <SelectItem key={p.id} value={p.id}>
                    {p.name} ({p.bed_width_mm}×{p.bed_depth_mm} mm)
                  </SelectItem>
                ))}
                <SelectItem value={CUSTOM_PRESET_ID}>Custom</SelectItem>
              </SelectContent>
            </Select>
          </label>

          {presetId === CUSTOM_PRESET_ID && (
            <div className="grid gap-2 sm:grid-cols-3">
              <label className="block text-sm">
                <span className="mb-1 block text-muted-foreground">Width (mm)</span>
                <input
                  className={inputClass}
                  inputMode="numeric"
                  value={customWidth}
                  onChange={(e) => setCustomWidth(e.target.value)}
                  disabled={!engineReady || busy}
                />
              </label>
              <label className="block text-sm">
                <span className="mb-1 block text-muted-foreground">Depth (mm)</span>
                <input
                  className={inputClass}
                  inputMode="numeric"
                  value={customDepth}
                  onChange={(e) => setCustomDepth(e.target.value)}
                  disabled={!engineReady || busy}
                />
              </label>
              <label className="block text-sm">
                <span className="mb-1 block text-muted-foreground">Height (mm)</span>
                <input
                  className={inputClass}
                  inputMode="numeric"
                  value={customHeight}
                  onChange={(e) => setCustomHeight(e.target.value)}
                  disabled={!engineReady || busy}
                />
              </label>
            </div>
          )}

          {hostedPlanning ? null : (
          <label className="flex items-center gap-2 text-sm">
            <Checkbox
              checked={connectNewPrinter}
              onCheckedChange={(checked) => setConnectNewPrinter(checked === true)}
              disabled={!engineReady || busy}
            />
            Connect to this printer
          </label>
          )}
          <p className="text-xs text-muted-foreground">
            {hostedPlanning
              ? HOSTED_PLANNING_COMPOSE_NOTE
              : "Optional. Configure a connection after adding the printer for live status, file access, and sending."}
          </p>
          <Button className="min-h-10" disabled={!canAdd} onClick={() => void onAddPrinter()}>
            Add printer
          </Button>
        </div>

        <ul className="space-y-3">
          {linkedPrinterList.map((printer) => {
            const linkedId = printer.integration_id?.trim() || "";
            const host = hostsById.get(linkedId);
            const hostTypeKey = isPrinterHostType(host?.type) ? host.type : "moonraker";
            const typeLabel = PRINTER_HOST_TYPE_LABELS[hostTypeKey] ?? host?.type ?? "Printer";
            const enabled = host?.config.enabled !== false;
            const status = linkedId ? statusByIntegration[linkedId] : null;
            const detail =
              host?.type === "bambu"
                ? String(host.config.host ?? host.config.hostname ?? "")
                : String(host?.config.base_url ?? host?.config.baseUrl ?? "");
            return (
              <li
                key={printer.id}
                className="space-y-2 rounded-md border border-border px-3 py-2.5"
              >
                <div className="flex flex-col gap-2 sm:flex-row sm:items-start sm:justify-between">
                  <div className="min-w-0 flex-1 space-y-1">
                    <p className="text-sm font-medium">
                      {printer.name}{" "}
                      <span className="font-normal text-muted-foreground">
                        ({typeLabel})
                      </span>
                    </p>
                    <p className="truncate font-mono text-xs text-muted-foreground">
                      {detail}
                      {" · "}
                      Bed {printer.bed_width_mm}×{printer.bed_depth_mm} mm
                    </p>
                    <p className="text-xs text-muted-foreground">
                      {printer.preset_id
                        ? `Preset reference: ${printer.preset_id}`
                        : "Custom geometry"}
                    </p>
                  </div>
                  <span
                    className={cn(
                      "inline-flex min-h-8 shrink-0 items-center rounded-md px-2.5 text-xs font-medium",
                      statusTone({ tone: printerStatusTone(status?.state), emphasis: "soft" }),
                    )}
                    title={status?.message}
                  >
                    {enabled ? statusPillLabel(status) : "Connection off"}
                  </span>
                </div>

                {desktopRebuild && !hostedPlanning && host?.type === "bambu" && linkedId ? (
                  <BambuCertificatePin
                    integrationId={linkedId}
                    disabled={!engineReady || busy || testingId === linkedId}
                    onMessage={setMessage}
                    onError={setLoadError}
                  />
                ) : null}

                <div className="flex flex-wrap items-center gap-2">
                  {hostedPlanning ? null : (
                    <>
                  <label
                    htmlFor={`printer-enabled-${printer.id}`}
                    className="flex items-center gap-2 text-sm"
                  >
                    <Checkbox
                      id={`printer-enabled-${printer.id}`}
                      checked={enabled}
                      disabled={busy || !linkedId}
                      onCheckedChange={(next) => void onToggleEnabled(printer, next === true)}
                    />
                    Communication enabled
                  </label>
                  <Button
                    variant="secondary"
                    size="sm"
                    disabled={!engineReady || testingId === linkedId || !linkedId || !enabled}
                    onClick={() => void onTest(linkedId)}
                  >
                    {testingId === linkedId ? "Testing…" : "Test connection"}
                  </Button>
                    </>
                  )}
                  <Button
                    variant="outline"
                    size="sm"
                    disabled={busy}
                    onClick={() => onStartEdit(printer)}
                  >
                    Edit printer
                  </Button>
                  <ConfirmDialog
                    trigger={
                      <Button variant="destructive" size="sm" disabled={busy}>
                        Remove
                      </Button>
                    }
                    title="Remove this printer?"
                    description={removePrinterConsequence(printer)}
                    confirmLabel="Remove printer"
                    disabled={busy}
                    onConfirm={() => void onRemove(printer)}
                  />
                  <label className="flex items-center gap-2 text-sm">
                    <span className="text-xs text-muted-foreground">Preferred slicer:</span>
                    <Select
                      value={printer.preferred_slicer ?? "auto"}
                      onValueChange={(v) =>
                        void onSlicerOverrideChange(printer, v as SlicerOverride | "auto")
                      }
                      disabled={!engineReady || busy}
                    >
                      <SelectTrigger className="h-8 w-40 text-xs">
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        <SelectItem value="auto">Auto</SelectItem>
                        {SLICER_OVERRIDES.map((k) => (
                          <SelectItem key={k} value={k}>
                            {SLICER_OVERRIDE_LABELS[k]}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  </label>
                </div>

                {editingId === printer.id && (
                  <PrinterDetailsEditor
                    draft={detailsDraft}
                    presets={presets}
                    disabled={busy}
                    onChange={onDetailsDraftChange}
                    onPresetChange={onEditPresetChange}
                    onSave={() => void onSaveDetails(printer)}
                    onCancel={() => setEditingId(null)}
                  />
                )}

                <p className="text-xs text-muted-foreground">
                  Accepted Plate export does not choose slicer profiles.
                </p>

                {printer.loaded_filaments.length > 0 && (
                  <div className="grid gap-2 sm:grid-cols-2">
                    {printer.loaded_filaments.map((slot) => (
                      <SlotFilamentPicker
                        key={slot.slot}
                        slot={slot.slot}
                        extraLabel={slot.label && slot.label !== slot.filament_color_id ? slot.label : undefined}
                        filamentColorId={slot.filament_color_id}
                        catalog={catalog}
                        disabled={!engineReady || busy}
                        onChange={(colorId, label) => {
                          void onSlotColorChange(printer.id, slot.slot, colorId, label);
                        }}
                      />
                    ))}
                  </div>
                )}

                <PrinterProfileAssignmentSection
                  printer={printer}
                  engineReady={engineReady}
                  disabled={busy}
                />

                {host?.capabilities?.status === true && (
                  <div className="flex items-center gap-2 mt-2">
                    <span className="text-xs text-muted-foreground min-w-fit">Default plan:</span>
                    <Select
                      value={planBindings.find(b => b.integration_id === printer.integration_id)?.profile_id?.toString() ?? "none"}
                      onValueChange={(val) => {
                        const profileId = val === "none" ? null : Number(val);
                        void savePrinterPlanBinding(printer.integration_id!, profileId)
                          .then(setPlanBindings);
                      }}
                    >
                      <SelectTrigger className="h-7 text-xs">
                        <SelectValue placeholder="No default plan" />
                      </SelectTrigger>
                      <SelectContent>
                        <SelectItem value="none">No default plan</SelectItem>
                        {profiles.map(p => (
                          <SelectItem key={p.id} value={String(p.id)}>{p.name}</SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  </div>
                )}
              </li>
            );
          })}
        </ul>

        {orphanPrinterList.length > 0 && (
          <div className="space-y-2">
            <p className="text-sm font-medium text-muted-foreground">
              Planning printers
            </p>
            <ul className="space-y-2">
              {orphanPrinterList.map((printer) => {
                const attaching = connectingId === printer.id;
                return (
                  <li
                    key={printer.id}
                    className="flex flex-col gap-2 rounded-md border border-dashed border-border px-3 py-2.5"
                  >
                    <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
                      <div className="min-w-0">
                        <p className="text-sm font-medium">{printer.name}</p>
                        <p className="text-xs text-muted-foreground tabular">
                          Bed {printer.bed_width_mm}×{printer.bed_depth_mm} mm · planning only
                        </p>
                        <p className="text-xs text-muted-foreground">
                          {printer.preset_id
                            ? `Preset reference: ${printer.preset_id}`
                            : "Custom geometry"}
                        </p>
                      </div>
                      <div className="flex flex-wrap gap-2">
                        {!attaching && editingId !== printer.id && (
                          <Button
                            variant="outline"
                            size="sm"
                            disabled={busy}
                            onClick={() => onStartEdit(printer)}
                          >
                            Edit printer
                          </Button>
                        )}
                        {!attaching && editingId !== printer.id && !hostedPlanning && (
                          <Button
                            variant="secondary"
                            size="sm"
                            disabled={busy}
                            onClick={() => {
                              setEditingId(null);
                              setConnectingId(printer.id);
                            }}
                          >
                            Add connection
                          </Button>
                        )}
                        <ConfirmDialog
                          trigger={
                            <Button variant="destructive" size="sm" disabled={busy}>
                              Remove
                            </Button>
                          }
                          title="Remove this printer?"
                          description={removePrinterConsequence(printer)}
                          confirmLabel="Remove printer"
                          disabled={busy}
                          onConfirm={() => void onRemove(printer)}
                        />
                      </div>
                    </div>

                    {editingId === printer.id && (
                      <PrinterDetailsEditor
                        draft={detailsDraft}
                        presets={presets}
                        disabled={busy}
                        onChange={onDetailsDraftChange}
                        onPresetChange={onEditPresetChange}
                        onSave={() => void onSaveDetails(printer)}
                        onCancel={() => setEditingId(null)}
                      />
                    )}

                    {attaching && !hostedPlanning && (
                      <div className="space-y-2 border-t border-border pt-2">
                        <label className="block text-sm">
                          <span className="mb-1 block text-muted-foreground">Type</span>
                          <Select
                            value={hostType}
                            onValueChange={(value) => {
                              if (isPrinterHostType(value)) setHostType(value);
                            }}
                            disabled={!engineReady || busy}
                          >
                            <SelectTrigger className="min-h-10 w-full max-w-none sm:max-w-md">
                              <SelectValue />
                            </SelectTrigger>
                            <SelectContent>
                              <SelectItem value="moonraker">Klipper (Moonraker)</SelectItem>
                              <SelectItem value="prusalink">Prusa (PrusaLink)</SelectItem>
                              <SelectItem value="bambu">Bambu (LAN + Connect)</SelectItem>
                            </SelectContent>
                          </Select>
                        </label>

                        {hostType === "bambu" ? (
                          <>
                            <label className="block text-sm">
                              <span className="mb-1 block text-muted-foreground">
                                Printer IP / hostname
                              </span>
                              <input
                                className={inputClass}
                                value={bambuHost}
                                onChange={(event) => setBambuHost(event.target.value)}
                                placeholder="192.168.1.60"
                                disabled={!engineReady || busy}
                              />
                            </label>
                            <label className="block text-sm">
                              <span className="mb-1 block text-muted-foreground">
                                LAN access code
                              </span>
                              <input
                                className={inputClass}
                                type="password"
                                autoComplete="off"
                                value={accessCode}
                                onChange={(event) => setAccessCode(event.target.value)}
                                disabled={!engineReady || busy}
                              />
                            </label>
                            <label className="block text-sm">
                              <span className="mb-1 block text-muted-foreground">
                                Serial / device id
                              </span>
                              <input
                                className={inputClass}
                                value={serial}
                                onChange={(event) => setSerial(event.target.value)}
                                disabled={!engineReady || busy}
                              />
                            </label>
                          </>
                        ) : (
                          <>
                            <label className="block text-sm">
                              <span className="mb-1 block text-muted-foreground">Base URL</span>
                              <input
                                className={inputClass}
                                value={newUrl}
                                onChange={(event) => setNewUrl(event.target.value)}
                                placeholder={
                                  hostType === "moonraker"
                                    ? "http://192.168.1.40:7125"
                                    : "http://192.168.1.50"
                                }
                                disabled={!engineReady || busy}
                              />
                            </label>
                            {hostType === "moonraker" ? (
                              <label className="block text-sm">
                                <span className="mb-1 block text-muted-foreground">
                                  API key (optional for trusted clients)
                                </span>
                                <input
                                  className={inputClass}
                                  type="password"
                                  autoComplete="off"
                                  value={apiKey}
                                  onChange={(event) => setApiKey(event.target.value)}
                                  disabled={!engineReady || busy}
                                />
                              </label>
                            ) : (
                              <>
                                <label className="block text-sm">
                                  <span className="mb-1 block text-muted-foreground">
                                    Digest username
                                  </span>
                                  <input
                                    className={inputClass}
                                    value={username}
                                    onChange={(event) => setUsername(event.target.value)}
                                    disabled={!engineReady || busy}
                                  />
                                </label>
                                <label className="block text-sm">
                                  <span className="mb-1 block text-muted-foreground">
                                    Digest password (printer API key)
                                  </span>
                                  <input
                                    className={inputClass}
                                    type="password"
                                    autoComplete="off"
                                    value={password}
                                    onChange={(event) => setPassword(event.target.value)}
                                    disabled={!engineReady || busy}
                                  />
                                </label>
                              </>
                            )}
                          </>
                        )}

                        <div className="flex flex-wrap gap-2">
                          <Button
                            size="sm"
                            disabled={!connectionReady || busy}
                            onClick={() => void onAttachConnection(printer)}
                          >
                            Save connection
                          </Button>
                          <Button
                            variant="outline"
                            size="sm"
                            disabled={busy}
                            onClick={() => setConnectingId(null)}
                          >
                            Cancel
                          </Button>
                        </div>
                      </div>
                    )}
                  </li>
                );
              })}
            </ul>
          </div>
        )}

        {!linkedPrinterList.length && !orphanPrinterList.length && engineReady && (
          <p className="text-sm text-muted-foreground">
            No printers yet. Add one above to plan Plates and export 3MF. A connection is
            optional until you need status or sending.
          </p>
        )}
      </CardContent>
    </Card>
  );
}
