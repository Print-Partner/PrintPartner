import type { FilamentShortfalls, Health, ProfileSummary } from "@print-partner/rust-client";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { createMemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App, createQueryClient } from "../App";
import { OFFLINE_REASON } from "../frame/connection";
import { routes } from "../routes";

// A scripted core. The rows match rust/crates/pp-core/tests/filament_shortfalls_http.rs,
// which builds the same Build from fixtures/integrations/spoolman.
const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });

const BUILD: ProfileSummary = {
  id: 1,
  name: "Synthetic Build 01",
  order_number: null,
  special_request: null,
  part_count: 1,
  accepted_progress: { kind: "ready", total_units: 3, remaining_units: 1 },
  build_stale: false,
  freshness: { status: "current", accepted_input_set_id: 1, accepted_at: "2026-10-02T00:05:00Z" },
  archived_at: null,
  last_used_at: null,
  customer: null,
  due_date: null,
};

const health: Health = {
  ok: true,
  authenticated: true,
  authentication_required: true,
  capabilities: [],
  degraded_detail: null,
  discord_oauth_configured: false,
  github_oauth_configured: false,
  multi_user: false,
  owner_mapping_required: false,
  registration_open: false,
  single_user_auth: true,
  single_user_setup_required: false,
  modules: [{ module: "spoolman", state: "on" }],
};

type Row = Extract<FilamentShortfalls, { kind: "ready" }>["filaments"][number];
const MEASURED: Row["grams_needed"] = { kind: "measured", grams: 42, job_count: 1 };
const LABEL = "Prusament PLA · Galaxy Black";
const FILAMENT_ID = "spoolman:local:filament:2";

const row = (extra: Partial<Row> = {}): Row => ({
  filament_color_id: FILAMENT_ID,
  label: LABEL,
  grams_needed: MEASURED,
  grams_available: { kind: "spoolman", grams: 752, spool_ids: [7, 8] },
  status: { kind: "sufficient" },
  parts: [{ part_id: 2, filename: "included.stl", remaining_units: 1, grams_needed: MEASURED }],
  ...extra,
});

const ready = (filaments: Row[]): FilamentShortfalls => ({
  kind: "ready",
  profile_id: 1,
  revision_id: 1,
  plan_version: 1,
  filaments,
});

const SHORT = row({
  grams_available: { kind: "spoolman", grams: 18, spool_ids: [7] },
  status: { kind: "short", short_grams: 24 },
});
const UNREACHABLE = row({
  label: "",
  grams_available: { kind: "unknown", reason: "unreachable" },
  status: { kind: "unknown" },
});

const SHORTFALLS = "/api/v2/plans/1/filament-shortfalls";

/** The Overview's reads, with the shortfall answer from `shortfalls`. */
function core(shortfalls: () => Response | Promise<Response>) {
  return vi.fn((path: string) => {
    if (path === "/health") return Promise.resolve(json(health));
    if (path === SHORTFALLS) return Promise.resolve(shortfalls());
    if (path === "/api/v2/plans/1") return Promise.resolve(json(BUILD));
    if (path === "/api/v2/plans/1/layers")
      return Promise.resolve(json({ profile_id: 1, layers: [] }));
    return Promise.resolve(json({ status: 404 }, 404));
  });
}

function renderOverview(fetchImpl: (path: string) => Promise<Response>) {
  vi.stubGlobal("fetch", fetchImpl);
  const router = createMemoryRouter(routes, { initialEntries: ["/builds/1"] });
  const queryClient = createQueryClient();
  queryClient.setDefaultOptions({
    queries: { ...queryClient.getDefaultOptions().queries, retryDelay: 0 },
  });
  render(<App router={router} queryClient={queryClient} />);
}

const section = () => screen.getByRole("region", { name: "Filament" });
const fact = (term: string) =>
  within(section()).getByText(term, { selector: "dt" }).nextElementSibling?.textContent;
const reads = (fetchMock: ReturnType<typeof core>) =>
  fetchMock.mock.calls.filter(([p]) => p === SHORTFALLS).length;

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe("GRE-1102 Filament section on the Build Overview", () => {
  it("shows each filament's need, Spoolman stock, status and Parts", async () => {
    renderOverview(core(() => json(ready([row()]))));
    expect(await screen.findByRole("heading", { level: 3, name: LABEL })).toBeTruthy();
    expect(fact("Needed")).toBe("42 gmeasured over 1 print");
    expect(fact("In Spoolman")).toBe("752 gon 2 spools");
    expect(fact("Status")).toBe("Enough");
    const parts = within(section()).getByRole("list", { name: `Parts on ${LABEL}` });
    expect(within(parts).getByRole("listitem").textContent).toBe("included.stl 1 unit left");
    expect(within(section()).getByRole("status").querySelector("time")).toBeTruthy();
  });

  it("Refresh reads again once and shows a shortfall as a tag", async () => {
    let answer = row();
    const fetchMock = core(() => json(ready([answer])));
    renderOverview(fetchMock);
    await screen.findByText("Enough");
    expect(reads(fetchMock)).toBe(1);
    answer = SHORT;
    fireEvent.click(within(section()).getByRole("button", { name: "Refresh" }));
    const tag = await within(section()).findByText("Short by 24 g");
    expect(tag.classList.contains("pp-tag")).toBe(true);
    expect(fact("In Spoolman")).toBe("18 gon 1 spool");
    expect(reads(fetchMock)).toBe(2);
  });

  it("unreachable Spoolman names the row by its filament ID and never shows 0 g or Short", async () => {
    let answer = UNREACHABLE;
    renderOverview(core(() => json(ready([answer]))));
    expect(await screen.findByRole("heading", { level: 3, name: FILAMENT_ID })).toBeTruthy();
    expect(fact("In Spoolman")).toBe("Not knownSpoolman could not be reached");
    expect(fact("Status")).toBe("Not known");
    expect(section().textContent).not.toMatch(/\b0 g|Short/);
    answer = row();
    fireEvent.click(within(section()).getByRole("button", { name: "Refresh" }));
    expect(await screen.findByRole("heading", { level: 3, name: LABEL })).toBeTruthy();
    expect(fact("In Spoolman")).toBe("752 gon 2 spools");
  });

  it("an unknown side shows Not known even if the status says short", async () => {
    const unknown = { kind: "unknown", reason: "grams_not_recorded" } as const;
    renderOverview(
      core(() =>
        json(ready([row({ grams_needed: unknown, status: { kind: "short", short_grams: 5 } })])),
      ),
    );
    await screen.findByRole("heading", { level: 3, name: LABEL });
    expect(fact("Needed")).toBe("Not known yetno print of this Build has recorded its grams");
    expect(fact("Status")).toBe("Not known");
    expect(section().textContent).not.toContain("Short");
  });

  it.each([
    [ready([]), "Nothing left to print."],
    [{ kind: "no_accepted_plan", profile_id: 1 }, "No accepted Plan yet."],
  ])("says what a %j answer means", async (body, text) => {
    renderOverview(core(() => json(body)));
    expect(
      await within(await screen.findByRole("region", { name: "Filament" })).findByText(text),
    ).toBeTruthy();
  });

  it("Module off shows the Module-off block with its Settings link", async () => {
    renderOverview(
      core(() => json({ kind: "module_off", module: "spoolman", reason: "default_off" })),
    );
    expect(await screen.findByText("The Spoolman module is off.")).toBeTruthy();
    expect(within(section()).getByRole("link", { name: "See Modules in Settings" })).toBeTruthy();
  });

  it("the rest of the Overview does not wait for Spoolman", async () => {
    renderOverview(core(() => new Promise<Response>(() => {})));
    expect(await screen.findByText("Progress", { selector: "dt" })).toBeTruthy();
    expect(within(section()).getByRole("status").textContent).toBe("Reading Spoolman");
  });

  it("a failed refresh keeps the rows with their time and offers Retry", async () => {
    let fail = false;
    renderOverview(core(() => (fail ? json({}, 503) : json(ready([row()])))));
    await screen.findByText("Enough");
    fail = true;
    fireEvent.click(within(section()).getByRole("button", { name: "Refresh" }));
    const status = await within(section()).findByText(
      /Could not refresh: Rust core answered 503/,
      {},
      { timeout: 3000 },
    );
    expect(status.querySelector("time")).toBeTruthy();
    expect(screen.getByText("Enough")).toBeTruthy();
    fail = false;
    fireEvent.click(within(section()).getByRole("button", { name: "Retry" }));
    await waitFor(() => expect(within(section()).queryByText(/Could not refresh/)).toBeNull());
    expect(screen.getByText("Enough")).toBeTruthy();
  });

  it("offline keeps the rows with their time and disables Refresh with the reason", async () => {
    let down = false;
    const live = core(() => json(ready([row()])));
    const fetchMock = vi.fn((path: string) =>
      down ? Promise.reject(new TypeError("Failed to fetch")) : live(path),
    );
    vi.useFakeTimers({ shouldAdvanceTime: true });
    renderOverview(fetchMock);
    await screen.findByText("Enough");
    down = true;
    await act(() => vi.advanceTimersByTimeAsync(31_000));
    await waitFor(() => expect(within(section()).getByText(OFFLINE_REASON)).toBeTruthy());
    expect(screen.getByText("Enough")).toBeTruthy();
    expect(within(section()).getByRole("status").querySelector("time")).toBeTruthy();
    const refresh = within(section()).getByRole("button", { name: "Refresh" });
    expect(refresh.getAttribute("aria-disabled")).toBe("true");
    const before = fetchMock.mock.calls.filter(([p]) => p === SHORTFALLS).length;
    fireEvent.click(refresh);
    expect(fetchMock.mock.calls.filter(([p]) => p === SHORTFALLS).length).toBe(before);
  });
});
