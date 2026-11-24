import type { FarmPrinterList, Health, ProfileSummary, SendQueue } from "@print-partner/rust-client";
import { QueryClient } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { createMemoryRouter } from "react-router";
import { afterEach, expect, it, vi } from "vitest";
import { App } from "../App";
import { routes } from "../routes";

const QUEUE = "/api/v2/printer-send-queue";
const BUILDS = "/api/v2/plans";
const json = (body: unknown, status = 200) => Response.json(body, { status });

const health = (farm: "on" | "off"): Health => ({
  ok: true,
  authenticated: true,
  authentication_required: true,
  capabilities: [],
  multi_user: false,
  registration_open: false,
  modules: [
    farm === "on"
      ? { module: "farm", state: "on" }
      : { module: "farm", state: "off", reason: "turned_off" },
  ],
});

const summary = (id: number, name: string): ProfileSummary => ({
  id,
  name,
  order_number: null,
  special_request: null,
  part_count: 0,
  accepted_progress: { kind: "empty" },
  build_stale: false,
  freshness: {
    status: "untracked",
    accepted_at: null,
    accepted_input_set_id: null,
    reasons: [{ kind: "no_accepted_inputs" }],
  },
  archived_at: null,
  last_used_at: null,
  customer: null,
  due_date: null,
});
const profiles = [summary(2, "Synthetic spindle"), summary(5, "Synthetic bracket")];
const printers: FarmPrinterList["printers"] = ["Micron", "Trident"].map((name, i) => ({
  id: `p${i + 1}`,
  name,
  model: "Synthetic model",
  integration: null,
  state: { execution: "idle", bed_clear: "clear", display: "ready" },
  reason: null,
  last_heard_at: null,
  progress_percent: null,
  loaded_filaments: [],
}));
type Item = SendQueue["items"][number];
const queued: Item = {
  id: "queued",
  filename: "spindle.gcode",
  state: "queued",
  profile_id: 2,
  printer_id: "p1",
  start: false,
  wait_for_idle: false,
  unit_count: 0,
  created_at: null,
  updated_at: null,
  error: null,
  upload_job_id: null,
  outcome_unknown: false,
};
const unknown: Item = {
  ...queued,
  id: "unknown",
  profile_id: 5,
  filename: "bracket.gcode",
  printer_id: "p2",
  state: "error",
  outcome_unknown: true,
  upload_job_id: "job-unknown",
};
const removed: Item = { ...queued, id: "removed", filename: "removed.gcode", state: "cancelled" };

function renderFarm({
  farm = "on",
  queue = () => json({ items: [queued, unknown, removed] }),
  builds = () => json({ profiles }),
}: {
  farm?: "on" | "off";
  queue?: () => Response;
  builds?: () => Response;
} = {}) {
  const fetchMock = vi.fn(async (path: string, init?: RequestInit) => {
    if (init?.method && init.method !== "GET") return json({}, 405);
    if (path === "/health") return json(health(farm));
    if (path === "/api/v2/printers") return json({ printers });
    if (path === QUEUE) return queue();
    if (path === BUILDS) return builds();
    if (path === `${BUILDS}/5`) return json(profiles[1]);
    return json({}, 404);
  });
  vi.stubGlobal("fetch", fetchMock);
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createMemoryRouter(routes, { initialEntries: ["/farm"] });
  render(<App router={router} queryClient={queryClient} />);
  return {
    paths: () => fetchMock.mock.calls.map(([path]) => path),
    expectReadOnly: () => {
      expect(fetchMock.mock.calls.map(([, init]) => init?.method ?? "GET").every((m) => m === "GET"))
        .toBe(true);
      expect(queryClient.getMutationCache().getAll()).toEqual([]);
    },
  };
}

const queueSection = () => screen.getByRole("region", { name: "Send queue" });
const printerTable = () => screen.getByRole("table", { name: "Printers" });

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

it("shows sends from two Builds with matching printer names, states and Production links", async () => {
  const { paths, expectReadOnly } = renderFarm();
  const queue = within(await screen.findByRole("table", { name: "Send queue" }));
  const first = await queue.findByRole("row", { name: /Synthetic spindle/ });
  const second = queue.getByRole("row", { name: /Synthetic bracket/ });
  expect(within(first).getAllByRole("cell").map((cell) => cell.textContent)).toEqual([
    "spindle.gcode", "Micron", "Not sent",
  ]);
  expect(within(second).getAllByRole("cell").map((cell) => cell.textContent)).toEqual([
    "bracket.gcode", "Trident", "Outcome unknown",
  ]);
  expect(within(first).getByRole("link").getAttribute("href")).toBe("/builds/2/production");
  const production = within(second).getByRole("link", { name: "Open Production for Synthetic bracket" });
  expect(production.getAttribute("href")).toBe("/builds/5/production");
  expect(queue.queryByText("removed.gcode")).toBeNull();
  expect(within(queueSection()).queryByRole("button")).toBeNull();
  expect(within(printerTable()).getAllByText("Ready")).toHaveLength(2);
  expect(paths().filter((path) => path === QUEUE)).toHaveLength(1);
  expectReadOnly();

  fireEvent.click(production);
  const sends = within(await screen.findByRole("table", { name: "Sends" }));
  expect(sends.getByText("bracket.gcode")).toBeTruthy();
  expect(sends.queryByText("spindle.gcode")).toBeNull();
  expectReadOnly();
});

it("shows an empty queue independently of printer status", async () => {
  const { paths, expectReadOnly } = renderFarm({ queue: () => json({ items: [removed] }) });
  expect(await screen.findByText("No sends in the queue.")).toBeTruthy();
  expect(within(printerTable()).getAllByText("Ready")).toHaveLength(2);
  expect(paths()).not.toContain(BUILDS);
  expectReadOnly();
});

it("keeps printer status visible when the queue GET fails and can retry it", async () => {
  const queue = vi.fn(() => json({ items: [] }));
  queue.mockImplementationOnce(() => json({ title: "Send queue is unavailable." }, 503));
  const { paths, expectReadOnly } = renderFarm({ queue });
  const section = within(await screen.findByRole("region", { name: "Send queue" }));
  expect((await section.findByRole("alert")).textContent).toContain("Could not load Send queue.");
  expect(within(printerTable()).getAllByText("Ready")).toHaveLength(2);
  fireEvent.click(section.getByRole("button", { name: "Retry" }));
  expect(await section.findByText("No sends in the queue.")).toBeTruthy();
  expect(paths().filter((path) => path === QUEUE)).toHaveLength(2);
  expectReadOnly();
});

it("shows IDs if lookup fails and leaves an unassigned send without a Production link", async () => {
  const { expectReadOnly } = renderFarm({
    builds: () => json({ title: "Builds unavailable" }, 503),
    queue: () => json({
      items: [queued, { ...queued, id: "unassigned", profile_id: null, printer_id: "old-printer" }],
    }),
  });
  const queue = within(await screen.findByRole("table", { name: "Send queue", busy: false }));
  expect(queue.getByRole("link", { name: "Open Production for Build #2" }).getAttribute("href"))
    .toBe("/builds/2/production");
  const unassigned = within(queue.getByRole("row", { name: /No Build/ }));
  expect(unassigned.getByText("old-printer")).toBeTruthy();
  expect(unassigned.queryByRole("link")).toBeNull();
  expectReadOnly();
});

it("does not read or show the queue when the Farm Module is off", async () => {
  const { paths, expectReadOnly } = renderFarm({ farm: "off" });
  expect(await screen.findByText("The Farm module is off.")).toBeTruthy();
  expect(screen.queryByRole("region", { name: "Send queue" })).toBeNull();
  expect(paths()).not.toContain(QUEUE);
  expect(paths()).not.toContain(BUILDS);
  expectReadOnly();
});
