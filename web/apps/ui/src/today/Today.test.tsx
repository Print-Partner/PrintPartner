import type {
  CheckoffResultList,
  FarmPrinterList,
  Health,
  TaskFeed,
} from "@print-partner/rust-client";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { createMemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App, createQueryClient } from "../App";
import { routes } from "../routes";

type Result = CheckoffResultList["results"][number];
type Printer = FarmPrinterList["printers"][number];
type Task = TaskFeed["tasks"][number];

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });

const result = (
  id: string,
  printerId: string,
  units: [number, number, string | null][],
  extra: Partial<Result> = {},
): Result => ({
  id,
  profile_id: 22,
  printer_id: printerId,
  integration_id: "sim-moonraker",
  filename: `${id}.gcode`,
  state: "awaiting_verify",
  host_outcome: "success",
  created_at: "2026-11-05T18:00:00.000Z",
  completed_at: "2026-11-05T19:00:00.000Z",
  applied_at: null,
  unit_count: units.length,
  units: units.map(([partId, unitIndex, partFilename]) => ({
    part_id: partId,
    unit_index: unitIndex,
    part_filename: partFilename,
  })),
  resolved_unit_count: 0,
  resolved_units: [],
  ...extra,
});

const printer = (
  id: string,
  name: string,
  execution: Printer["state"]["execution"] = "idle",
): Printer => ({
  id,
  name,
  model: "Voron 2.4",
  integration: { id: "sim-moonraker", kind: "moonraker" },
  state: {
    execution,
    bed_clear: "needs_clearing",
    display: execution === "idle" ? "bed_to_clear" : "unreachable",
  },
  reason: execution === "idle" ? null : "unreachable",
  last_heard_at: null,
  progress_percent: null,
  loaded_filaments: [],
});

const task = (planId: number, buildName: string, label: string): Task => ({
  plan_id: planId,
  build_name: buildName,
  attention: true,
  due_date: null,
  rank_reason: `${buildName} is due first`,
  next_action: {
    kind: "verify_prints",
    label,
    reason: "Prints are waiting for a check",
    stage_id: "checkoff",
    target: { plan_id: planId, draft_id: null, tab: "checkoff" },
    draft_id: null,
    unit_count: 4,
    item_count: null,
    issue_count: null,
  },
});

const health: Health = {
  ok: true,
  authenticated: true,
  authentication_required: true,
  capabilities: [],
  multi_user: false,
  registration_open: false,
  modules: [{ module: "farm", state: "on" }],
};

const BEDS = (): Result[] => [
  result("link-voron", "voron", [
    [1, 1, "bracket.stl"],
    [1, 2, "bracket.stl"],
    [3, 0, "gear.stl"],
  ]),
  result("link-trident", "trident", [[2, 0, null]], { created_at: "2026-11-05T18:30:00.000Z" }),
];

/**
 * A Rust core that applies verify-batch and undo like the real routes: a repeated
 * idempotency key returns the first receipt and writes nothing.
 */
function fakeCore({
  results = BEDS(),
  printers = [printer("voron", "Voron 2.4"), printer("trident", "Trident")],
  loseFirstAnswers = 0,
  loseUndoAnswers = 0,
} = {}) {
  const state = {
    results,
    printers,
    marked: 0,
    receipts: new Map<string, unknown>(),
    undone: new Set<string>(),
  };
  const before = new Map<string, Result>();
  const lose = { confirm: loseFirstAnswers, undo: loseUndoAnswers };
  const posts: { path: string; body: Record<string, unknown> }[] = [];
  const fetchMock = vi.fn(async (path: string, init?: RequestInit) => {
    if (init?.method === "POST") {
      const body = JSON.parse(String(init.body)) as Record<string, unknown>;
      posts.push({ path, body });
      const answer = post(path, body);
      const kind = path.endsWith("/undo") ? "undo" : "confirm";
      if (lose[kind] > 0) {
        lose[kind] -= 1;
        throw new TypeError("Failed to fetch");
      }
      return answer;
    }
    if (path === "/health") return json(health);
    if (path === "/api/v2/printer-checkoff") return json({ results: state.results });
    if (path === "/api/v2/printers") return json({ printers: state.printers });
    if (path === "/api/v2/tasks")
      return json({
        tasks: [task(22, "Build 22", "Verify prints"), task(19, "Build 19", "Prepare production")],
        unavailable: [],
      });
    return json({ title: "Not Found" }, 404);
  });
  function post(path: string, body: Record<string, unknown>) {
    if (path === "/api/v2/printer-checkoff/verify-batch") {
      const key = body.idempotency_key as string;
      const seen = state.receipts.get(key);
      if (seen) return json(seen);
      const beds = body.beds as {
        link_id: string;
        decisions: { part_id: number; unit_index: number }[];
      }[];
      for (const bed of beds) {
        const index = state.results.findIndex((r) => r.id === bed.link_id);
        const link = state.results[index]!;
        before.set(key, link);
        const resolved = [
          ...link.resolved_units,
          ...bed.decisions.map((d) => ({ ...d, result: "confirmed" as const })),
        ];
        state.results[index] = {
          ...link,
          resolved_units: resolved,
          resolved_unit_count: resolved.length,
          state: resolved.length === link.units.length ? "verified" : link.state,
        };
        state.marked += bed.decisions.length;
      }
      const units = beds.reduce((n, b) => n + b.decisions.length, 0);
      const receipt = {
        idempotency_key: key,
        undo_token: `undo-${key}`,
        units_confirmed: units,
        units_rejected: 0,
        beds: [{ units_confirmed: units, units_rejected: 0 }],
      };
      state.receipts.set(key, receipt);
      return json(receipt);
    }
    const token = body.undo_token as string;
    const key = token.replace(/^undo-/, "");
    if (state.undone.has(token))
      return json({ title: "Conflict", detail: "This Checkoff burst was already undone" }, 409);
    const prior = before.get(key)!;
    state.results = state.results.map((r) => (r.id === prior.id ? prior : r));
    state.marked -= prior.units.length - prior.resolved_units.length;
    state.undone.add(token);
    return json({ undo_token: token, undone_at: "2026-11-05T21:00:00.000Z" });
  }
  return { fetchMock, posts, state };
}

function renderToday(core: ReturnType<typeof fakeCore>) {
  vi.stubGlobal("fetch", core.fetchMock);
  const router = createMemoryRouter(routes, { initialEntries: ["/today"] });
  render(<App router={router} queryClient={createQueryClient()} />);
}

const hero = () => screen.getByRole("region", { name: /and confirm what came off/ });
const bed = (name: string) => within(hero()).getByRole("heading", { name }).closest("li")!;
const status = () => screen.getByRole("status", { name: "Checkoff tally" });

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("UI4-01 the hero task shows the reason and the diff inline", () => {
  it("names the beds, the printers they free, and what each confirm marks", async () => {
    renderToday(fakeCore());
    expect(
      await screen.findByRole("heading", {
        level: 2,
        name: "Clear 2 beds and confirm what came off",
      }),
    ).toBeTruthy();
    expect(await within(hero()).findByText("Frees Voron 2.4 and Trident.")).toBeTruthy();
    const diff = (name: string) =>
      within(bed(name)).getByRole("list", { name: `What confirming ${name} marks` });
    expect(
      within(diff("Voron 2.4"))
        .getAllByRole("listitem")
        .map((li) => li.textContent),
    ).toEqual(["bracket.stl: +2 printed (units 2 and 3)", "gear.stl: +1 printed (unit 1)"]);
    expect(
      within(diff("Trident"))
        .getAllByRole("listitem")
        .map((li) => li.textContent),
    ).toEqual(["Part 2: +1 printed (unit 1)"]);
    const ranked = await screen.findByRole("list", { name: "Ranked tasks" });
    expect(within(ranked).getAllByRole("listitem")).toHaveLength(2);
  });

  it("shows the top-ranked task with its reason when no bed waits", async () => {
    renderToday(fakeCore({ results: [] }));
    expect(await screen.findByRole("heading", { level: 2, name: "Verify prints" })).toBeTruthy();
    expect(screen.getByText(": Prints are waiting for a check")).toBeTruthy();
    expect(screen.getByText("4 units")).toBeTruthy();
    const ranked = await screen.findByRole("list", { name: "Ranked tasks" });
    expect(
      within(ranked)
        .getAllByRole("listitem")
        .map((li) => li.textContent),
    ).toEqual(["Build 19 Prepare productionNeeds youBuild 19 is due first"]);
  });
});

describe("UI4-02 one bed is one verify-batch request, with undo", () => {
  it("confirms one bed in one request, then undo restores it", async () => {
    const core = fakeCore();
    renderToday(core);
    fireEvent.click(await screen.findByRole("button", { name: "Confirm Voron 2.4 bed" }));
    expect(await screen.findByText("Confirmed 3 units from Voron 2.4.")).toBeTruthy();
    expect(core.posts).toHaveLength(1);
    expect(core.posts[0]!.path).toBe("/api/v2/printer-checkoff/verify-batch");
    expect(core.posts[0]!.body).toEqual({
      idempotency_key: expect.any(String),
      beds: [
        {
          link_id: "link-voron",
          decisions: [
            { part_id: 1, unit_index: 1, result: "confirmed" },
            { part_id: 1, unit_index: 2, result: "confirmed" },
            { part_id: 3, unit_index: 0, result: "confirmed" },
          ],
        },
      ],
    });
    expect(
      await screen.findByRole("heading", {
        level: 2,
        name: "Clear 1 bed and confirm what came off",
      }),
    ).toBeTruthy();
    expect(core.state.marked).toBe(3);

    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    expect(
      await screen.findByText("Undid the Voron 2.4 confirmation. 3 units are waiting again."),
    ).toBeTruthy();
    expect(core.posts.map((p) => p.path)).toEqual([
      "/api/v2/printer-checkoff/verify-batch",
      "/api/v2/printer-checkoff/verify-batch/undo",
    ]);
    expect(core.posts[1]!.body).toEqual({
      undo_token: `undo-${String(core.posts[0]!.body.idempotency_key)}`,
    });
    expect(
      await screen.findByRole("heading", {
        level: 2,
        name: "Clear 2 beds and confirm what came off",
      }),
    ).toBeTruthy();
    expect(core.state.marked).toBe(0);
    expect(screen.queryByRole("button", { name: "Undo" })).toBeNull();
  });
});

describe("UI4-03 a retried confirm does not double count", () => {
  it("resends a lost answer with the same key and counts the units once", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const core = fakeCore({ loseFirstAnswers: 1 });
    renderToday(core);
    fireEvent.click(await screen.findByRole("button", { name: "Confirm Voron 2.4 bed" }));
    await act(() => vi.advanceTimersByTimeAsync(1_500));
    expect(await screen.findByText("Confirmed 3 units from Voron 2.4.")).toBeTruthy();
    expect(core.posts).toHaveLength(2);
    expect(core.posts[1]!.body).toEqual(core.posts[0]!.body);
    expect(core.state.marked).toBe(3);
  });

  it("offers Retry after the resends run out, still with the same key", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const core = fakeCore({ loseFirstAnswers: 3 });
    renderToday(core);
    fireEvent.click(await screen.findByRole("button", { name: "Confirm Voron 2.4 bed" }));
    await act(() => vi.advanceTimersByTimeAsync(4_000));
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("Could not confirm the bed.");
    fireEvent.click(within(alert).getByRole("button", { name: "Retry" }));
    expect(await screen.findByText("Confirmed 3 units from Voron 2.4.")).toBeTruthy();
    expect(new Set(core.posts.map((p) => p.body.idempotency_key)).size).toBe(1);
    expect(core.posts).toHaveLength(4);
    expect(core.state.marked).toBe(3);
  });
});

describe("an uncertain write is reconciled, not repeated", () => {
  it("shows the undo as done when its answer was lost and the resend finds it undone", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const core = fakeCore({ loseUndoAnswers: 1 });
    renderToday(core);
    fireEvent.click(await screen.findByRole("button", { name: "Confirm Voron 2.4 bed" }));
    fireEvent.click(await screen.findByRole("button", { name: "Undo" }));
    await act(() => vi.advanceTimersByTimeAsync(1_500));
    expect(
      await screen.findByText("Undid the Voron 2.4 confirmation. 3 units are waiting again."),
    ).toBeTruthy();
    expect(core.posts.filter((p) => p.path.endsWith("/undo"))).toHaveLength(2);
    expect(await screen.findByRole("button", { name: "Confirm Voron 2.4 bed" })).toBeTruthy();
    expect(screen.queryByRole("alert")).toBeNull();
    expect(core.state.marked).toBe(0);
  });

  it("reuses the unanswered key when the same bed is confirmed again", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const core = fakeCore({ loseFirstAnswers: 3 });
    renderToday(core);
    fireEvent.click(await screen.findByRole("button", { name: "Confirm Voron 2.4 bed" }));
    await act(() => vi.advanceTimersByTimeAsync(4_000));
    await screen.findByRole("alert");
    fireEvent.click(screen.getByRole("button", { name: "Confirm Voron 2.4 bed" }));
    expect(await screen.findByText("Confirmed 3 units from Voron 2.4.")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Undo" })).toBeTruthy();
    expect(new Set(core.posts.map((p) => p.body.idempotency_key)).size).toBe(1);
    expect(core.state.marked).toBe(3);
  });
});

describe("UI4-04 with printer links down Today offers manual checkoff and loses no job", () => {
  it("keeps every bed confirmable by hand and lists prints a down printer was watching", async () => {
    const watching = result("link-annex", "annex", [[4, 0, "clip.stl"]], {
      state: "watching",
      host_outcome: null,
      completed_at: null,
    });
    const core = fakeCore({
      results: [...BEDS(), watching],
      printers: [
        printer("voron", "Voron 2.4", "unreachable"),
        printer("trident", "Trident", "unreachable"),
        printer("annex", "Annex K3", "unreachable"),
      ],
    });
    renderToday(core);
    expect(
      await screen.findByRole("heading", {
        level: 2,
        name: "Clear 2 beds and confirm what came off",
      }),
    ).toBeTruthy();
    await screen.findByRole("heading", { level: 3, name: "Voron 2.4" });
    expect(
      within(bed("Voron 2.4")).getByText(/Printer link down, so confirm by hand what came off/),
    ).toBeTruthy();
    const unseen = screen.getByRole("region", { name: "Prints the app cannot see finish" });
    expect(unseen.textContent).toContain("Annex K3: link-annex.gcode. Its printer link is down");
    expect(
      within(unseen).getByRole("link", { name: "Build's Checkoff tab" }).getAttribute("href"),
    ).toBe("/builds/22/checkoff");
    fireEvent.click(screen.getByRole("button", { name: "Confirm Trident bed" }));
    expect(await screen.findByText("Confirmed 1 unit from Trident.")).toBeTruthy();
    expect(core.state.marked).toBe(1);
  });

  it("notices a link that drops while Today is open", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const watching = result("link-annex", "annex", [[4, 0, "clip.stl"]], {
      state: "watching",
      host_outcome: null,
      completed_at: null,
    });
    const core = fakeCore({
      results: [...BEDS(), watching],
      printers: [
        printer("voron", "Voron 2.4"),
        printer("trident", "Trident"),
        printer("annex", "Annex K3"),
      ],
    });
    renderToday(core);
    await screen.findByRole("heading", { level: 3, name: "Voron 2.4" });
    expect(screen.queryByRole("region", { name: "Prints the app cannot see finish" })).toBeNull();
    core.state.printers = [
      printer("voron", "Voron 2.4"),
      printer("trident", "Trident"),
      printer("annex", "Annex K3", "unreachable"),
    ];
    await act(() => vi.advanceTimersByTimeAsync(30_000));
    expect(
      (await screen.findByRole("region", { name: "Prints the app cannot see finish" })).textContent,
    ).toContain("Annex K3: link-annex.gcode");
  });

  it("does not claim a link is up when the farm read fails", async () => {
    const core = fakeCore();
    core.fetchMock.mockImplementation(async (path: string) =>
      path === "/api/v2/printers"
        ? json({}, 500)
        : path === "/health"
          ? json(health)
          : json({ results: BEDS() }),
    );
    renderToday(core);
    expect(await screen.findByRole("heading", { level: 3, name: "voron" })).toBeTruthy();
    expect(within(bed("voron")).getByText(/Printer link down/)).toBeTruthy();
  });
});

describe("UI4-05 tallies are announced in a live region", () => {
  it("puts the tally in the status region that was there before it", async () => {
    renderToday(fakeCore());
    await screen.findByRole("button", { name: "Confirm Voron 2.4 bed" });
    const region = status();
    expect(region.textContent).toBe("");
    fireEvent.click(screen.getByRole("button", { name: "Confirm Voron 2.4 bed" }));
    await screen.findByText("Confirmed 3 units from Voron 2.4.");
    expect(status()).toBe(region);
    expect(region.textContent).toContain("Confirmed 3 units from Voron 2.4.");
  });
});
