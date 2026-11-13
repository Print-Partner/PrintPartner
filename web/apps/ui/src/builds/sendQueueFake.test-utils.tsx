import type { FarmPrinterList, Health, SendQueue } from "@print-partner/rust-client";
import { fireEvent, render, screen, within } from "@testing-library/react";
import { createMemoryRouter } from "react-router";
import { vi } from "vitest";
import { App, createQueryClient } from "../App";
import { routes } from "../routes";

// Test-only: the fake core the Production tab tests share. It records every request, so a
// test can assert what a printer simulator would have received.

export const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });

/** A problem answer with `code`, as the core sends a refusal. */
export const problem = (status: number, code: string, detail = `Refused: ${code}`) =>
  json(
    { type: "urn:print-partner:problem:conflict", title: "Conflict", status, code, detail },
    status,
  );

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
  modules: [
    farm === "on"
      ? { module: "farm", state: "on" }
      : { module: "farm", state: "off", reason: "default_off" },
  ],
});

export type Printer = FarmPrinterList["printers"][number];
export type State = Printer["state"];
export const printer = (id: string, name: string, state: State): Printer => ({
  id,
  name,
  model: "Voron Micron",
  integration: { id: `${id}-host`, kind: "moonraker" },
  state,
  reason: null,
  last_heard_at: null,
  progress_percent: null,
  loaded_filaments: [],
});
export const READY: State = { execution: "idle", bed_clear: "clear", display: "ready" };
export const BED: State = {
  execution: "idle",
  bed_clear: "needs_clearing",
  display: "bed_to_clear",
};
export const PRINTING: State = {
  execution: "printing",
  bed_clear: "needs_clearing",
  display: "printing",
};
export const UNREACHABLE: State = {
  execution: "unreachable",
  bed_clear: "unknown",
  display: "unreachable",
};

export type Item = SendQueue["items"][number];
export const item = (id: string, overrides: Partial<Item> = {}): Item => ({
  id,
  filename: "plate.gcode",
  printer_id: "p1",
  profile_id: 2,
  state: "queued",
  start: false,
  wait_for_idle: false,
  unit_count: 0,
  created_at: "2026-11-12T20:00:00.000Z",
  updated_at: "2026-11-12T20:00:00.000Z",
  error: null,
  upload_job_id: null,
  outcome_unknown: false,
  ...overrides,
});

type Answer = Response | Promise<Response>;
type Route = (path: string, init: RequestInit | undefined) => Answer | undefined;

/** The path's item id for `/printer-send-queue/{id}{suffix}`, or null. */
const itemId = (path: string, suffix: string) =>
  new RegExp(`^/api/v2/printer-send-queue/([^/]+)${suffix}$`).exec(path)?.[1] ?? null;

/** Add: queues the form's file as a `queued` item. */
const addRoute =
  (queue: Item[], next: () => string): Route =>
  (path, init) => {
    if (path !== "/api/v2/printer-send-queue" || init?.method !== "POST") return undefined;
    const form = init.body as FormData;
    const added = item(next(), {
      printer_id: String(form.get("printer_id")),
      start: form.get("start") === "true",
      wait_for_idle: form.get("wait_for_idle") === "true",
      filename: (form.get("file") as File).name,
    });
    queue.push(added);
    return json({ item: added });
  };

/** Send now: the item becomes `sending`, then `done` at the next queue read. */
const sendNowRoute =
  (queue: Item[], dispatch?: (id: string) => Answer): Route =>
  (path, init) => {
    const id = itemId(path, "/dispatch");
    if (!id || init?.method !== "POST") return undefined;
    if (dispatch) return dispatch(id);
    const found = queue.find((i) => i.id === id)!;
    found.state = "sending";
    found.upload_job_id = "job-1";
    return json({ item: found, job_id: "job-1" });
  };

const removeRoute =
  (queue: Item[]): Route =>
  (path, init) => {
    const id = itemId(path, "");
    if (!id || init?.method !== "DELETE") return undefined;
    const found = queue.find((i) => i.id === id)!;
    found.state = "cancelled";
    return json({ item: found });
  };

/** Reconcile: an answer for another send of the item is 409 `send_changed`. */
const reconcileRoute =
  (queue: Item[], reconcile?: (id: string) => Answer): Route =>
  (path, init) => {
    const id = itemId(path, "/reconcile");
    if (!id || init?.method !== "POST") return undefined;
    if (reconcile) return reconcile(id);
    const found = queue.find((i) => i.id === id)!;
    const body = JSON.parse(String(init.body)) as { printed: boolean; job_id: string | null };
    if (body.job_id !== found.upload_job_id) return problem(409, "send_changed");
    found.outcome_unknown = false;
    found.state = body.printed ? "done" : "error";
    found.error = body.printed ? null : "You reconciled this send: the printer did not print it.";
    return json({ item: found });
  };

/**
 * A fake core with a send queue that records every request. Add queues an item; Send now
 * marks it `sending` and the next queue read `done`, unless `dispatch` answers otherwise.
 */
export function fakeCore({
  farm = "on",
  printers = [printer("p1", "Micron", READY)],
  items = [] as Item[],
  dispatch,
  reconcile,
}: {
  farm?: "on" | "off";
  printers?: Printer[];
  items?: Item[];
  dispatch?: (id: string) => Answer;
  reconcile?: (id: string) => Answer;
} = {}) {
  const queue = items.map((i) => ({ ...i }));
  let next = 1;
  const writeRoutes = [
    addRoute(queue, () => `q${next++}`),
    sendNowRoute(queue, dispatch),
    removeRoute(queue),
    reconcileRoute(queue, reconcile),
  ];
  const fetchMock = vi.fn(async (path: string, init?: RequestInit) => {
    if (path === "/health") return json(health(farm));
    if (path === "/api/v2/printers") return json({ printers });
    if (path === "/api/v2/printer-send-queue" && !init?.method) {
      const answer = { items: queue.map((i) => ({ ...i })) };
      for (const i of queue) if (i.state === "sending") i.state = "done";
      return json(answer);
    }
    for (const route of writeRoutes) {
      const answer = route(path, init);
      if (answer) return answer;
    }
    return json(
      { type: "urn:print-partner:problem:not-found", title: "Not Found", status: 404 },
      404,
    );
  });
  return { fetchMock, queue };
}

export function renderAt(fetchMock: ReturnType<typeof fakeCore>["fetchMock"]) {
  vi.stubGlobal("fetch", fetchMock);
  const router = createMemoryRouter(routes, { initialEntries: ["/builds/2/production"] });
  const queryClient = createQueryClient();
  queryClient.setDefaultOptions({
    queries: { ...queryClient.getDefaultOptions().queries, retryDelay: 0 },
  });
  render(<App router={router} queryClient={queryClient} />);
}

export const writes = (fetchMock: ReturnType<typeof fakeCore>["fetchMock"]) =>
  fetchMock.mock.calls.filter(([, init]) => init?.method && init.method !== "GET");

export async function openSheet(file = "plate.gcode", printerName = "Micron") {
  fireEvent.click(await screen.findByRole("button", { name: "Send a file" }));
  const sheet = await screen.findByRole("dialog", { name: "Send a file" });
  fireEvent.change(within(sheet).getByLabelText("Sliced file"), {
    target: { files: [new File(["G28\n"], file)] },
  });
  const select = within(sheet).getByLabelText<HTMLSelectElement>("Printer");
  await within(sheet).findByRole("option", { name: printerName });
  const option = within(sheet).getByRole<HTMLOptionElement>("option", { name: printerName });
  fireEvent.change(select, { target: { value: option.value } });
  return sheet;
}
