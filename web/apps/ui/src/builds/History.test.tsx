import type {
  BuildHistoryPage,
  DraftAcceptPreview,
  DraftWorkspace,
  Health,
} from "@print-partner/rust-client";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { createMemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App, createQueryClient } from "../App";
import { routes } from "../routes";
import { HISTORY_EMPTY } from "./planCopy";

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });

const health = (farm: "on" | "off"): Health => ({
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
  modules:
    farm === "on"
      ? [{ module: "farm", state: "on" }]
      : [{ module: "farm", state: "off", reason: "default_off" }],
});

type Handler = (path: string, init?: RequestInit) => Response | Promise<Response>;

/** A fake core: /health with the Farm module as given, plus `handler` for the rest. */
function core(handler: Handler, farm: "on" | "off" = "on") {
  return vi.fn((path: string, init?: RequestInit) =>
    Promise.resolve(path === "/health" ? json(health(farm)) : handler(path, init)),
  );
}

function renderAt(path: string, fetchImpl: ReturnType<typeof core>) {
  vi.stubGlobal("fetch", fetchImpl);
  const router = createMemoryRouter(routes, { initialEntries: [path] });
  const queryClient = createQueryClient();
  queryClient.setDefaultOptions({
    queries: { ...queryClient.getDefaultOptions().queries, retryDelay: 0 },
    mutations: { ...queryClient.getDefaultOptions().mutations, retryDelay: 0 },
  });
  render(<App router={router} queryClient={queryClient} />);
  return { router, queryClient };
}

const requested = (fetchMock: ReturnType<typeof core>) => fetchMock.mock.calls.map(([p]) => p);

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

type Entry = BuildHistoryPage["entries"][number];

const entry = (
  type: Entry["type"],
  id: string,
  minute: number,
  summary: string,
  extra: Partial<Entry> = {},
): Entry => ({
  id: `${type}:${id}`,
  type,
  at: `2026-11-05T10:${String(minute).padStart(2, "0")}:00.000Z`,
  actor: "user",
  summary,
  link: { kind: type, id },
  ...extra,
});

// One read returns every kind, newest first, and a cursor to one older page.
const FIRST: BuildHistoryPage = {
  plan_id: 2,
  entries: [
    entry("decision", "4", 50, "Use PETG for the brackets", { actor: "assistant" }),
    entry("checkoff_burst", "9", 40, "3 units checked off", {
      undo: { undoable: false, reason: "no_undo_token" },
    }),
    entry("send", "q-1", 30, "Sent plate 1 to Voron 2.4", {
      undo: { undoable: false, reason: "printer_command" },
    }),
    entry("snapshot", "2", 20, "Before accepting revision 2"),
    entry("revision", "2", 10, "Accepted revision 2"),
  ],
  next_cursor: "older-1",
};
const OLDER: BuildHistoryPage = {
  plan_id: 2,
  entries: [entry("revision", "1", 1, "Accepted revision 1")],
  next_cursor: null,
};

const history: Handler = (path) => {
  if (path === "/api/v2/plans/2/history") return json(FIRST);
  if (path === "/api/v2/plans/2/history?cursor=older-1") return json(OLDER);
  return json({ title: "Not Found", status: 404 }, 404);
};

const table = () => screen.getByRole("table", { name: "History" });
const rows = () =>
  within(table())
    .getAllByRole("row")
    .slice(1)
    .map((row) => row.querySelector("th")!.firstChild!.textContent);
const row = (summary: string) => screen.getByText(summary).closest("tr")!;

describe("UI8-01 History lists the Build's records from one read", () => {
  it("shows revisions, snapshots, bursts, sends and decisions, newest first", async () => {
    const fetchMock = core(history);
    renderAt("/builds/2/history", fetchMock);
    await screen.findByText("Use PETG for the brackets");
    expect(rows()).toEqual(["Decision", "Checkoff burst", "Send", "Snapshot", "Accepted revision"]);
    expect(within(row("Use PETG for the brackets")).getByText("Assistant")).toBeTruthy();
    expect(within(row("Accepted revision 2")).getByText("You")).toBeTruthy();
    expect(requested(fetchMock).filter((p) => p.includes("/history"))).toEqual([
      "/api/v2/plans/2/history",
    ]);
    const tabs = within(screen.getByRole("navigation", { name: "Build sections" })).getAllByRole(
      "link",
    );
    expect(tabs.find((t) => t.getAttribute("aria-current") === "page")?.textContent).toBe(
      "History",
    );
  });

  it("loads older entries from the page's cursor", async () => {
    const fetchMock = core(history);
    renderAt("/builds/2/history", fetchMock);
    fireEvent.click(await screen.findByRole("button", { name: "Show older entries" }));
    await screen.findByText("Accepted revision 1");
    expect(rows()).toHaveLength(6);
    expect(requested(fetchMock)).toContain("/api/v2/plans/2/history?cursor=older-1");
    expect(screen.queryByRole("button", { name: "Show older entries" })).toBeNull();
    expect(document.activeElement).toBe(row("Accepted revision 1"));
  });

  it("keeps the loaded entries when an older page fails, and retries that page", async () => {
    let olderUp = false;
    const fetchMock = core((path) =>
      path.includes("cursor=") && !olderUp ? json({}, 503) : history(path),
    );
    renderAt("/builds/2/history", fetchMock);
    fireEvent.click(await screen.findByRole("button", { name: "Show older entries" }));
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("Could not load older entries.");
    expect(screen.queryByText(/Could not refresh/)).toBeNull();
    expect(rows()).toHaveLength(5);
    expect(document.activeElement).toBe(within(alert).getByRole("button", { name: "Retry" }));
    olderUp = true;
    fireEvent.click(within(alert).getByRole("button", { name: "Retry" }));
    await screen.findByText("Accepted revision 1");
    expect(rows()).toHaveLength(6);
    expect(requested(fetchMock).filter((p) => p === "/api/v2/plans/2/history")).toHaveLength(1);
  });

  it("shows Not found for a missing Build and does not retry it", async () => {
    const fetchMock = core(() =>
      json({ title: "Not Found", status: 404, detail: "Profile not found" }, 404),
    );
    renderAt("/builds/999/history", fetchMock);
    expect(await screen.findByRole("heading", { name: "Not found" })).toBeTruthy();
    expect(screen.queryByText(/Could not load/)).toBeNull();
    expect(requested(fetchMock).filter((p) => p === "/api/v2/plans/999/history")).toHaveLength(1);
  });

  it("says when nothing has happened yet", async () => {
    renderAt(
      "/builds/2/history",
      core(() => json({ plan_id: 2, entries: [], next_cursor: null })),
    );
    expect(await screen.findByText(HISTORY_EMPTY)).toBeTruthy();
    expect(HISTORY_EMPTY).toMatch(/^Nothing has happened to this Build yet\./);
    expect(HISTORY_EMPTY).toMatch(/snapshots Accept saves/);
  });
});

describe("UI8-03 History says which entries cannot be undone and why", () => {
  it("shows a send and an old burst as not undoable with the reason, and offers no Undo", async () => {
    renderAt("/builds/2/history", core(history));
    await screen.findByText("Sent plate 1 to Voron 2.4");
    const send = row("Sent plate 1 to Voron 2.4");
    expect(send.lastElementChild!.textContent).toBe(
      "Not undoableA printer command cannot be undone",
    );
    expect(row("3 units checked off").lastElementChild!.textContent).toBe(
      "Not undoableNo undo is kept for this burst",
    );
    expect(row("Accepted revision 2").lastElementChild!.textContent).toBe("");
    expect(screen.queryByRole("button", { name: "Undo" })).toBeNull();
  });
});

type Undo = NonNullable<Entry["undo"]>;
const refusedUndo = (reason: NonNullable<Undo["reason"]>, detail = "Refused"): Undo => ({
  undoable: false,
  reason,
  detail,
});
const SCOPE = "Undo reverses the whole burst: 2 beds on Builds 2 and 5";
const BURST = entry("checkoff_burst", "job-a", 45, "Checked off 2 units", {
  undo: { undoable: true, scope: SCOPE },
});
const UNDO_PATH = "/api/v2/plans/2/history/checkoff_burst%3Ajob-a/undo";
const BURSTS: BuildHistoryPage = {
  plan_id: 2,
  entries: [
    BURST,
    entry("checkoff_burst", "manual-b", 44, "Unchecked 1 unit", {
      undo: refusedUndo("state_changed", "Checkoff changed since this burst"),
    }),
    entry("checkoff_burst", "manual-c", 43, "Assembled 1 unit", {
      undo: refusedUndo("already_undone"),
    }),
    entry("checkoff_burst", "job-1", 42, "Checked off 1 unit from plate.gcode", {
      undo: refusedUndo("archived"),
    }),
    entry("send", "q-1", 30, "Sent plate 1 to Voron 2.4", { undo: refusedUndo("printer_command") }),
  ],
  next_cursor: null,
};

/** History with one undoable burst. `undoAnswer` answers each undo POST and calls `land`
 * when the core applied that undo; History then lists the burst as already undone. */
function undoable(
  undoAnswer: (land: () => void) => Response = (land) => {
    land();
    return json({ undone_at: "2026-11-05T11:00:00.000Z", entry: null });
  },
) {
  let undone = false;
  return core((path, init) => {
    if (path === UNDO_PATH && init?.method === "POST") {
      return undoAnswer(() => {
        undone = true;
      });
    }
    if (path !== "/api/v2/plans/2/history") return json({ title: "Not Found", status: 404 }, 404);
    if (!undone) return json(BURSTS);
    const done = { ...BURST, undo: refusedUndo("already_undone") };
    return json({ ...BURSTS, entries: [done, ...BURSTS.entries.slice(1)] });
  });
}

const undoCell = (summary: string) => row(summary).lastElementChild!.textContent;

describe("UI8-03 Undo of a checkoff burst works from History", () => {
  it("offers Undo only where the core allows it and says why elsewhere", async () => {
    renderAt("/builds/2/history", undoable());
    await screen.findByText("Checked off 2 units");
    expect(within(row("Checked off 2 units")).getByRole("button", { name: "Undo" })).toBeTruthy();
    expect(undoCell("Checked off 2 units")).toBe(`Undo${SCOPE}`);
    expect(screen.getAllByRole("button", { name: "Undo" })).toHaveLength(1);
    expect(undoCell("Unchecked 1 unit")).toBe(
      "Not undoableCheckoff changed sinceCheckoff changed since this burst",
    );
    expect(undoCell("Assembled 1 unit")).toBe("Not undoableAlready undone");
    expect(undoCell("Checked off 1 unit from plate.gcode")).toBe(
      "Not undoableA Build it touched is archived",
    );
    expect(undoCell("Sent plate 1 to Voron 2.4")).toBe(
      "Not undoableA printer command cannot be undone",
    );
  });

  it("undoes a burst with a bodyless POST and reloads every core read", async () => {
    const fetchMock = undoable();
    const { queryClient } = renderAt("/builds/2/history", fetchMock);
    const invalidate = vi.spyOn(queryClient, "invalidateQueries");
    await screen.findByText("Checked off 2 units");
    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    const status = await screen.findByText("Undid the checkoff burst: Checked off 2 units.");
    expect(status.getAttribute("role")).toBe("status");
    expect(document.activeElement).toBe(status.parentElement);
    const post = fetchMock.mock.calls.find(([, init]) => init?.method === "POST")!;
    expect(post[0]).toBe(UNDO_PATH);
    expect(post[1]!.body).toBeUndefined();
    await waitFor(() => expect(undoCell("Checked off 2 units")).toBe("Not undoableAlready undone"));
    expect(invalidate.mock.calls.map(([filters]) => filters?.queryKey)).toEqual([["core"]]);
  });

  it("says why a refused undo failed", async () => {
    const detail = "Checkoff changed since this burst: a later change on its Parts depends on it";
    renderAt(
      "/builds/2/history",
      undoable(() => json({ title: "Conflict", status: 409, detail }, 409)),
    );
    await screen.findByText("Checked off 2 units");
    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("Could not undo the checkoff burst.");
    expect(alert.textContent).toContain(detail);
  });

  it("sends one undo however often Undo is clicked while it is in flight", async () => {
    const fetchMock = undoable();
    renderAt("/builds/2/history", fetchMock);
    await screen.findByText("Checked off 2 units");
    const button = screen.getByRole("button", { name: "Undo" });
    fireEvent.click(button);
    fireEvent.click(button);
    await screen.findByText("Undid the checkoff burst: Checked off 2 units.");
    expect(fetchMock.mock.calls.filter(([, init]) => init?.method === "POST")).toHaveLength(1);
  });

  it.each([
    [409, "This Checkoff burst was already undone"],
    // A printer burst's undo deletes its entry.
    [404, "History entry not found"],
  ])(
    "counts an undo whose answer was lost as done when the resend answers %i %s",
    async (code, detail) => {
      let sends = 0;
      const fetchMock = undoable((land) => {
        sends += 1;
        // The first send lands, but its answer is lost on the way back.
        if (sends === 1) {
          land();
          throw new TypeError("Failed to fetch");
        }
        return json({ title: "Refused", status: code, detail }, code);
      });
      renderAt("/builds/2/history", fetchMock);
      await screen.findByText("Checked off 2 units");
      fireEvent.click(screen.getByRole("button", { name: "Undo" }));
      await screen.findByText("Undid the checkoff burst: Checked off 2 units.");
      expect(screen.queryByRole("alert")).toBeNull();
      expect(sends).toBe(2);
      await waitFor(() =>
        expect(undoCell("Checked off 2 units")).toBe("Not undoableAlready undone"),
      );
    },
  );

  it("says an undo may not have reached the core when every send is lost", async () => {
    renderAt(
      "/builds/2/history",
      undoable(() => {
        throw new TypeError("Failed to fetch");
      }),
    );
    await screen.findByText("Checked off 2 units");
    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain(
      "The undo of the checkoff burst may not have reached the core. History was reloaded",
    );
    expect(alert.textContent).not.toContain("Could not undo");
    expect(document.activeElement).toBe(alert.parentElement);
  });
});

// Restore as draft: Build 2 at accepted revision 4, with one restorable snapshot and one the
// old app saved.
const DIGEST = "a".repeat(64);
const BASE = { revision_id: 4, plan_version: 3 };
const BUILD = { name: "Bracket kit", order_number: null, customer: null, due_date: null };
const OPEN = {
  draft_id: 9,
  state: "open",
  lifecycle_version: 2,
  snapshot_digest: DIGEST,
  base: BASE,
};
const RESTORED = { ...OPEN, draft_id: 12, lifecycle_version: 1 };
const RESTORE_PATH = "/api/v2/plans/2/snapshots/7/restore";

const SNAPSHOTS: BuildHistoryPage = {
  plan_id: 2,
  entries: [
    entry("snapshot", "7", 30, "Snapshot before apply", { restorable: true }),
    entry("snapshot", "3", 20, "Before PETG", { restorable: false }),
    entry("revision", "4", 10, "Accepted revision 4"),
  ],
  next_cursor: null,
};

const restoredWorkspace = (draft: typeof OPEN): DraftWorkspace => ({
  profile_id: 2,
  draft,
  parts: [
    {
      draft_part_id: 31,
      base_revision_part_id: 11,
      part_key: "bracket.stl",
      filename: "bracket.stl",
      relative_path: "stl/bracket.stl",
      source_layer: "base:Kit",
      role: "primary",
      quantity_inferred: 2,
      quantity_override: null,
      quantity_effective: 2,
      included: true,
    },
  ],
  diff: { base_is_current: true, added: [], removed: [], changed: [] },
  reconciliation: { kind: "ready", reused_units: 2, new_units: 0, surplus_units: 0 },
});

const restoredPreview: DraftAcceptPreview = {
  kind: "changed",
  revision: {
    draft_id: 12,
    expected_snapshot_digest: DIGEST,
    expected_lifecycle_version: 1,
    expected_base: BASE,
    expected_header_version: null,
  },
  snapshot: {
    saved_before_accept: true,
    detail: "Print Partner saves a snapshot of the accepted Plan before it accepts this draft.",
  },
  changes: {
    fields: [
      {
        draft_part_id: 31,
        filename: "bracket.stl",
        field: "quantity_override",
        old: { kind: "integer", value: 4 },
        new: null,
      },
    ],
    header: [],
    inputs: [],
    parts_added: [],
    parts_removed: [],
    required_units: { added: [], removed: [], printed_affected: [] },
    plates: { cleared: [] },
    stock: { kind: "off" },
    duplicate_orders: [],
    production: null,
    blocked_by: [],
  },
};

type Restore = (init: RequestInit) => Response | Promise<Response>;

/**
 * A fake core for restore: History, the Plan reads, and `restore` for the restore POST.
 * The Plan shows the restored draft once a restore has answered 200.
 */
function restoreCore(restore: Restore, { open = false } = {}) {
  let restored = false;
  return core((path, init) => {
    if (path === RESTORE_PATH) {
      return Promise.resolve(restore(init!)).then((response) => {
        if (response.ok) restored = true;
        return response;
      });
    }
    if (path === "/api/v2/plans/2/history") return json(SNAPSHOTS);
    if (path === "/api/v2/plans/2/drafts/header") return json({ build: BUILD, base: BASE });
    if (path === "/api/v2/plans/2/drafts") {
      const drafts = restored ? [RESTORED] : open ? [OPEN] : [];
      return json({ profile_id: 2, drafts });
    }
    if (path === "/api/v2/plans/2/drafts/9") return json(restoredWorkspace(OPEN));
    if (path === "/api/v2/plans/2/drafts/9/header") {
      return json({ draft_id: 9, build: BUILD, draft: null });
    }
    if (path === "/api/v2/plans/2/drafts/12") return json(restoredWorkspace(RESTORED));
    if (path === "/api/v2/plans/2/drafts/12/header") {
      return json({ draft_id: 12, build: BUILD, draft: null });
    }
    if (path === "/api/v2/plans/2/drafts/12/accept-preview") return json(restoredPreview);
    return json({ title: "Not Found", status: 404 }, 404);
  });
}

const restored = () => json(restoredWorkspace(RESTORED));
const refused = (status: number, code: string) =>
  json({ type: "about:blank", title: "Conflict", status, code }, status);

const writes = (fetchMock: ReturnType<typeof core>) =>
  fetchMock.mock.calls
    .filter(([, init]) => init?.method && init.method !== "GET")
    .map(([path, init]) => ({
      path,
      key: new Headers(init!.headers).get("idempotency-key"),
      body: JSON.parse(String(init!.body)),
    }));

describe("UI8-02 R on a snapshot opens Restore as draft into the Accept gate", () => {
  it("R on a restorable snapshot restores it as a draft and opens Accept on that draft", async () => {
    const fetchMock = restoreCore(restored);
    const { router } = renderAt("/builds/2/history", fetchMock);
    await screen.findByText("Snapshot before apply");
    fireEvent.keyDown(row("Snapshot before apply"), { key: "r" });
    const gate = await screen.findByRole("dialog", { name: "Accept this draft" });
    expect(router.state.location.pathname).toBe("/builds/2/plan");
    expect(gate.textContent).toContain(
      "This draft restores Snapshot before apply. Accepting publishes that Plan. Printed units and printer commands stay as they are.",
    );
    await within(gate).findByText("bracket.stl: Quantity 4 to none");
    expect(gate.contains(document.activeElement)).toBe(true);
    // The restore is the only write: nothing is accepted, and no printer is asked.
    const sent = writes(fetchMock);
    expect(sent).toEqual([
      {
        path: RESTORE_PATH,
        key: expect.any(String),
        body: { mode: "draft", expected_base: BASE, expected_draft: null },
      },
    ]);
    expect(requested(fetchMock)).toContain("/api/v2/plans/2/drafts/12/accept-preview");
  });

  it("Restore as draft names the open draft it replaces", async () => {
    const fetchMock = restoreCore(restored, { open: true });
    renderAt("/builds/2/history", fetchMock);
    await screen.findByText("Snapshot before apply");
    fireEvent.click(
      within(row("Snapshot before apply")).getByRole("button", { name: "Restore as draft" }),
    );
    await screen.findByRole("dialog", { name: "Accept this draft" });
    expect(writes(fetchMock)[0]!.body).toEqual({
      mode: "draft",
      expected_base: BASE,
      expected_draft: OPEN,
    });
  });

  it("offers restore only on snapshots the core can restore", async () => {
    const fetchMock = restoreCore(restored);
    renderAt("/builds/2/history", fetchMock);
    await screen.findByText("Before PETG");
    expect(row("Before PETG").lastElementChild!.textContent).toBe(
      "Not restorableIt holds no accepted Plan of this Build",
    );
    expect(within(row("Accepted revision 4")).queryByRole("button")).toBeNull();
    expect(screen.getAllByRole("button", { name: "Restore as draft" })).toHaveLength(1);
    fireEvent.keyDown(row("Before PETG"), { key: "r" });
    fireEvent.keyDown(row("Accepted revision 4"), { key: "R" });
    fireEvent.keyDown(row("Snapshot before apply"), { key: "r", ctrlKey: true });
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(writes(fetchMock)).toEqual([]);
  });

  it.each([
    [
      409,
      "snapshot_not_restorable",
      "This snapshot holds no accepted Plan of this Build, so it cannot be restored.",
    ],
    [409, "inputs_changed", "The Build's Sources changed since this snapshot was saved"],
    [409, "printed_units_affected", "Restore never rewinds printed units."],
    [409, "base_changed", "The accepted Plan changed while restoring."],
    [409, "draft_changed", "The open draft changed while restoring."],
    [409, "accepted_baseline_required", "This Build has no accepted Plan yet"],
    [422, "required_unit_set_unavailable", "The accepted Plan has no Required units yet"],
    [422, "invalid_header", "saved in this snapshot is no longer valid"],
    [422, "build_archived", "This Build is archived, so its Plan cannot change."],
    [404, "snapshot_not_found", "This snapshot no longer exists."],
  ])("says a %i %s refusal in words and moves focus to it", async (status, code, words) => {
    const fetchMock = restoreCore(() => refused(status, code));
    const { router } = renderAt("/builds/2/history", fetchMock);
    await screen.findByText("Snapshot before apply");
    fireEvent.keyDown(row("Snapshot before apply"), { key: "r" });
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("Could not restore Snapshot before apply.");
    expect(alert.textContent).toContain(words);
    await waitFor(() => expect(document.activeElement?.contains(alert)).toBe(true));
    expect(router.state.location.pathname).toBe("/builds/2/history");
    expect(writes(fetchMock)).toHaveLength(1);
  });

  it("sends a refused restore again with a new key", async () => {
    const answers = [refused(409, "base_changed"), restored()];
    const fetchMock = restoreCore(() => answers.shift()!);
    renderAt("/builds/2/history", fetchMock);
    await screen.findByText("Snapshot before apply");
    fireEvent.keyDown(row("Snapshot before apply"), { key: "r" });
    await screen.findByRole("alert");
    fireEvent.keyDown(row("Snapshot before apply"), { key: "r" });
    await screen.findByRole("dialog", { name: "Accept this draft" });
    const [first, second] = writes(fetchMock);
    expect(second!.key).not.toBe(first!.key);
  });

  it("sends one restore however often R is pressed while it is in flight", async () => {
    let answer!: (response: Response) => void;
    const fetchMock = restoreCore(() => new Promise<Response>((resolve) => (answer = resolve)));
    renderAt("/builds/2/history", fetchMock);
    await screen.findByText("Snapshot before apply");
    fireEvent.keyDown(row("Snapshot before apply"), { key: "r" });
    await waitFor(() => expect(writes(fetchMock)).toHaveLength(1));
    expect((await screen.findByText("Restoring Snapshot before apply.")).getAttribute("role")).toBe(
      "status",
    );
    fireEvent.keyDown(row("Snapshot before apply"), { key: "r" });
    fireEvent.click(screen.getByRole("button", { name: "Restore as draft" }));
    answer(restored());
    await screen.findByRole("dialog", { name: "Accept this draft" });
    expect(writes(fetchMock)).toHaveLength(1);
  });

  it("resends a restore whose answer was lost with the same key and body", async () => {
    let sends = 0;
    const fetchMock = restoreCore(() => {
      sends += 1;
      if (sends === 1) throw new TypeError("Failed to fetch");
      return restored();
    });
    renderAt("/builds/2/history", fetchMock);
    await screen.findByText("Snapshot before apply");
    fireEvent.keyDown(row("Snapshot before apply"), { key: "r" });
    await screen.findByRole("dialog", { name: "Accept this draft" });
    const [first, second] = writes(fetchMock);
    expect(second).toEqual(first);
  });

  it("keeps the key after every send is lost, so Restore again replays it", async () => {
    let lost = true;
    const fetchMock = restoreCore(() => {
      if (lost) throw new TypeError("Failed to fetch");
      return restored();
    });
    renderAt("/builds/2/history", fetchMock);
    await screen.findByText("Snapshot before apply");
    fireEvent.keyDown(row("Snapshot before apply"), { key: "r" });
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain(
      "The restore of Snapshot before apply may not have reached the core.",
    );
    expect(alert.textContent).not.toContain("Could not restore");
    lost = false;
    fireEvent.click(screen.getByRole("button", { name: "Restore as draft" }));
    await screen.findByRole("dialog", { name: "Accept this draft" });
    const sent = writes(fetchMock);
    expect(sent.length).toBeGreaterThan(2);
    expect(new Set(sent.map((w) => w.key)).size).toBe(1);
    expect(new Set(sent.map((w) => JSON.stringify(w.body))).size).toBe(1);
  });

  it("opens nothing when a replayed restore's draft is no longer open", async () => {
    const fetchMock = restoreCore(() =>
      json(restoredWorkspace({ ...RESTORED, state: "consumed" })),
    );
    const { router } = renderAt("/builds/2/history", fetchMock);
    await screen.findByText("Snapshot before apply");
    fireEvent.keyDown(row("Snapshot before apply"), { key: "r" });
    const status = await screen.findByText(
      /was already restored, and that draft is no longer open/,
    );
    expect(status.getAttribute("role")).toBe("status");
    expect(router.state.location.pathname).toBe("/builds/2/history");
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("sends nothing when the Plan cannot be read first, and does not call it lost", async () => {
    const fetchMock = restoreCore(restored);
    const planDown = vi.fn((path: string, init?: RequestInit) =>
      path === "/api/v2/plans/2/drafts"
        ? Promise.reject(new TypeError("Failed to fetch"))
        : fetchMock(path, init),
    );
    renderAt("/builds/2/history", planDown);
    await screen.findByText("Snapshot before apply");
    fireEvent.keyDown(row("Snapshot before apply"), { key: "r" });
    const alert = await screen.findByRole("alert", undefined, { timeout: 5000 });
    expect(alert.textContent).toContain("Could not restore Snapshot before apply.");
    expect(alert.textContent).toContain("Could not read the Plan before restoring");
    expect(writes(fetchMock)).toEqual([]);
  });

  it("shows Not found when the Build is gone", async () => {
    const fetchMock = restoreCore(() => refused(404, "profile_not_found"));
    renderAt("/builds/2/history", fetchMock);
    await screen.findByText("Snapshot before apply");
    fireEvent.keyDown(row("Snapshot before apply"), { key: "r" });
    expect(await screen.findByRole("heading", { name: "Not found" })).toBeTruthy();
  });

  it("opens no gate on a later visit to the Plan tab", async () => {
    const fetchMock = restoreCore(restored);
    const { router } = renderAt("/builds/2/history", fetchMock);
    await screen.findByText("Snapshot before apply");
    fireEvent.keyDown(row("Snapshot before apply"), { key: "r" });
    const gate = await screen.findByRole("dialog", { name: "Accept this draft" });
    fireEvent.click(within(gate).getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(router.state.location.state).toBeNull();
    await router.navigate("/builds/2/history");
    await router.navigate("/builds/2/plan");
    await screen.findByText(/A draft is open/);
    expect(screen.queryByRole("dialog")).toBeNull();
  });
});
