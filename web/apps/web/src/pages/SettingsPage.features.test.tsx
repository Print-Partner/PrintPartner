// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter } from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import {
  TENANT_FEATURE_TOGGLES_CAPABILITY,
  type FeatureSettings,
} from "@print-partner/contracts";
import SettingsPage from "./SettingsPage";
import { fetchDiscordNotifySettings, saveDiscordNotifySettings } from "../api/endpoints/settings";

const state = vi.hoisted((): { capabilities: string[]; featureSettings: FeatureSettings; patchCalls: unknown[]; fetchError: Error | null; patchError: Error | null } => ({
  capabilities: [],
  featureSettings: {
    choices: {},
    effective: {
      farm: { enabled: false, source: "default" },
      orders: { enabled: false, source: "default" },
      bambu: { enabled: false, source: "default" },
      spoolman: { enabled: false, source: "default" },
    },
  },
  patchError: null,
  fetchError: null,
  patchCalls: [],
}));

vi.mock("../hooks/useEngineHealth", () => ({
  useEngineHealth: () => ({
    health: { ok: true, capabilities: state.capabilities },
    error: null,
    loading: false,
    fetching: false,
    refresh: vi.fn(),
  }),
}));

vi.mock("../hooks/useAppUpdateCheck", () => ({
  useAppUpdateCheck: () => ({
    updateCheck: null,
    refresh: vi.fn(),
  }),
}));

vi.mock("../hooks/useJobRunner", () => ({
  useJobRunner: () => ({
    running: false,
    run: vi.fn(),
  }),
}));

vi.mock("../context/AuthContext", () => ({
  useAuth: () => ({
    user: { id: "user-1", email: "user@example.com", display_name: "User", hasPassword: true },
    authRequired: false,
    logout: vi.fn(),
  }),
}));

vi.mock("../context/DateFormatContext", () => ({
  useDateFormat: () => ({
    format: "iso",
    setFormat: vi.fn(),
  }),
}));

vi.mock("../queries/buildTracking", () => ({
  useBuildTrackingSettingsQuery: () => ({
    data: { assembly_tracking: false },
    error: null,
    isLoading: false,
  }),
  useSaveBuildTrackingSettingsMutation: () => ({
    mutate: vi.fn(),
    isPending: false,
  }),
}));

vi.mock("../queries/externalAccess", () => ({
  useExternalAccessSettingsQuery: () => ({
    data: { mode: "off" },
    error: null,
    isLoading: false,
  }),
}));

vi.mock("../api/endpoints/settings", () => ({
  fetchDiscordNotifySettings: vi.fn().mockResolvedValue({
    webhook_url: null,
    notify_on_update: false,
    notify_on_sync: false,
    auto_sync_updates: false,
  }),
  saveDiscordNotifySettings: vi.fn(),
  testDiscordNotify: vi.fn(),
  fetchDateFormatSetting: vi.fn().mockResolvedValue({ format: "iso" }),
  saveDateFormatSetting: vi.fn(),
  fetchBuildTrackingSettings: vi.fn().mockResolvedValue({ assembly_tracking: false }),
  saveBuildTrackingSettings: vi.fn(),
  fetchExternalAccessSettings: vi.fn().mockResolvedValue({ mode: "off" }),
  saveExternalAccessSettings: vi.fn(),
}));

vi.mock("../api/endpoints/filaments", () => ({
  createCustomFilament: vi.fn(),
  deleteCustomFilament: vi.fn(),
  fetchCustomFilaments: vi.fn().mockResolvedValue([]),
}));

vi.mock("../api/endpoints/sourceContent", () => ({
  fetchGitHubPatSettings: vi.fn().mockResolvedValue({ configured: false, token_last_four: null }),
  fetchSourceUpdateCheckSettings: vi.fn().mockResolvedValue({ interval_hours: 24 }),
  saveGitHubPat: vi.fn(),
  saveSourceMonitoringSettings: vi.fn(),
  saveSourceUpdateCheckInterval: vi.fn(),
  startCheckSourceUpdates: vi.fn(),
}));

vi.mock("../components/settings/StlNamingEditor", () => ({
  StlNamingSettingsCard: () => <div data-testid="stl-naming-card" />,
}));
vi.mock("../components/settings/IntegrationsSettingsCard", () => ({
  default: () => <div data-testid="integrations-card" />,
}));
vi.mock("../components/settings/PrintersSettingsCard", () => ({
  default: () => <div data-testid="printers-card" />,
}));
vi.mock("../components/settings/SlicersSettingsCard", () => ({
  default: () => <div data-testid="slicers-card" />,
}));
vi.mock("../components/settings/AboutUpdatesCard", () => ({
  default: () => <div data-testid="about-updates-card" />,
}));
vi.mock("../components/settings/ExternalToolsSettingsCard", () => ({
  default: () => <div data-testid="external-tools-card" />,
}));
vi.mock("../components/settings/BackupManagementCard", () => ({
  default: () => <div data-testid="backup-card" />,
}));
vi.mock("../components/settings/ApiKeyManagementCard", () => ({
  default: () => <div data-testid="api-key-card" />,
}));
vi.mock("../components/settings/LoggingManagementCard", () => ({
  default: () => <div data-testid="logging-card" />,
}));
vi.mock("../components/sources/SourceCategoryManager", () => ({
  default: () => <div data-testid="category-manager" />,
}));
vi.mock("../components/ThemePreferenceControl", () => ({
  default: () => <div data-testid="theme-control" />,
}));

vi.mock("../api/endpoints/tenantFeatures", () => ({
  fetchTenantFeatures: vi.fn().mockImplementation(async () => {
    if (state.fetchError) throw state.fetchError;
    return state.featureSettings;
  }),
  patchTenantFeatures: vi.fn().mockImplementation(async (patch) => {
    state.patchCalls.push(patch);
    if (state.patchError) throw state.patchError;
    const nextChoices = { ...state.featureSettings.choices, ...patch };
    state.featureSettings = {
      choices: nextChoices,
      effective: {
        farm: { enabled: nextChoices.farm ?? false, source: nextChoices.farm ? "user" : "default" },
        orders: { enabled: nextChoices.orders ?? false, source: nextChoices.orders ? "user" : "default" },
        bambu: { enabled: nextChoices.bambu ?? false, source: nextChoices.bambu ? "user" : "default" },
        spoolman: { enabled: nextChoices.spoolman ?? false, source: nextChoices.spoolman ? "user" : "default" },
      },
    };
    return state.featureSettings;
  }),
}));

function renderSettingsPage() {
  const queryClient = new QueryClient({
    defaultOptions: {
      queries: { retry: false },
    },
  });
  return render(
    <QueryClientProvider client={queryClient}>
      <MemoryRouter>
        <SettingsPage />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

describe("SettingsPage tenant feature toggles gate and controls", () => {
  beforeEach(() => {
    state.capabilities = [];
    state.patchError = null;
    state.fetchError = null;
    state.patchCalls = [];
    state.featureSettings = {
      choices: {},
      effective: {
        farm: { enabled: false, source: "default" },
        orders: { enabled: false, source: "default" },
        bambu: { enabled: false, source: "default" },
        spoolman: { enabled: false, source: "default" },
      },
    };
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("keeps a saved redacted webhook out of the replacement input", async () => {
    const saved = {
      webhook_url: "https://discord.com/[REDACTED]",
      notify_on_update: true,
      notify_on_sync: false,
      auto_sync_updates: true,
    };
    vi.mocked(fetchDiscordNotifySettings).mockResolvedValueOnce(saved);
    vi.mocked(saveDiscordNotifySettings).mockResolvedValueOnce(saved);
    renderSettingsPage();
    expect(await screen.findByLabelText("Saved webhook URL")).toBeTruthy();
    expect(screen.queryByLabelText("Webhook URL")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Replace" }));
    const input = screen.getByLabelText("Webhook URL") as HTMLInputElement;
    expect(input.value).toBe("");
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(screen.queryByLabelText("Webhook URL")).toBeNull();
    expect(saveDiscordNotifySettings).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Replace" }));
    const replacement = "https://discord.com/api/webhooks/123/obviously-fake-replacement";
    fireEvent.change(screen.getByLabelText("Webhook URL"), { target: { value: replacement } });
    const section = screen.getByLabelText("Webhook URL").closest<HTMLElement>("[data-surface]")!;
    fireEvent.click(within(section).getByRole("button", { name: "Save" }));
    await waitFor(() => expect(saveDiscordNotifySettings).toHaveBeenCalledWith({ webhook_url: replacement }));
    await screen.findByLabelText("Saved webhook URL");
    expect(screen.queryByLabelText("Webhook URL")).toBeNull();
  });

  it("omits feature toggles controls when TENANT_FEATURE_TOGGLES_CAPABILITY is absent", () => {
    state.capabilities = [];
    renderSettingsPage();

    expect(screen.queryByText("Feature toggles")).toBeNull();
    expect(screen.queryByLabelText(/Print farm/i)).toBeNull();
    expect(screen.queryByLabelText(/Orders/i)).toBeNull();
    expect(screen.queryByLabelText(/Bambu printers/i)).toBeNull();
    expect(screen.queryByLabelText(/Spoolman/i)).toBeNull();
  });

  it("renders feature toggles controls with defaults off when capability is present", async () => {
    state.capabilities = [TENANT_FEATURE_TOGGLES_CAPABILITY];
    renderSettingsPage();

    expect(await screen.findByText("Feature toggles")).toBeTruthy();
    const farmSwitch = screen.getByLabelText(/Print farm/i);
    const ordersSwitch = screen.getByLabelText(/Orders/i);
    const bambuSwitch = screen.getByLabelText(/Bambu printers/i);
    const spoolmanSwitch = screen.getByLabelText(/Spoolman/i);

    expect(farmSwitch.getAttribute("aria-checked")).toBe("false");
    expect(ordersSwitch.getAttribute("aria-checked")).toBe("false");
    expect(bambuSwitch.getAttribute("aria-checked")).toBe("false");
    expect(spoolmanSwitch.getAttribute("aria-checked")).toBe("false");
  });

  it("handles functional PATCH toggling and displays saved state", async () => {
    state.capabilities = [TENANT_FEATURE_TOGGLES_CAPABILITY];
    renderSettingsPage();

    const farmSwitch = await screen.findByLabelText(/Print farm/i);
    expect(farmSwitch.getAttribute("aria-checked")).toBe("false");

    fireEvent.click(farmSwitch);

    await waitFor(() => {
      expect(state.patchCalls).toContainEqual({ farm: true });
      expect(farmSwitch.getAttribute("aria-checked")).toBe("true");
    });
  });

  it("displays load error when fetch fails", async () => {
    state.capabilities = [TENANT_FEATURE_TOGGLES_CAPABILITY];
    state.fetchError = new Error("Failed to load features");
    renderSettingsPage();

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("Could not load feature toggles: Failed to load features");
  });

  it("displays save error when patch mutation rejects", async () => {
    state.capabilities = [TENANT_FEATURE_TOGGLES_CAPABILITY];
    renderSettingsPage();

    const farmSwitch = await screen.findByLabelText(/Print farm/i);
    state.patchError = new Error("Network error during save");

    fireEvent.click(farmSwitch);

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("Failed to save feature toggle: Network error during save");
  });
});
