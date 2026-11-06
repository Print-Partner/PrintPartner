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
  const before = new Map<string, { links: Result[]; units: number }>();
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
      const units = beds.reduce((n, b) => n + b.decisions.length, 0);
      before.set(key, {
        links: beds.map((b) => state.results.find((r) => r.id === b.link_id)!),
        units,
      });
      for (const bed of beds) {
        const index = state.results.findIndex((r) => r.id === bed.link_id);
        const link = state.results[index]!;
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
      const receipt = {
        idempotency_key: key,
        undo_token: `undo-${key}`,
        units_confirmed: units,
        units_rejected: 0,
        beds: beds.map((b) => ({ units_confirmed: b.decisions.length, units_rejected: 0 })),
      };
      state.receipts.set(key, receipt);
      return json(receipt);
    }
    const token = body.undo_token as string;
    const key = token.replace(/^undo-/, "");
    if (state.undone.has(token))
      return json({ title: "Conflict", detail: "This Checkoff burst was already undone" }, 409);
    const prior = before.get(key)!;
    state.results = state.results.map((r) => prior.links.find((l) => l.id === r.id) ?? r);
    state.marked -= prior.units;
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

describe("ranked tasks stay current", () => {
  it("rereads the task feed every 30 s and keeps the last list when a reread fails", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const core = fakeCore();
    const feed = { tasks: [task(22, "Build 22", "Verify prints")], unavailable: [] as never[] };
    let failTasks = false;
    const base = core.fetchMock.getMockImplementation()!;
    core.fetchMock.mockImplementation(async (path: string, init?: RequestInit) => {
      if (path !== "/api/v2/tasks") return base(path, init);
      return failTasks ? json({}, 500) : json(feed);
    });
    renderToday(core);
    const ranked = await screen.findByRole("list", { name: "Ranked tasks" });
    expect(within(ranked).getAllByRole("listitem")).toHaveLength(1);
    feed.tasks.push(task(19, "Build 19", "Prepare production"));
    await act(() => vi.advanceTimersByTimeAsync(30_000));
    expect(
      within(screen.getByRole("list", { name: "Ranked tasks" })).getAllByRole("listitem"),
    ).toHaveLength(2);
    failTasks = true;
    await act(() => vi.advanceTimersByTimeAsync(40_000));
    expect((await screen.findByText(/Could not refresh ranked tasks/)).textContent).toContain(
      "Showing the last list.",
    );
    expect(
      within(screen.getByRole("list", { name: "Ranked tasks" })).getAllByRole("listitem"),
    ).toHaveLength(2);
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

describe("a burst names the lower units Progress needs first", () => {
  it("names a lower unit on another bed and picks it on request, sending one request", async () => {
    const core = fakeCore({
      results: [
        result("link-voron", "voron", [[7, 1, "hinge.stl"]], { created_at: "2026-11-05T18:00:00.000Z" }),
        result("link-trident", "trident", [[7, 0, "hinge.stl"]], { created_at: "2026-11-05T18:30:00.000Z" }),
      ],
    });
    renderToday(core);
    await screen.findByRole("heading", { level: 3, name: "Voron 2.4" });
    expect(
      within(bed("Voron 2.4")).getByText(
        "Progress fills from the left, so confirm Trident: hinge.stl unit 1 first, or pick those units with this bed below.",
      ),
    ).toBeTruthy();
    expect(within(bed("Trident")).queryByText(/Progress fills from the left/)).toBeNull();
    fireEvent.click(within(bed("Voron 2.4")).getByRole("checkbox", { name: "hinge.stl unit 2" }));
    expect(screen.getByText(/the core refuses this until these lower units are picked too: Trident: hinge.stl unit 1\./)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Pick them too" }));
    expect(screen.queryByText(/lower units are picked too/)).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Confirm 2 picked units" }));
    expect(await screen.findByText(/^Confirmed 2 units from/)).toBeTruthy();
    expect(core.posts).toHaveLength(1);
    expect((core.posts[0]!.body.beds as { link_id: string }[]).map((b) => b.link_id)).toEqual([
      "link-voron",
      "link-trident",
    ]);
  });
});

describe("a burst names lower units that are still printing", () => {
  it("says a lower unit on a running print blocks the confirm", async () => {
    const core = fakeCore({
      results: [
        result("link-voron", "voron", [[7, 1, "hinge.stl"]]),
        result("link-annex", "annex", [[7, 0, "hinge.stl"]], {
          state: "watching",
          host_outcome: null,
          completed_at: null,
        }),
      ],
      printers: [printer("voron", "Voron 2.4"), printer("annex", "Annex K3")],
    });
    renderToday(core);
    await screen.findByRole("heading", { level: 3, name: "Voron 2.4" });
    const note =
      "Lower units of these Parts are still printing: Annex K3: hinge.stl unit 1. Progress fills from the left, so the core refuses this until they come off and are confirmed.";
    expect(within(bed("Voron 2.4")).getByText(note)).toBeTruthy();
    fireEvent.click(within(bed("Voron 2.4")).getByRole("checkbox", { name: "hinge.stl unit 2" }));
    expect(within(screen.getByText(/^Will mark 1 unit/).closest("div")!).getByText(note)).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Pick them too" })).toBeNull();
  });
});

describe("a burst names lower units no bed can confirm", () => {
  it("names a failed print's lower unit, and not one confirmed on another result", async () => {
    const core = fakeCore({
      results: [
        result("link-voron", "voron", [[7, 2, "hinge.stl"]]),
        result("link-annex", "annex", [[7, 0, "hinge.stl"]], { state: "host_failed", host_outcome: "failed" }),
        result("link-old", "trident", [[7, 1, "hinge.stl"]], {
          state: "awaiting_verify",
          resolved_units: [{ part_id: 7, unit_index: 1, result: "rejected" }],
          resolved_unit_count: 1,
        }),
        result("link-redo", "trident", [[7, 1, "hinge.stl"]], {
          state: "verified",
          resolved_units: [{ part_id: 7, unit_index: 1, result: "confirmed" }],
          resolved_unit_count: 1,
        }),
      ],
      printers: [printer("voron", "Voron 2.4"), printer("annex", "Annex K3"), printer("trident", "Trident")],
    });
    renderToday(core);
    await screen.findByRole("heading", { level: 3, name: "Voron 2.4" });
    expect(
      within(bed("Voron 2.4")).getByText(
        "Lower units of these Parts are not confirmed on any print Today shows: Annex K3: hinge.stl unit 1. Progress fills from the left, so the core refuses this unless they were completed another way.",
      ),
    ).toBeTruthy();
  });
});

describe("a burst stays inside the verify-batch contract", () => {
  it("says to split a burst of more than 50 beds instead of offering Confirm", async () => {
    const results = Array.from({ length: 51 }, (_, i) =>
      result(`link-${i}`, "voron", [[9, i, "clip.stl"]]),
    );
    const core = fakeCore({ results });
    renderToday(core);
    const first = await screen.findByRole("checkbox", { name: "clip.stl unit 1" });
    first.focus();
    fireEvent.keyDown(first, { key: "f" });
    expect(
      screen.getByText(
        "One burst holds at most 50 beds, and 51 are picked. Untick some beds and confirm them in a second burst.",
      ),
    ).toBeTruthy();
    expect(screen.queryByRole("button", { name: /^Confirm \d+ picked units$/ })).toBeNull();
    expect(core.posts).toHaveLength(0);
  });
});

describe("the newest failed write is the one shown", () => {
  it("shows a failed Undo even after a refused confirm of another bed", async () => {
    const core = fakeCore();
    const real = core.fetchMock.getMockImplementation()!;
    renderToday(core);
    fireEvent.click(await screen.findByRole("button", { name: "Confirm Voron 2.4 bed" }));
    expect(await screen.findByText("Confirmed 3 units from Voron 2.4.")).toBeTruthy();
    core.fetchMock.mockImplementation(async (path: string, init?: RequestInit) =>
      init?.method === "POST"
        ? path.endsWith("/undo")
          ? json({ title: "Conflict", detail: "A newer change to this Checkoff link exists" }, 409)
          : json(
              {
                title: "Bad Request",
                detail: "beds[0]: Confirm must include lower incomplete units first",
              },
              400,
            )
        : real(path, init),
    );
    fireEvent.click(await screen.findByRole("button", { name: "Confirm Trident bed" }));
    expect(await screen.findByText("Could not confirm what came off.")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    expect(await screen.findByText("Could not undo the confirmation.")).toBeTruthy();
    expect(screen.queryByText("Could not confirm what came off.")).toBeNull();
    expect(screen.getByText(/A newer change to this Checkoff link exists/)).toBeTruthy();
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
    expect(alert.textContent).toContain("Could not confirm what came off.");
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
    expect(unseen.textContent).toContain(
      "Annex K3: link-annex.gcode. Printer link down, so check it off by hand",
    );
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

  it("stops claiming a link is up when a farm reread fails", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const core = fakeCore();
    const real = core.fetchMock.getMockImplementation()!;
    renderToday(core);
    await screen.findByRole("heading", { level: 3, name: "Voron 2.4" });
    expect(within(bed("Voron 2.4")).queryByText(/Printer link down/)).toBeNull();
    core.fetchMock.mockImplementation(async (path: string, init?: RequestInit) =>
      path === "/api/v2/printers" ? json({}, 500) : real(path, init),
    );
    // The reread at 30 s fails, then react-query's three backed-off retries (1 + 2 + 4 s).
    await act(() => vi.advanceTimersByTimeAsync(38_000));
    expect(
      await within(bed("Voron 2.4")).findByText(/Printer links could not be read/),
    ).toBeTruthy();
    expect(within(bed("Voron 2.4")).queryByText(/Printer link down/)).toBeNull();
  });

  it("does not claim a link is up while the farm read is pending or after it fails", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
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
    expect(within(bed("voron")).getByText(/Printer links not read yet/)).toBeTruthy();
    // The read fails, then react-query's three backed-off retries (1 + 2 + 4 s).
    await act(() => vi.advanceTimersByTimeAsync(8_000));
    expect(await within(bed("voron")).findByText(/Printer links could not be read/)).toBeTruthy();
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

const SHARED = (): Result[] => [
  result("link-voron", "voron", [
    [1, 1, "bracket.stl"],
    [1, 2, "bracket.stl"],
    [3, 0, "gear.stl"],
  ]),
  result(
    "link-trident",
    "trident",
    [
      [3, 1, "gear.stl"],
      [2, 0, null],
    ],
    { created_at: "2026-11-05T18:30:00.000Z" },
  ),
];

const unit = (name: string) => screen.getByRole("checkbox", { name });
const preview = () => screen.getByText(/^(Will mark|Nothing picked)/).closest("div")!;
const previewLines = () =>
  within(preview())
    .queryAllByRole("listitem")
    .map((li) => li.textContent);

describe("UI5-01 P, F, digits, Space and Shift pick units and preview them before commit", () => {
  it("P picks the focused unit's plate and previews it without sending anything", async () => {
    const core = fakeCore({ results: SHARED() });
    renderToday(core);
    await screen.findByRole("heading", { level: 3, name: "Voron 2.4" });
    expect(preview().textContent).toContain("Nothing picked.");
    const gear = within(bed("Voron 2.4")).getByRole("checkbox", { name: "gear.stl unit 1" });
    gear.focus();
    fireEvent.keyDown(gear, { key: "p" });
    expect(preview().textContent).toContain("Will mark 3 units on 1 bed (Voron 2.4):");
    expect(previewLines()).toEqual([
      "bracket.stl: +2 printed (units 2 and 3)",
      "gear.stl: +1 printed (unit 1)",
    ]);
    expect(core.posts).toHaveLength(0);
  });

  it("F fills the focused unit's Part on every bed, and digits set its count from the lowest unit", async () => {
    renderToday(fakeCore({ results: SHARED() }));
    await screen.findByRole("heading", { level: 3, name: "Voron 2.4" });
    const gear = within(bed("Voron 2.4")).getByRole("checkbox", { name: "gear.stl unit 1" });
    fireEvent.keyDown(gear, { key: "F" });
    expect(preview().textContent).toContain("Will mark 2 units on 2 beds (Voron 2.4 and Trident):");
    expect(previewLines()).toEqual(["gear.stl: +2 printed (units 1 and 2)"]);
    const gear2 = within(bed("Trident")).getByRole("checkbox", { name: "gear.stl unit 2" });
    fireEvent.keyDown(gear2, { key: "1" });
    expect(previewLines()).toEqual(["gear.stl: +1 printed (unit 1)"]);
    expect((gear as HTMLInputElement).checked).toBe(true);
    expect((gear2 as HTMLInputElement).checked).toBe(false);
    fireEvent.keyDown(gear2, { key: "0" });
    expect(preview().textContent).toContain("Nothing picked.");
  });

  it("Space ticks one unit and Shift picks a run from the last one ticked", async () => {
    renderToday(fakeCore({ results: SHARED() }));
    await screen.findByRole("heading", { level: 3, name: "Voron 2.4" });
    fireEvent.click(unit("bracket.stl unit 2"));
    expect(previewLines()).toEqual(["bracket.stl: +1 printed (unit 2)"]);
    fireEvent.click(within(bed("Trident")).getByRole("checkbox", { name: "gear.stl unit 2" }), {
      shiftKey: true,
    });
    expect(preview().textContent).toContain("Will mark 4 units on 2 beds");
    expect(previewLines()).toEqual([
      "bracket.stl: +2 printed (units 2 and 3)",
      "gear.stl: +2 printed (units 1 and 2)",
    ]);
    fireEvent.click(unit("bracket.stl unit 3"));
    expect(previewLines()).toEqual([
      "bracket.stl: +1 printed (unit 2)",
      "gear.stl: +2 printed (units 1 and 2)",
    ]);
  });

  it("leaves the frame's shortcuts alone for keys it does not use", async () => {
    renderToday(fakeCore({ results: SHARED() }));
    await screen.findByRole("heading", { level: 3, name: "Voron 2.4" });
    const box = unit("bracket.stl unit 2");
    fireEvent.keyDown(box, { key: "p", ctrlKey: true });
    fireEvent.keyDown(box, { key: "x" });
    expect(preview().textContent).toContain("Nothing picked.");
  });
});

describe("UI5-02 one burst is one request and one undo entry", () => {
  it("confirms units from two beds in one request and undoes both with one token", async () => {
    const core = fakeCore({ results: SHARED() });
    renderToday(core);
    await screen.findByRole("heading", { level: 3, name: "Voron 2.4" });
    fireEvent.keyDown(within(bed("Voron 2.4")).getByRole("checkbox", { name: "gear.stl unit 1" }), {
      key: "f",
    });
    fireEvent.keyDown(unit("bracket.stl unit 2"), { key: "2" });
    fireEvent.click(screen.getByRole("button", { name: "Confirm 4 picked units" }));
    expect(await screen.findByText("Confirmed 4 units from Voron 2.4 and Trident.")).toBeTruthy();
    expect(core.posts).toHaveLength(1);
    expect(core.posts[0]!.body.beds).toEqual([
      {
        link_id: "link-voron",
        decisions: [
          { part_id: 1, unit_index: 1, result: "confirmed" },
          { part_id: 1, unit_index: 2, result: "confirmed" },
          { part_id: 3, unit_index: 0, result: "confirmed" },
        ],
      },
      { link_id: "link-trident", decisions: [{ part_id: 3, unit_index: 1, result: "confirmed" }] },
    ]);
    expect(screen.getAllByRole("button", { name: "Undo" })).toHaveLength(1);
    expect(core.state.marked).toBe(4);
    expect(
      await screen.findByRole("heading", {
        level: 2,
        name: "Clear 1 bed and confirm what came off",
      }),
    ).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    expect(
      await screen.findByText(
        "Undid the Voron 2.4 and Trident confirmation. 4 units are waiting again.",
      ),
    ).toBeTruthy();
    expect(core.posts.filter((p) => p.path.endsWith("/undo"))).toHaveLength(1);
    expect(core.state.marked).toBe(0);
    expect(
      await screen.findByRole("heading", {
        level: 2,
        name: "Clear 2 beds and confirm what came off",
      }),
    ).toBeTruthy();
  });
});
