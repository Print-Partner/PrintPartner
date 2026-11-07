import type { FarmLogPage, Health } from "@print-partner/rust-client";
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

type Entry = FarmLogPage["entries"][number];
type Undo = Entry["undo"];

const ALLOWED: Undo = { undoable: true, reason: null, detail: null };
const refused = (reason: NonNullable<Undo["reason"]>, detail: string | null = null): Undo => ({
  undoable: false,
  reason,
  detail,
});

const entry = (
  kind: Entry["kind"],
  id: string,
  minute: number,
  summary: string,
  undo: Undo,
): Entry => ({
  id: `${kind}:${id}`,
  kind,
  at: `2026-11-05T10:${String(minute).padStart(2, "0")}:00.000Z`,
  summary,
  build_ids: [2],
  printer_ids: ["voron"],
  units_confirmed: kind === "checkoff_burst" ? 3 : null,
  units_rejected: kind === "checkoff_burst" ? 0 : null,
  undoes: kind === "checkoff_burst_undo" ? "checkoff_burst:1" : null,
  send_state: kind === "send" ? "done" : null,
  undo,
});

const BURST = entry("checkoff_burst", "3", 40, "Verified 1 bed on Voron 2.4", ALLOWED);
const UNDONE = entry(
  "checkoff_burst_undo",
  "4",
  45,
  "Undid the burst on Voron 2.4",
  refused("undo_entry"),
);
const PAGE: FarmLogPage = {
  entries: [
    BURST,
    entry("send", "q-1", 30, "Sent plate 1 to Voron 2.4", refused("printer_command")),
    entry(
      "checkoff_burst",
      "2",
      20,
      "Verified 2 beds on Trident",
      refused("state_changed", "Checkoff changed since this burst"),
    ),
    entry("checkoff_burst", "1", 10, "Verified 1 bed on Trident", refused("already_undone")),
  ],
  next_cursor: null,
};

/** The log, and after an undo the log with the burst undone and its undo entry on top. */
/** `undoAnswer` answers each undo POST; it calls `land` when the core applied that undo. */
function farmLog(
  undoAnswer: (land: () => void) => Response = (land) => {
    land();
    return json(UNDONE);
  },
) {
  let undone = false;
  const handler: Handler = (path, init) => {
    if (path === "/api/v2/farm-log/checkoff_burst%3A3/undo" && init?.method === "POST") {
      return undoAnswer(() => {
        undone = true;
      });
    }
    if (path !== "/api/v2/farm-log") return json({ title: "Not Found", status: 404 }, 404);
    if (!undone) return json(PAGE);
    return json({
      ...PAGE,
      entries: [UNDONE, { ...BURST, undo: refused("already_undone") }, ...PAGE.entries.slice(1)],
    });
  };
  return core(handler);
}

const row = (summary: string) => screen.getByText(summary).closest("tr")!;

describe("UI8-04 farm log with undo where allowed", () => {
  it("lists printer and queue actions with Undo only where the core allows it", async () => {
    renderAt("/farm/log", farmLog());
    await screen.findByText("Verified 1 bed on Voron 2.4");
    expect(
      within(row("Verified 1 bed on Voron 2.4")).getByRole("button", { name: "Undo" }),
    ).toBeTruthy();
    expect(screen.getAllByRole("button", { name: "Undo" })).toHaveLength(1);
    expect(row("Sent plate 1 to Voron 2.4").lastElementChild!.textContent).toBe(
      "Not undoableA printer command cannot be undone",
    );
    expect(row("Verified 2 beds on Trident").lastElementChild!.textContent).toBe(
      "Not undoableCheckoff changed sinceCheckoff changed since this burst",
    );
    expect(row("Verified 1 bed on Trident").lastElementChild!.textContent).toBe(
      "Not undoableAlready undone",
    );
    expect(
      within(row("Sent plate 1 to Voron 2.4"))
        .getByRole("link", { name: "Build 2" })
        .getAttribute("href"),
    ).toBe("/builds/2/history");
  });

  it("undoes a burst with a bodyless POST and reloads the log", async () => {
    const fetchMock = farmLog();
    renderAt("/farm/log", fetchMock);
    await screen.findByText("Verified 1 bed on Voron 2.4");
    fireEvent.click(
      within(row("Verified 1 bed on Voron 2.4")).getByRole("button", { name: "Undo" }),
    );
    const status = await screen.findByText("Undid the burst on Voron 2.4", { selector: "p" });
    expect(status.getAttribute("role")).toBe("status");
    await screen.findByText("Undid the burst on Voron 2.4", { selector: "span" });
    const post = fetchMock.mock.calls.find(([, init]) => init?.method === "POST")!;
    expect(post[0]).toBe("/api/v2/farm-log/checkoff_burst%3A3/undo");
    expect(post[1]!.body).toBeUndefined();
    expect(row("Verified 1 bed on Voron 2.4").lastElementChild!.textContent).toBe(
      "Not undoableAlready undone",
    );
    expect(requested(fetchMock).filter((p) => p === "/api/v2/farm-log")).toHaveLength(2);
  });

  it("says why a refused undo failed and reloads the log", async () => {
    const detail = "Checkoff changed since this burst: a later change on its Parts depends on it";
    renderAt(
      "/farm/log",
      farmLog(() => json({ type: "about:blank", title: "Conflict", status: 409, detail }, 409)),
    );
    await screen.findByText("Verified 1 bed on Voron 2.4");
    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("Could not undo the checkoff burst.");
    expect(alert.textContent).toContain(detail);
  });

  it("sends one undo however often Undo is clicked while it is in flight", async () => {
    const fetchMock = farmLog();
    renderAt("/farm/log", fetchMock);
    await screen.findByText("Verified 1 bed on Voron 2.4");
    const button = screen.getByRole("button", { name: "Undo" });
    fireEvent.click(button);
    fireEvent.click(button);
    await screen.findByText("Undid the burst on Voron 2.4", { selector: "p" });
    expect(fetchMock.mock.calls.filter(([, init]) => init?.method === "POST")).toHaveLength(1);
    expect(document.activeElement?.textContent).toBe("Undid the burst on Voron 2.4");
  });

  it("counts an undo whose answer was lost as done when the resend finds it already undone", async () => {
    let sends = 0;
    const fetchMock = farmLog((land) => {
      sends += 1;
      // The first send lands, but its answer is lost on the way back.
      if (sends === 1) {
        land();
        throw new TypeError("Failed to fetch");
      }
      return json(
        { title: "Conflict", status: 409, detail: "This Checkoff burst was already undone" },
        409,
      );
    });
    renderAt("/farm/log", fetchMock);
    await screen.findByText("Verified 1 bed on Voron 2.4");
    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    expect((await screen.findByText("Undid the checkoff burst.")).getAttribute("role")).toBe(
      "status",
    );
    expect(screen.queryByRole("alert")).toBeNull();
    expect(sends).toBe(2);
    await screen.findByText("Undid the burst on Voron 2.4", { selector: "span" });
    expect(row("Verified 1 bed on Voron 2.4").lastElementChild!.textContent).toBe(
      "Not undoableAlready undone",
    );
    expect(screen.queryByRole("button", { name: "Undo" })).toBeNull();
  });

  it("says an undo may not have reached the core when every send is lost", async () => {
    const fetchMock = farmLog(() => {
      throw new TypeError("Failed to fetch");
    });
    renderAt("/farm/log", fetchMock);
    await screen.findByText("Verified 1 bed on Voron 2.4");
    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain(
      "The undo of the checkoff burst may not have reached the core.",
    );
    expect(alert.textContent).not.toContain("Could not undo");
  });

  it("with the Farm module off, says what it would show", async () => {
    const fetchMock = core(() => json(PAGE), "off");
    renderAt("/farm/log", fetchMock);
    expect(await screen.findByText("The Farm module is off.")).toBeTruthy();
    expect(requested(fetchMock)).not.toContain("/api/v2/farm-log");
  });
});
