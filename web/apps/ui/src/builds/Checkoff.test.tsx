import type { CheckoffUnits, Health } from "@print-partner/rust-client";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { createMemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App, createQueryClient } from "../App";
import { OFFLINE_REASON } from "../frame/connection";
import { routes } from "../routes";

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });

// Farm and Spoolman are off: the tab must not need either.
const HEALTH: Health = {
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
  modules: [
    { module: "farm", state: "off", reason: "default_off" },
    { module: "spoolman", state: "off", reason: "default_off" },
  ],
};

const BASIS = {
  profile_id: 2,
  plan_version: 3,
  plan_revision_id: 7,
  plan_revision_digest: "sha256:aa",
  required_unit_mapping_digest: "sha256:bb",
};

const token = (n: number) => `ppu_${String(n).padStart(32, "0")}`;
const plate = (ordinal: number) => ({
  plate_id: `plate_${String(ordinal).padStart(32, "0")}`,
  ordinal,
  printer_name: ordinal === 1 ? "Voron 2.4" : "Trident",
});
const unit = (n: number, index: number, printed: boolean, onPlate: number | null) => ({
  token: token(n),
  unit_index: index,
  object_name: `unit ${n}`,
  printed,
  assembled: false,
  plate: onPlate === null ? null : plate(onPlate),
});
const part = (id: number, filename: string, units: ReturnType<typeof unit>[]) => ({
  part_id: id,
  filename,
  relative_path: filename,
  source_layer: "base",
  role: "part",
  quantity: units.length,
  printed_count: units.filter((u) => u.printed).length,
  assembled_count: units.filter((u) => u.assembled).length,
  units,
});

// bracket.stl units 1-3 (unit 1 printed; units 1 and 2 on Plate 1, unit 3 on Plate 2) and
// gear.stl units 1-2 (unit 1 on Plate 1, unit 2 on no Plate).
const units = (): CheckoffUnits => ({
  profile_id: 2,
  basis: BASIS,
  plate_revision_id: 4,
  parts: [
    part(11, "bracket.stl", [unit(1, 0, true, 1), unit(2, 1, false, 1), unit(3, 2, false, 2)]),
    part(12, "gear.stl", [unit(4, 0, false, 1), unit(5, 1, false, null)]),
  ],
});

const RECEIPT = {
  profile_id: 2,
  idempotency_key: "k",
  undo_token: "ppm_undo",
  printed_added: 2,
  printed_removed: 0,
  assembled_added: 0,
  assembled_removed: 0,
  parts: [],
};

type Init = RequestInit | undefined;

const UNDONE = { undo_token: "ppm_undo", undone_at: "2026-11-06T12:00:00Z" };
const answer = (path: string) => json(path.endsWith("/undo") ? UNDONE : RECEIPT);
type Handler = (path: string, init: Init) => Response | Promise<Response>;

function core(handler: Handler) {
  return vi.fn((path: string, init?: RequestInit) =>
    Promise.resolve(path === "/health" ? json(HEALTH) : handler(path, init)),
  );
}

const reads =
  (data: () => CheckoffUnits, write: Handler = answer): Handler =>
  (path, init) => {
    if (path === "/api/v2/plans/2/checkoff/units") return json(data());
    if (path.startsWith("/api/v2/plans/2/checkoff/bursts")) return write(path, init);
    return json(
      { type: "urn:print-partner:problem:not-found", title: "Not Found", status: 404 },
      404,
    );
  };

function renderAt(fetchImpl: ReturnType<typeof core>) {
  vi.stubGlobal("fetch", fetchImpl);
  const router = createMemoryRouter(routes, { initialEntries: ["/builds/2/checkoff"] });
  const queryClient = createQueryClient();
  queryClient.setDefaultOptions({
    queries: { ...queryClient.getDefaultOptions().queries, retryDelay: 0 },
    mutations: { ...queryClient.getDefaultOptions().mutations, retryDelay: 0 },
  });
  render(<App router={router} queryClient={queryClient} />);
}

const box = (filename: string, n: number) =>
  screen.getByRole<HTMLInputElement>("checkbox", { name: `${filename} unit ${n}` });
const preview = () =>
  screen.queryByRole("list", { name: "What the burst marks" })?.textContent ?? "";
const posts = (fetchMock: ReturnType<typeof core>) =>
  fetchMock.mock.calls
    .filter(([, init]) => init?.method === "POST")
    .map(([path, init]) => ({ path, body: JSON.parse(String(init!.body)) }));

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe("CHK-01 Checkoff tab", () => {
  it("shows accepted units with durable counts, with farm and Spoolman off", async () => {
    const fetchMock = core(reads(units));
    renderAt(fetchMock);
    const bracket = await screen.findByRole("group", { name: /^bracket\.stl/ });
    expect(within(bracket).getByText("bracket.stl").parentElement?.textContent).toContain(
      "1 of 3 printed, 0 assembled",
    );
    expect(box("bracket.stl", 1).checked).toBe(true);
    expect(box("bracket.stl", 2).checked).toBe(false);
    expect(screen.getByText(/Plate 2 on Trident/)).toBeTruthy();
    const paths = fetchMock.mock.calls.map(([p]) => p);
    expect(paths.every((p) => p === "/health" || p.startsWith("/api/v2/plans/2"))).toBe(true);
  });

  it("says so when the Build has no accepted Plan", async () => {
    renderAt(
      core(reads(() => ({ profile_id: 2, basis: null, plate_revision_id: null, parts: [] }))),
    );
    expect(await screen.findByText(/no accepted Plan yet/)).toBeTruthy();
  });
});

describe("CHK-02 burst keys", () => {
  it("P previews its Plate's units, then records one burst and offers one Undo", async () => {
    const fetchMock = core(reads(units));
    renderAt(fetchMock);
    await screen.findByRole("group", { name: /^bracket\.stl/ });
    fireEvent.keyDown(box("gear.stl", 1), { key: "p" });
    expect(preview()).toContain("bracket.stl: +1 printed (unit 2)");
    expect(preview()).toContain("gear.stl: +1 printed (unit 1)");
    expect(preview()).not.toContain("unit 3");
    fireEvent.click(screen.getByRole("button", { name: "Record 2 picked units" }));
    await screen.findByText(/Recorded: \+2 printed/);
    const [sent] = posts(fetchMock);
    expect(sent!.path).toBe("/api/v2/plans/2/checkoff/bursts");
    expect(sent!.body.expected).toEqual(BASIS);
    expect(sent!.body.units).toEqual([
      { token: token(2), printed: true },
      { token: token(4), printed: true },
    ]);
    expect(sent!.body.idempotency_key).toMatch(/\S/);
    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    await screen.findByText(/Undid the last burst/);
    expect(posts(fetchMock).at(-1)).toEqual({
      path: "/api/v2/plans/2/checkoff/bursts/undo",
      body: { undo_token: "ppm_undo" },
    });
    expect(posts(fetchMock)).toHaveLength(2);
  });

  it("P on a unit on no Plate says why and picks nothing", async () => {
    renderAt(core(reads(units)));
    await screen.findByRole("group", { name: /^gear\.stl/ });
    fireEvent.keyDown(box("gear.stl", 2), { key: "P" });
    expect(screen.getByText(/on no Plate of the accepted layout/)).toBeTruthy();
    expect(preview()).toBe("");
  });

  it("F fills a Part, a digit sets its count, Space ticks one and Shift picks a run", async () => {
    renderAt(core(reads(units)));
    await screen.findByRole("group", { name: /^bracket\.stl/ });
    fireEvent.keyDown(box("bracket.stl", 1), { key: "f" });
    expect(preview()).toBe("bracket.stl: +2 printed (units 2 and 3)");
    fireEvent.keyDown(box("bracket.stl", 1), { key: "0" });
    expect(preview()).toBe("bracket.stl: \u22121 printed (unit 1)");
    fireEvent.keyDown(box("bracket.stl", 1), { key: "2" });
    expect(preview()).toBe("bracket.stl: +1 printed (unit 2)");
    fireEvent.keyDown(box("bracket.stl", 1), { key: "1" });
    expect(preview()).toBe("");
    fireEvent.click(box("gear.stl", 1));
    expect(preview()).toBe("gear.stl: +1 printed (unit 1)");
    fireEvent.click(box("bracket.stl", 2));
    fireEvent.click(box("gear.stl", 2), { shiftKey: true });
    expect(preview()).toContain("bracket.stl: +2 printed (units 2 and 3)");
    expect(preview()).toContain("gear.stl: +2 printed (units 1 and 2)");
  });

  it("refuses to send a burst that prints and clears across one Part", async () => {
    renderAt(core(reads(units)));
    await screen.findByRole("group", { name: /^bracket\.stl/ });
    fireEvent.click(box("bracket.stl", 1));
    fireEvent.click(box("bracket.stl", 3));
    expect(screen.getByText(/both print and clear units of bracket\.stl/)).toBeTruthy();
    expect(screen.queryByRole("button", { name: /^Record/ })).toBeNull();
  });

  it("assembled mode marks only printed units", async () => {
    const fetchMock = core(reads(units));
    renderAt(fetchMock);
    await screen.findByRole("group", { name: /^bracket\.stl/ });
    fireEvent.click(screen.getByRole("radio", { name: "Mark assembled" }));
    expect(box("bracket.stl", 2).disabled).toBe(true);
    fireEvent.keyDown(box("bracket.stl", 1), { key: "f" });
    expect(preview()).toBe("bracket.stl: +1 assembled (unit 1)");
    fireEvent.click(screen.getByRole("button", { name: "Record 1 picked unit" }));
    await screen.findByText(/Recorded:/);
    expect(posts(fetchMock)[0]!.body.units).toEqual([{ token: token(1), assembled: true }]);
  });
});

describe("CHK-03 retry and refusals", () => {
  it("a lost send is retried with the same key, and a refusal shows its reason without Retry", async () => {
    let calls = 0;
    const fetchMock = core(
      reads(units, (path) => {
        calls += 1;
        // The first send and its two automatic resends are lost.
        if (calls <= 3) return Promise.reject(new TypeError("Failed to fetch"));
        if (path.endsWith("/undo"))
          return json({ detail: "A later change touched these units" }, 409);
        return json(RECEIPT);
      }),
    );
    renderAt(fetchMock);
    await screen.findByRole("group", { name: /^bracket\.stl/ });
    fireEvent.keyDown(box("bracket.stl", 1), { key: "f" });
    fireEvent.click(screen.getByRole("button", { name: "Record 2 picked units" }));
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("Could not record the burst.");
    fireEvent.click(within(alert).getByRole("button", { name: "Retry" }));
    await screen.findByText(/Recorded:/);
    const sent = posts(fetchMock).map((p) => p.body);
    expect(sent).toHaveLength(4);
    for (const body of sent) expect(body).toEqual(sent[0]);
    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    const refused = await screen.findByRole("alert");
    expect(refused.textContent).toContain("A later change touched these units");
    expect(within(refused).queryByRole("button", { name: "Retry" })).toBeNull();
  });
});

describe("CHK-05 offline", () => {
  it("disables recording with the reason and queues nothing", async () => {
    let down = false;
    const handler = reads(units);
    const fetchMock = vi.fn((path: string, init?: RequestInit) => {
      if (down) return Promise.reject(new TypeError("Failed to fetch"));
      return Promise.resolve(path === "/health" ? json(HEALTH) : handler(path, init));
    });
    vi.useFakeTimers({ shouldAdvanceTime: true });
    renderAt(fetchMock as unknown as ReturnType<typeof core>);
    await screen.findByRole("group", { name: /^bracket\.stl/ });
    fireEvent.keyDown(box("bracket.stl", 1), { key: "f" });
    down = true;
    await act(() => vi.advanceTimersByTimeAsync(31_000));
    await waitFor(() => expect(screen.getByText("Offline")).toBeTruthy());
    const record = screen.getByRole("button", { name: "Record 2 picked units" });
    expect(record.getAttribute("aria-disabled")).toBe("true");
    expect(screen.getAllByText(OFFLINE_REASON).length).toBeGreaterThan(0);
    fireEvent.click(record);
    expect(fetchMock.mock.calls.some(([, init]) => init?.method === "POST")).toBe(false);
  });
});
