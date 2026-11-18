import type {
  DeductionDetail,
  DeductionView,
  FarmLogPage,
  Health,
} from "@print-partner/rust-client";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { createMemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App, createQueryClient } from "../App";
import { routes } from "../routes";

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });

const health = (spoolman: "on" | "off"): Health => ({
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
    { module: "farm", state: "on" },
    spoolman === "on"
      ? { module: "spoolman", state: "on" }
      : { module: "spoolman", state: "off", reason: "default_off" },
  ],
});

const LOG: FarmLogPage = {
  entries: [
    {
      id: "checkoff_burst:1",
      kind: "checkoff_burst",
      at: "2026-11-17T15:00:00.000Z",
      summary: "Verified 1 bed on Voron 2.4",
      build_ids: [2],
      printer_ids: ["voron"],
      units_confirmed: 2,
      units_rejected: 0,
      undoes: null,
      send_state: null,
      undo: { undoable: false, reason: "printer_command", detail: null },
    },
  ],
  next_cursor: null,
};

const deduction = (id: number, extra: Partial<DeductionView>): DeductionView => ({
  id,
  state: "applied",
  reason: null,
  link_id: `link-${id}`,
  profile_id: 2,
  units: 2,
  integration_id: "local",
  spool_id: 7,
  grams: 20,
  grams_basis: "measured",
  failure: null,
  failure_status: null,
  created_at: "2026-11-17T15:00:00Z",
  updated_at: "2026-11-17T15:00:01Z",
  ...extra,
});

const UNKNOWN = deduction(3, { state: "unknown", failure: "reply_lost", spool_id: 7, grams: 20 });
const FAILED = deduction(2, {
  state: "failed",
  failure: "refused",
  failure_status: 500,
  spool_id: 9,
  grams: 12,
});
const APPLIED = deduction(1, { spool_id: 4, grams: 30 });
const SKIPPED = deduction(4, { state: "skipped", reason: "no_pinned_spool", spool_id: null });

const DETAILS: Record<number, DeductionDetail> = {
  3: {
    deduction: UNKNOWN,
    grams_before: { kind: "known", grams: 820 },
    grams_now: { kind: "known", grams: 800 },
  },
  2: {
    deduction: FAILED,
    grams_before: { kind: "known", grams: 500 },
    grams_now: { kind: "unknown" },
  },
};

const LIST = "/api/v2/spoolman/deductions";

type Reconcile = (id: number, answer: string) => Response | Promise<Response>;

/**
 * A fake core: the Farm log, the deduction list and each row's detail. `reconcile` answers
 * each POST; by default it applies the answer the way the core does and returns the row.
 */
function core(
  options: {
    spoolman?: "on" | "off";
    reconcile?: Reconcile;
    rows?: DeductionView[];
    log?: FarmLogPage;
  } = {},
) {
  let rows = options.rows ?? [SKIPPED, UNKNOWN, FAILED, APPLIED];
  const apply: Reconcile = (id, answer) => {
    const next = { deducted: "applied", retry: "pending", dismiss: "dismissed" }[answer]!;
    rows = rows.map((row) =>
      row.id === id
        ? { ...row, state: next, failure: next === "pending" ? null : row.failure }
        : row,
    );
    return json(rows.find((row) => row.id === id));
  };
  const reconcile = options.reconcile ?? apply;
  return vi.fn((path: string, init?: RequestInit) => {
    if (path === "/health") return Promise.resolve(json(health(options.spoolman ?? "on")));
    if (path === "/api/v2/farm-log") return Promise.resolve(json(options.log ?? LOG));
    if (path === LIST) return Promise.resolve(json({ deductions: rows }));
    const match = /^\/api\/v2\/spoolman\/deductions\/(\d+)(\/reconcile)?$/.exec(path);
    if (match && match[2] && init?.method === "POST") {
      const body = JSON.parse(String(init.body)) as { answer: string };
      return Promise.resolve(reconcile(Number(match[1]), body.answer));
    }
    if (match && DETAILS[Number(match[1])]) return Promise.resolve(json(DETAILS[Number(match[1])]));
    return Promise.resolve(json({ title: "Not Found", status: 404 }, 404));
  });
}

function renderAt(fetchImpl: ReturnType<typeof core>) {
  vi.stubGlobal("fetch", fetchImpl);
  const router = createMemoryRouter(routes, { initialEntries: ["/farm/log"] });
  const queryClient = createQueryClient();
  queryClient.setDefaultOptions({
    queries: { ...queryClient.getDefaultOptions().queries, retryDelay: 0 },
    mutations: { ...queryClient.getDefaultOptions().mutations, retryDelay: 0 },
  });
  render(<App router={router} queryClient={queryClient} />);
  return { queryClient };
}

const requested = (fetchMock: ReturnType<typeof core>) => fetchMock.mock.calls.map(([p]) => p);
const posts = (fetchMock: ReturnType<typeof core>) =>
  fetchMock.mock.calls.filter(([, init]) => init?.method === "POST");

const table = () => screen.getByRole("table", { name: "Filament deductions" });
const row = (spool: string) =>
  within(table()).getByRole("rowheader", { name: spool }).closest("tr")!;
const buttons = (tr: HTMLElement) =>
  within(tr)
    .queryAllByRole("button")
    .map((b) => b.textContent);

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("GRE-411 PR 4: Filament deductions on the Farm log", () => {
  it("shows spool, grams, before and now, and the answers on unknown and failed rows", async () => {
    const fetchMock = core();
    renderAt(fetchMock);
    expect(await screen.findByRole("heading", { name: "Filament deductions" })).toBeTruthy();
    await screen.findByRole("table", { name: "Filament deductions" });
    await within(row("Spool 7")).findByText("Spool before the send: 820 g. Now: 800 g.");

    const unknown = row("Spool 7");
    expect(unknown.textContent).toContain("20 g");
    expect(unknown.textContent).toContain("outcome unknown");
    expect(unknown.textContent).toContain("Spoolman's reply never came back");
    expect(within(unknown).getByRole("link", { name: "Build 2" }).getAttribute("href")).toBe(
      "/builds/2",
    );
    expect(buttons(unknown)).toEqual(["Deducted", "Retry", "Dismiss"]);

    const failed = row("Spool 9");
    await within(failed).findByText("Spool before the send: 500 g. Now: not known.");
    expect(failed.textContent).toContain("12 g");
    expect(failed.textContent).toContain("Spoolman refused it with HTTP 500");
    // A failed send never reached Spoolman's stock, so the core refuses `deducted` for it.
    expect(buttons(failed)).toEqual(["Retry", "Dismiss"]);

    expect(buttons(row("Spool 4"))).toEqual([]);
    expect(row("Spool 4").textContent).toContain("applied");
    expect(row("No spool").textContent).toContain("Because no spool is pinned");
    expect(buttons(row("No spool"))).toEqual([]);
    // Only the rows that take an answer ask Spoolman for grams.
    expect(
      requested(fetchMock)
        .filter((p) => p.startsWith(`${LIST}/`))
        .sort(),
    ).toEqual([`${LIST}/2`, `${LIST}/3`]);
  });

  it("answers Deducted and updates the row in place without reading the list again", async () => {
    const fetchMock = core();
    renderAt(fetchMock);
    await within(await screen.findByRole("table", { name: "Filament deductions" })).findByText(
      "Spool before the send: 820 g. Now: 800 g.",
    );
    fireEvent.click(within(row("Spool 7")).getByRole("button", { name: "Deducted" }));

    const status = await screen.findByText(
      "Recorded the 20 g from spool 7 as deducted. Nothing was sent.",
    );
    expect(status.getAttribute("role")).toBe("status");
    const [post] = posts(fetchMock);
    expect(post![0]).toBe("/api/v2/spoolman/deductions/3/reconcile");
    expect(JSON.parse(String(post![1]!.body))).toEqual({ answer: "deducted" });
    expect(row("Spool 7").textContent).toContain("applied");
    expect(buttons(row("Spool 7"))).toEqual([]);
    expect(row("Spool 7").textContent).not.toContain("Spool before the send");
    expect(requested(fetchMock).filter((p) => p === LIST)).toHaveLength(1);
    expect(requested(fetchMock).filter((p) => p === "/api/v2/farm-log")).toHaveLength(1);
  });

  it("answers Retry on a failed row and shows it waiting to send", async () => {
    const fetchMock = core();
    renderAt(fetchMock);
    await screen.findByRole("table", { name: "Filament deductions" });
    fireEvent.click(within(row("Spool 9")).getByRole("button", { name: "Retry" }));
    await screen.findByText("The 12 g from spool 9 will be sent to Spoolman once more.");
    expect(JSON.parse(String(posts(fetchMock)[0]![1]!.body))).toEqual({ answer: "retry" });
    expect(row("Spool 9").textContent).toContain("waiting to send");
    expect(row("Spool 9").textContent).not.toContain("refused");
    expect(buttons(row("Spool 9"))).toEqual([]);
    expect(requested(fetchMock).filter((p) => p === LIST)).toHaveLength(1);
  });

  it("answers Dismiss on an unknown row and shows it dismissed", async () => {
    const fetchMock = core();
    renderAt(fetchMock);
    await screen.findByRole("table", { name: "Filament deductions" });
    fireEvent.click(within(row("Spool 7")).getByRole("button", { name: "Dismiss" }));
    await screen.findByText("Dismissed the 20 g from spool 7. Nothing was sent.");
    expect(JSON.parse(String(posts(fetchMock)[0]![1]!.body))).toEqual({ answer: "dismiss" });
    expect(row("Spool 7").textContent).toContain("dismissed");
    expect(buttons(row("Spool 7"))).toEqual([]);
  });

  it("sends one answer however often it is clicked while in flight", async () => {
    let release: () => void = () => {};
    const fetchMock = core({
      reconcile: () =>
        new Promise<Response>((resolve) => {
          release = () => resolve(json({ ...UNKNOWN, state: "applied" }));
        }),
    });
    renderAt(fetchMock);
    await screen.findByRole("table", { name: "Filament deductions" });
    fireEvent.click(within(row("Spool 7")).getByRole("button", { name: "Deducted" }));
    fireEvent.click(within(row("Spool 7")).getByRole("button", { name: "Deducted" }));
    fireEvent.click(within(row("Spool 9")).getByRole("button", { name: "Dismiss" }));
    await vi.waitFor(() => expect(posts(fetchMock)).toHaveLength(1));
    release();
    await screen.findByText("Recorded the 20 g from spool 7 as deducted. Nothing was sent.");
    expect(posts(fetchMock)).toHaveLength(1);
  });

  it("says why a refused answer failed and reads the list again", async () => {
    const detail = "This deduction is applied, so it takes no answer";
    let refused = false;
    const fetchMock = core({
      reconcile: () => {
        refused = true;
        return json(
          { type: "about:blank", title: "Conflict", status: 409, code: "deduction_state", detail },
          409,
        );
      },
    });
    renderAt(fetchMock);
    await screen.findByRole("table", { name: "Filament deductions" });
    fireEvent.click(within(row("Spool 7")).getByRole("button", { name: "Deducted" }));
    const alert = await screen.findByRole("alert");
    expect(refused).toBe(true);
    expect(alert.textContent).toContain("Could not answer the deduction from spool 7.");
    expect(alert.textContent).toContain(detail);
    await vi.waitFor(() => expect(requested(fetchMock).filter((p) => p === LIST)).toHaveLength(2));
    expect(posts(fetchMock)).toHaveLength(1);
  });

  it("says an answer may not have reached the core when the send is lost, without resending", async () => {
    const fetchMock = core({
      reconcile: () => {
        throw new TypeError("Failed to fetch");
      },
    });
    renderAt(fetchMock);
    await screen.findByRole("table", { name: "Filament deductions" });
    fireEvent.click(within(row("Spool 7")).getByRole("button", { name: "Retry" }));
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain(
      "The answer for the 20 g from spool 7 may not have reached the core.",
    );
    await vi.waitFor(() => expect(requested(fetchMock).filter((p) => p === LIST)).toHaveLength(2));
    expect(posts(fetchMock)).toHaveLength(1);
  });

  it("lists deductions while the farm log itself is empty", async () => {
    renderAt(core({ log: { entries: [], next_cursor: null } }));
    await screen.findByText("No farm actions yet. Checkoff bursts and sends appear here.");
    await screen.findByRole("table", { name: "Filament deductions" });
    expect(buttons(row("Spool 7"))).toEqual(["Deducted", "Retry", "Dismiss"]);
    expect(screen.getByRole("heading", { name: "Actions" })).toBeTruthy();
  });

  it("says when there are no deductions yet", async () => {
    renderAt(core({ rows: [] }));
    expect(await screen.findByText(/No filament deductions yet\./)).toBeTruthy();
    expect(screen.queryByRole("table", { name: "Filament deductions" })).toBeNull();
  });

  it("with the Spoolman module off, says so and never reads deductions", async () => {
    const fetchMock = core({ spoolman: "off" });
    renderAt(fetchMock);
    await screen.findByText("Verified 1 bed on Voron 2.4");
    expect(screen.getByText("The Spoolman module is off.")).toBeTruthy();
    expect(requested(fetchMock).some((p) => p.startsWith(LIST))).toBe(false);
  });
});
