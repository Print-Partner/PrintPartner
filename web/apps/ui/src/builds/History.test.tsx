import type { BuildHistoryPage, Health } from "@print-partner/rust-client";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
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
      "Not undoableRecorded before Checkoff kept an undo for it",
    );
    expect(row("Accepted revision 2").lastElementChild!.textContent).toBe("");
    expect(screen.queryByRole("button", { name: "Undo" })).toBeNull();
  });
});
