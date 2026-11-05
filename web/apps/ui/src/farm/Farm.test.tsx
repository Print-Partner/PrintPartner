import type { FarmPrinterList, Health } from "@print-partner/rust-client";
import { act, cleanup, render, screen, within } from "@testing-library/react";
import { createMemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App, createQueryClient } from "../App";
import { routes } from "../routes";
import { sockets, type FakeSocket } from "../testSockets";
import { PRINTER_DISPLAY_STATES } from "../status/PrinterStatus";

type FarmPrinter = FarmPrinterList["printers"][number];
type View = FarmPrinter["state"];

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });

const HEARD = "2026-11-05T20:00:00Z";

const printer = (id: string, name: string, extra: Partial<FarmPrinter> = {}): FarmPrinter => ({
  id,
  name,
  model: "Voron 2.4",
  integration: { id: `int-${id}`, kind: "moonraker" },
  state: { execution: "idle", bed_clear: "clear", display: "ready" },
  reason: null,
  last_heard_at: HEARD,
  progress_percent: null,
  loaded_filaments: [],
  ...extra,
});

// Synthetic fleet: one row per reason the farm read can give, plus Bambu H2D twice.
const FLEET: FarmPrinter[] = [
  printer("voron", "Voron 2.4", {
    state: { execution: "printing", bed_clear: "needs_clearing", display: "printing" },
    progress_percent: 42,
  }),
  printer("trident", "Trident", {
    model: "Trident",
    state: { execution: "idle", bed_clear: "needs_clearing", display: "bed_to_clear" },
  }),
  printer("micron", "Micron", { model: "Micron" }),
  printer("annex", "Annex K3", {
    model: "K3",
    state: { execution: "unreachable", bed_clear: "unknown", display: "unreachable" },
    reason: "unreachable",
  }),
  printer("h2d", "Bambu H2D", {
    model: "H2D",
    integration: { id: "int-bambu", kind: "bambu" },
    state: { execution: "manual", bed_clear: "unknown", display: "manual" },
    reason: "monitoring_not_supported",
    last_heard_at: null,
  }),
  printer("h2d-2", "Bambu H2D (spare)", {
    model: "H2D",
    integration: null,
    state: { execution: "not_set_up", bed_clear: "unknown", display: "not_set_up" },
    reason: "not_set_up",
    last_heard_at: null,
  }),
];

/** The stream's view of a farm-read printer, as /ws/printers sends it. */
const streamed = (p: FarmPrinter, display: View = p.state) => ({
  id: p.id,
  name: p.name,
  state: "idle",
  display,
  ...(p.reason ? { reason: p.reason } : {}),
  ...(p.progress_percent !== null ? { progress_percent: p.progress_percent } : {}),
  filaments: [],
  ...(p.last_heard_at ? { last_heard_at: p.last_heard_at } : {}),
});

const health = (farm: "on" | "off"): Health => ({
  ok: true,
  authenticated: true,
  authentication_required: true,
  capabilities: [],
  multi_user: false,
  registration_open: false,
  modules:
    farm === "on"
      ? [{ module: "farm", state: "on" }]
      : [{ module: "farm", state: "off", reason: "turned_off" }],
});

function core({ printers = FLEET, farm = "on" as "on" | "off" } = {}) {
  return vi.fn(async (path: string) => {
    if (path === "/health") return json(health(farm));
    if (path === "/api/v2/printers") return json({ printers });
    return json({ title: "Not Found" }, 404);
  });
}

function renderAt(path: string, fetchImpl = core()) {
  vi.stubGlobal("fetch", fetchImpl);
  const router = createMemoryRouter(routes, { initialEntries: [path] });
  render(<App router={router} queryClient={createQueryClient()} />);
  return { fetchMock: fetchImpl };
}

const socket = (index = sockets.length - 1): FakeSocket => {
  const found = sockets[index];
  if (!found) throw new Error(`no socket ${index}`);
  return found;
};

function snapshot(printers = FLEET, seq = 7, stream = "s1") {
  const s = socket();
  act(() => {
    s.open();
    s.frame({ type: "snapshot", stream, seq, heartbeat_seconds: 15, printers: printers.map((p) => streamed(p)) });
  });
}

const pulse = () => screen.getByRole("region", { name: "Printer pulse" });
const ROW_IDS: Record<string, string> = Object.fromEntries(FLEET.map((p) => [p.name, p.id]));
const row = (name: string) => {
  const found = document.querySelector<HTMLTableRowElement>(`tr[data-printer="${ROW_IDS[name] ?? name}"]`);
  if (!found) throw new Error(`no row for ${name}`);
  return found;
};
const cells = (name: string) => within(row(name)).getAllByRole("cell").map((c) => c.textContent);
const printerCalls = (fetchMock: ReturnType<typeof core>) =>
  fetchMock.mock.calls.filter(([path]) => path === "/api/v2/printers").length;

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("UI3-01 every printer row shows an icon and a word", () => {
  it("renders each display state as an icon plus a non-empty word", async () => {
    const fleet = PRINTER_DISPLAY_STATES.map((display, i) =>
      printer(`p${i}`, `Synthetic ${display}`, { state: { execution: "unknown", bed_clear: "unknown", display } }),
    );
    renderAt("/farm", core({ printers: fleet }));
    await screen.findByRole("table", { name: "Printers" });
    for (const p of fleet) {
      const status = within(row(p.id)).getAllByRole("cell")[0]!;
      expect(status.querySelector("svg")).not.toBeNull();
      expect(status.textContent?.trim()).not.toBe("");
    }
  });
});

describe("UI3-02 execution and bed are separate facts", () => {
  it("shows execution and bed in their own cells beside the summary", async () => {
    renderAt("/farm");
    await screen.findByRole("table", { name: "Printers" });
    expect(cells("Trident").slice(0, 3)).toEqual(["Bed to clear", "Idle", "Needs clearing"]);
    expect(cells("Voron 2.4").slice(0, 3)).toEqual(["Printing", "Printing 42%", "Needs clearing"]);
    expect(cells("Micron").slice(0, 3)).toEqual(["Ready", "Idle", "Clear, confirmed"]);
  });
});

describe("UI3-03 the pulse line counts by state from /ws/printers", () => {
  it("counts the snapshot, follows events, and never polls for printers", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.setSystemTime(new Date("2026-11-05T21:00:00Z"));
    const { fetchMock } = renderAt("/farm");
    expect(pulse().textContent).toBe("Printer counts not loaded");
    expect(socket().url).toBe("ws://localhost:3000/ws/printers");
    snapshot();
    expect(within(pulse()).getAllByRole("listitem").map((li) => li.getAttribute("aria-label"))).toEqual([
      "1 Printing",
      "1 Bed to clear",
      "1 Ready",
      "1 Unreachable",
      "1 Not set up",
      "1 Manual",
    ]);
    await screen.findByRole("table", { name: "Printers" });

    // Trident's bed is confirmed clear: one event moves it from Bed to clear to Ready.
    act(() =>
      socket().frame({
        type: "printer",
        stream: "s1",
        seq: 8,
        printer: streamed(FLEET[1]!, { execution: "idle", bed_clear: "clear", display: "ready" }),
      }),
    );
    expect(within(pulse()).getByLabelText("2 Ready")).toBeTruthy();
    expect(within(pulse()).queryByLabelText("1 Bed to clear")).toBeNull();
    expect(cells("Trident").slice(0, 3)).toEqual(["Ready", "Idle", "Clear, confirmed"]);

    // Ten minutes of heartbeats: no further printer reads, on this screen or any other.
    for (let i = 0; i < 40; i++) {
      await act(async () => {
        await vi.advanceTimersByTimeAsync(15_000);
        socket().frame({ type: "heartbeat", stream: "s1", seq: 8, at: HEARD });
      });
    }
    expect(printerCalls(fetchMock)).toBe(1);
    // An answering printer was heard as of the last frame; an unreachable one keeps its time.
    const heard = (name: string) => row(name).querySelector("time")?.getAttribute("dateTime");
    expect(Date.parse(heard("Micron")!)).toBeGreaterThan(Date.parse(HEARD) + 590_000);
    expect(heard("Annex K3")).toBe("2026-11-05T20:00:00.000Z");
    expect(fetchMock.mock.calls.every(([path]) => path === "/health" || path === "/api/v2/printers")).toBe(true);
    expect(sockets).toHaveLength(1);
  });

  it("is on every screen, with no false zeros", async () => {
    const { fetchMock } = renderAt("/builds");
    snapshot([]);
    expect(pulse().textContent).toBe("No printers");
    expect(printerCalls(fetchMock)).toBe(0);
  });

  it("moves last heard for an answering printer in an unknown state, not for one awaiting status", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.setSystemTime(new Date("2026-11-05T21:00:00Z"));
    const unknown = { execution: "unknown", bed_clear: "unknown", display: "unknown" } as const;
    const fleet = [
      printer("odd", "Odd state", { state: unknown }),
      printer("wait", "Awaiting", { state: unknown, reason: "awaiting_status" }),
    ];
    renderAt("/farm", core({ printers: fleet }));
    snapshot(fleet);
    await screen.findByRole("table", { name: "Printers" });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(60_000);
      socket().frame({ type: "heartbeat", stream: "s1", seq: 7, at: HEARD });
    });
    const heard = (id: string) => row(id).querySelector("time")?.getAttribute("dateTime");
    expect(Date.parse(heard("odd")!)).toBeGreaterThan(Date.parse(HEARD) + 50_000);
    expect(heard("wait")).toBe("2026-11-05T20:00:00.000Z");
  });

  it("reads details once for a printer added after the farm read", async () => {
    const added = printer("k1", "Moonraker K1", {
      model: "K1",
      state: { execution: "manual", bed_clear: "unknown", display: "manual" },
      reason: "monitoring_not_supported",
      last_heard_at: null,
    });
    let fleet = FLEET;
    const fetchMock = vi.fn(async (path: string) => {
      if (path === "/health") return json(health("on"));
      if (path === "/api/v2/printers") return json({ printers: fleet });
      return json({ title: "Not Found" }, 404);
    });
    renderAt("/farm", fetchMock);
    snapshot();
    await screen.findByRole("table", { name: "Printers" });
    expect(printerCalls(fetchMock)).toBe(1);
    fleet = [...FLEET, added];
    act(() => socket().frame({ type: "printer", stream: "s1", seq: 8, printer: streamed(added) }));
    await within(row("k1")).findByText("K1");
    expect(row("k1").textContent).toContain("cannot watch this Moonraker host");
    expect(printerCalls(fetchMock)).toBe(2);
    act(() =>
      socket().frame({
        type: "printer",
        stream: "s1",
        seq: 9,
        printer: streamed(FLEET[2]!, { execution: "idle", bed_clear: "needs_clearing", display: "bed_to_clear" }),
      }),
    );
    expect(printerCalls(fetchMock)).toBe(2);
  });

  it("drops a printer the stream removes", async () => {
    renderAt("/farm");
    snapshot();
    await screen.findByRole("table", { name: "Printers" });
    act(() => socket().frame({ type: "removed", stream: "s1", seq: 8, id: "micron" }));
    expect(document.querySelector('tr[data-printer="micron"]')).toBeNull();
    expect(within(pulse()).queryByLabelText("1 Ready")).toBeNull();
  });
});

describe("UI3-04 with the stream down the last-known state shows its time", () => {
  it("keeps the last states with their time and resumes from the last event", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.setSystemTime(new Date("2026-11-05T21:30:00Z"));
    renderAt("/farm");
    snapshot();
    await screen.findByRole("table", { name: "Printers" });
    act(() => socket().close());

    const notice = screen.getByText(/Live printer updates are down/).closest<HTMLElement>("[role=status]")!;
    expect(notice.textContent).toContain("Live printer updates are down, so these are the last-known states.");
    expect(notice.querySelector("time")?.getAttribute("dateTime")).toBe("2026-11-05T21:30:00.000Z");
    expect(pulse().querySelector("time")?.getAttribute("dateTime")).toBe("2026-11-05T21:30:00.000Z");
    expect(pulse().textContent).toContain("Live updates down. Last known");
    expect(within(pulse()).getByLabelText("1 Bed to clear")).toBeTruthy();
    expect(cells("Trident").slice(0, 3)).toEqual(["Bed to clear", "Idle", "Needs clearing"]);
    expect(row("Annex K3").querySelector("time")?.getAttribute("dateTime")).toBe("2026-11-05T20:00:00.000Z");

    await act(() => vi.advanceTimersByTimeAsync(1_000));
    expect(sockets).toHaveLength(2);
    expect(socket().url).toBe("ws://localhost:3000/ws/printers?stream=s1&since=7");
    // A resume that missed nothing gets no frame before the next heartbeat; open is current.
    act(() => socket().open());
    expect(screen.queryByText(/Live printer updates are down/)).toBeNull();
    expect(pulse().textContent).not.toContain("Live updates down");
    act(() => socket().frame({ type: "heartbeat", stream: "s1", seq: 7, at: HEARD }));
    expect(screen.queryByText(/Live printer updates are down/)).toBeNull();
  });

  it("announces that the pulse went stale", async () => {
    renderAt("/builds");
    snapshot();
    act(() => socket().close());
    const live = pulse().closest("[aria-live]");
    expect(live?.getAttribute("aria-live")).toBe("polite");
    expect(live?.textContent).toContain("Live updates down. Last known");
  });

  it("retries a socket that never opens", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    renderAt("/builds");
    socket().close = () => {};
    await act(() => vi.advanceTimersByTimeAsync(36_000));
    expect(pulse().textContent).toBe("Printer counts not loaded: live updates are down");
    await act(() => vi.advanceTimersByTimeAsync(1_000));
    expect(sockets).toHaveLength(2);
  });

  it("counts a quiet socket as down after two missed heartbeats, without waiting for its close", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    renderAt("/farm");
    snapshot();
    await screen.findByRole("table", { name: "Printers" });
    // An unreachable peer never completes the close handshake, so onclose never fires.
    const stalled = socket();
    stalled.close = () => {};
    await act(() => vi.advanceTimersByTimeAsync(34_000));
    expect(screen.queryByText(/Live printer updates are down/)).toBeNull();
    await act(() => vi.advanceTimersByTimeAsync(2_000));
    expect(screen.getByText(/Live printer updates are down/)).toBeTruthy();
    await act(() => vi.advanceTimersByTimeAsync(1_000));
    expect(sockets).toHaveLength(2);
    expect(socket().url).toContain("since=7");
    // A late frame from the dropped socket changes nothing.
    act(() => stalled.frame({ type: "removed", stream: "s1", seq: 8, id: "micron" }));
    expect(document.querySelector('tr[data-printer="micron"]')).not.toBeNull();
  });

  it("drops the socket on a gap in seq or a frame outside the contract", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    renderAt("/farm");
    snapshot();
    act(() => socket().frame({ type: "printer", stream: "s1", seq: 9, printer: streamed(FLEET[2]!) }));
    expect(socket().readyState).toBe(3);
    await act(() => vi.advanceTimersByTimeAsync(1_000));
    expect(socket().url).toContain("since=7");
    act(() => socket().frame({ type: "printer", stream: "s1", seq: 8, printer: { id: "x" } }));
    expect(socket().readyState).toBe(3);
  });
});

describe("UI3-05 Bambu H2D reads Manual or Not set up with a reason", () => {
  it("names why each H2D is not watched", async () => {
    renderAt("/farm");
    await screen.findByRole("table", { name: "Printers" });
    expect(cells("Bambu H2D")[0]).toBe(
      "ManualPrint Partner cannot watch Bambu printers yet. Check off its prints by hand.",
    );
    expect(cells("Bambu H2D (spare)")[0]).toBe(
      "Not set upNo printer integration is set up for it, so check off its prints by hand.",
    );
    expect(cells("Bambu H2D")[3]).toBe("Never");
  });

  it("says Never once the stream stops hearing a printer, not the page-load time", async () => {
    renderAt("/farm");
    snapshot();
    await screen.findByRole("table", { name: "Printers" });
    expect(row("Annex K3").querySelector("time")).not.toBeNull();
    const manual = { ...FLEET[3]!, reason: "monitoring_disabled" as const, last_heard_at: null };
    act(() =>
      socket().frame({
        type: "printer",
        stream: "s1",
        seq: 8,
        printer: streamed(manual, { execution: "manual", bed_clear: "unknown", display: "manual" }),
      }),
    );
    expect(cells("Annex K3")[3]).toBe("Never");
  });
});

describe("Farm screen states", () => {
  it("says the Farm module is off when the core says so", async () => {
    renderAt("/farm", core({ farm: "off" }));
    expect(await screen.findByText("The Farm module is off.")).toBeTruthy();
  });

  it("shows the streamed printers when the farm read fails", async () => {
    const failing = vi.fn(async (path: string) =>
      path === "/health" ? json(health("on")) : json({ title: "Internal Server Error" }, 500),
    );
    vi.useFakeTimers({ shouldAdvanceTime: true });
    renderAt("/farm", failing);
    snapshot();
    expect(await screen.findByRole("table", { name: "Printers" })).toBeTruthy();
    await act(async () => {
      for (let i = 0; i < 12; i++) {
        await vi.advanceTimersByTimeAsync(1_000);
        socket().frame({ type: "heartbeat", stream: "s1", seq: 7, at: HEARD });
      }
    });
    expect(cells("Trident").slice(0, 3)).toEqual(["Bed to clear", "Idle", "Needs clearing"]);
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("States are live from the printer stream.");
    act(() => socket().close());
    expect(alert.textContent).toContain("States are the last the printer stream sent.");
    expect(alert.textContent).not.toContain("live");
  });

  it("shows the empty state with no printers", async () => {
    renderAt("/farm", core({ printers: [] }));
    expect(await screen.findByText(/No printers yet/)).toBeTruthy();
  });
});
