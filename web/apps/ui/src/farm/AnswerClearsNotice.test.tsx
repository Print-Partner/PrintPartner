import type {
  DeductionDetail,
  DeductionView,
  FilamentShortfalls,
  Health,
  ProfileSummary,
} from "@print-partner/rust-client";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { createMemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App, createQueryClient } from "../App";
import { routes } from "../routes";

// GRE-1757: an `unknown` deduction answered on the Farm log clears the Build Overview's
// Filament notice. The rows match the Rust-only walk in the PR: Build 2, spool 7, 20 g.
const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });

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
  modules: [
    { module: "farm", state: "on" },
    { module: "spoolman", state: "on" },
  ],
};

const BUILD: ProfileSummary = {
  id: 2,
  name: "Synthetic Build 01",
  order_number: null,
  special_request: null,
  part_count: 1,
  accepted_progress: { kind: "ready", total_units: 1, remaining_units: 0 },
  build_stale: false,
  freshness: { status: "current", accepted_input_set_id: 1, accepted_at: "2026-11-01T00:05:00Z" },
  archived_at: null,
  last_used_at: null,
  customer: null,
  due_date: null,
};

const SHORTFALLS: FilamentShortfalls = {
  kind: "ready",
  profile_id: 2,
  revision_id: 1,
  plan_version: 1,
  filaments: [
    {
      filament_color_id: "spoolman:sm-1:filament:2",
      label: "Prusament PLA · Galaxy Black",
      grams_needed: { kind: "measured", grams: 20, job_count: 1 },
      grams_available: { kind: "spoolman", grams: 732, spool_ids: [7, 8] },
      status: { kind: "sufficient" },
      parts: [
        {
          part_id: 6,
          filename: "synthetic-s05-part-01.stl",
          remaining_units: 0,
          grams_needed: { kind: "measured", grams: 20, job_count: 1 },
        },
      ],
    },
  ],
};

const UNKNOWN: DeductionView = {
  id: 1,
  state: "unknown",
  reason: null,
  link_id: "link-evidence",
  profile_id: 2,
  units: 1,
  integration_id: "sm-1",
  spool_id: 7,
  grams: 20,
  grams_basis: "measured",
  failure: "reply_lost",
  failure_status: null,
  created_at: "2026-11-21T15:00:00Z",
  updated_at: "2026-11-21T15:00:01Z",
};

const DETAIL: DeductionDetail = {
  deduction: UNKNOWN,
  grams_before: { kind: "known", grams: 512 },
  grams_now: { kind: "known", grams: 492 },
};

const LIST = "/api/v2/spoolman/deductions";
const NOTICE = "Answer in the Farm log";

/**
 * One core for both screens. Reconcile applies `deducted` the way the core does. With
 * `holdList` set, list reads made after the answer wait until `release` is called, so a
 * test can see what the Overview shows from the cache alone.
 */
function core() {
  let rows: DeductionView[] = [UNKNOWN];
  let answered = false;
  const held: (() => void)[] = [];
  const state = { holdList: false };
  const listed = () => json({ deductions: rows });
  const fetchMock = vi.fn((path: string, init?: RequestInit) => {
    if (path === "/health") return Promise.resolve(json(health));
    if (path === "/api/v2/farm-log") {
      return Promise.resolve(json({ entries: [], next_cursor: null }));
    }
    if (path === LIST) {
      if (!(answered && state.holdList)) return Promise.resolve(listed());
      return new Promise<Response>((resolve) => held.push(() => resolve(listed())));
    }
    if (path === `${LIST}/1/reconcile` && init?.method === "POST") {
      const { answer } = JSON.parse(String(init.body)) as { answer: string };
      if (answer !== "deducted") return Promise.resolve(json({ status: 409 }, 409));
      answered = true;
      rows = [{ ...UNKNOWN, state: "applied", updated_at: "2026-11-21T15:05:00Z" }];
      return Promise.resolve(json(rows[0]));
    }
    if (path === `${LIST}/1`) return Promise.resolve(json(DETAIL));
    if (path === "/api/v2/plans/2") return Promise.resolve(json(BUILD));
    if (path === "/api/v2/plans/2/layers") {
      return Promise.resolve(json({ profile_id: 2, layers: [] }));
    }
    if (path === "/api/v2/plans/2/filament-shortfalls") return Promise.resolve(json(SHORTFALLS));
    return Promise.resolve(json({ status: 404 }, 404));
  });
  const release = () => held.splice(0).forEach((resolve) => resolve());
  return { fetchMock, state, release };
}

/** Opens the app at `path` with a new cache, as a full page load does. */
function load(fetchMock: ReturnType<typeof core>["fetchMock"], path: string) {
  vi.stubGlobal("fetch", fetchMock);
  const router = createMemoryRouter(routes, { initialEntries: [path] });
  const queryClient = createQueryClient();
  queryClient.setDefaultOptions({
    queries: { ...queryClient.getDefaultOptions().queries, retryDelay: 0 },
  });
  render(<App router={router} queryClient={queryClient} />);
  return router;
}

const filament = () => screen.getByRole("region", { name: "Filament" });
const lastDeduction = () =>
  within(filament()).getByText("Last deduction", { selector: "dt" }).nextElementSibling!
    .textContent;
const deductionRow = () =>
  within(screen.getByRole("table", { name: "Filament deductions" }))
    .getByRole("rowheader", { name: "Spool 7" })
    .closest("tr")!;

/** From the Overview's notice to the Farm log, where the owner answers Deducted. */
async function answerFromNotice() {
  const link = await within(await screen.findByRole("region", { name: "Filament" })).findByRole(
    "link",
    { name: NOTICE },
  );
  expect(link.closest("p")!.textContent).toBe(
    "1 deduction has an unknown outcome: 20 g. Spoolman's numbers may be off by up to 20 g. Answer in the Farm log",
  );
  expect(lastDeduction()).toMatch(/^20 g from spool 7outcome unknown, /);
  fireEvent.click(link);
  await within(await screen.findByRole("table", { name: "Filament deductions" })).findByText(
    "Spool before the send: 512 g. Now: 492 g.",
  );
  fireEvent.click(within(deductionRow()).getByRole("button", { name: "Deducted" }));
  await screen.findByText("Recorded the 20 g from spool 7 as deducted. Nothing was sent.");
  expect(deductionRow().textContent).toContain("applied");
}

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("GRE-1757: answering a deduction on the Farm log clears the Filament notice", () => {
  it("the notice is gone when the Overview opens again from the row's Build link", async () => {
    const { fetchMock, state, release } = core();
    const router = load(fetchMock, "/builds/2");
    await answerFromNotice();
    expect(router.state.location.pathname).toBe("/farm/log");

    // The Overview reads the list again on open. Holding that read shows the answer
    // already reached the shared cache, so no stale notice shows while it runs.
    state.holdList = true;
    fireEvent.click(within(deductionRow()).getByRole("link", { name: "Build 2" }));
    await screen.findByText("Enough");
    expect(router.state.location.pathname).toBe("/builds/2");
    expect(lastDeduction()).toMatch(/^20 g from spool 7applied, /);
    expect(within(filament()).queryByRole("link", { name: NOTICE })).toBeNull();
    await waitFor(() =>
      expect(fetchMock.mock.calls.filter(([path]) => path === LIST).length).toBeGreaterThan(2),
    );
    release();
    await waitFor(() => expect(lastDeduction()).toMatch(/^20 g from spool 7applied, /));
    expect(within(filament()).queryByRole("link", { name: NOTICE })).toBeNull();
    // One answer, and nothing else was written.
    expect(fetchMock.mock.calls.filter(([, init]) => init?.method === "POST")).toHaveLength(1);
  });

  it("the notice stays gone after a full reload of the Overview", async () => {
    const { fetchMock } = core();
    load(fetchMock, "/builds/2");
    await answerFromNotice();
    cleanup();

    load(fetchMock, "/builds/2");
    await screen.findByText("Enough");
    await waitFor(() => expect(lastDeduction()).toMatch(/^20 g from spool 7applied, /));
    expect(within(filament()).queryByRole("link", { name: NOTICE })).toBeNull();
    expect(filament().textContent).not.toContain("unknown outcome");
  });
});
