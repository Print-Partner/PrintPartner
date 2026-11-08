import type { BuildHistoryPage, Health } from "@print-partner/rust-client";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { createMemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App, createQueryClient } from "../App";
import { routes } from "../routes";

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
    expect(await screen.findByText("Nothing has happened to this Build yet.")).toBeTruthy();
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
const refused = (reason: NonNullable<Undo["reason"]>, detail = "Refused"): Undo => ({
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
      undo: refused("state_changed", "Checkoff changed since this burst"),
    }),
    entry("checkoff_burst", "manual-c", 43, "Assembled 1 unit", {
      undo: refused("already_undone"),
    }),
    entry("checkoff_burst", "job-1", 42, "Checked off 1 unit from plate.gcode", {
      undo: refused("archived"),
    }),
    entry("send", "q-1", 30, "Sent plate 1 to Voron 2.4", { undo: refused("printer_command") }),
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
    const done = { ...BURST, undo: refused("already_undone") };
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

  it("undoes a burst with a bodyless POST and reloads History, the farm log and Checkoff", async () => {
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
    expect(invalidate.mock.calls.map(([filters]) => filters?.queryKey)).toEqual([
      ["core", "history"],
      ["core", "farm-log"],
      ["core", "checkoff-units"],
    ]);
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
