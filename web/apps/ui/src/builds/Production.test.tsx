import type { FarmPrinterList, Health, SendQueue } from "@print-partner/rust-client";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { createMemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App, createQueryClient } from "../App";
import { OFFLINE_REASON } from "../frame/connection";
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
  modules: [
    farm === "on"
      ? { module: "farm", state: "on" }
      : { module: "farm", state: "off", reason: "default_off" },
  ],
});

type Printer = FarmPrinterList["printers"][number];
type State = Printer["state"];
const printer = (id: string, name: string, state: State): Printer => ({
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
const READY: State = { execution: "idle", bed_clear: "clear", display: "ready" };
const BED: State = { execution: "idle", bed_clear: "needs_clearing", display: "bed_to_clear" };
const PRINTING: State = { execution: "printing", bed_clear: "needs_clearing", display: "printing" };
const UNREACHABLE: State = {
  execution: "unreachable",
  bed_clear: "unknown",
  display: "unreachable",
};

type Item = SendQueue["items"][number];
const item = (id: string, overrides: Partial<Item> = {}): Item => ({
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

/**
 * A fake core with a send queue. Add queues an item; Send now marks it `sending` and the
 * next queue read `done`, unless `dispatch` answers otherwise.
 */
function fakeCore({
  farm = "on",
  printers = [printer("p1", "Micron", READY)],
  items = [] as Item[],
  dispatch,
}: {
  farm?: "on" | "off";
  printers?: Printer[];
  items?: Item[];
  dispatch?: (id: string) => Response | Promise<Response>;
} = {}) {
  const queue = [...items];
  let next = 1;
  const fetchMock = vi.fn(async (path: string, init?: RequestInit) => {
    if (path === "/health") return json(health(farm));
    if (path === "/api/v2/printers") return json({ printers });
    if (path === "/api/v2/printer-send-queue" && !init?.method) {
      const answer = { items: queue.map((i) => ({ ...i })) };
      for (const i of queue) if (i.state === "sending") i.state = "done";
      return json(answer);
    }
    if (path === "/api/v2/printer-send-queue" && init?.method === "POST") {
      const form = init.body as FormData;
      const added = item(`q${next++}`, {
        printer_id: String(form.get("printer_id")),
        start: form.get("start") === "true",
        wait_for_idle: form.get("wait_for_idle") === "true",
        filename: (form.get("file") as File).name,
      });
      queue.push(added);
      return json({ item: added });
    }
    const sendNow = /^\/api\/v2\/printer-send-queue\/([^/]+)\/dispatch$/.exec(path);
    if (sendNow && init?.method === "POST") {
      if (dispatch) return dispatch(sendNow[1]!);
      const found = queue.find((i) => i.id === sendNow[1])!;
      found.state = "sending";
      found.upload_job_id = "job-1";
      return json({ item: found, job_id: "job-1" });
    }
    return json(
      { type: "urn:print-partner:problem:not-found", title: "Not Found", status: 404 },
      404,
    );
  });
  return { fetchMock, queue };
}

function renderAt(fetchMock: ReturnType<typeof fakeCore>["fetchMock"]) {
  vi.stubGlobal("fetch", fetchMock);
  const router = createMemoryRouter(routes, { initialEntries: ["/builds/2/production"] });
  const queryClient = createQueryClient();
  queryClient.setDefaultOptions({
    queries: { ...queryClient.getDefaultOptions().queries, retryDelay: 0 },
  });
  render(<App router={router} queryClient={queryClient} />);
}

const writes = (fetchMock: ReturnType<typeof fakeCore>["fetchMock"]) =>
  fetchMock.mock.calls.filter(([, init]) => init?.method && init.method !== "GET");

async function openSheet(file = "plate.gcode", printerName = "Micron") {
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

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe("Production tab", () => {
  it("lists only this Build's sends with their states, and offers Send a file", async () => {
    const { fetchMock } = fakeCore({
      items: [
        item("a", { state: "done", filename: "done.gcode" }),
        item("b", {
          state: "error",
          error: "Upload failed (HTTP 500): Disk full",
          filename: "failed.gcode",
        }),
        item("c", {
          state: "error",
          outcome_unknown: true,
          upload_job_id: "j",
          filename: "lost.gcode",
        }),
        item("d", { state: "cancelled", filename: "removed.gcode" }),
        item("e", { profile_id: 9, filename: "other.gcode" }),
      ],
    });
    renderAt(fetchMock);
    const table = await screen.findByRole("table", { name: "Sends" });
    const text = table.textContent ?? "";
    expect(text).toContain("done.gcode");
    expect(text).toContain("Sent");
    expect(text).toContain("Upload failed (HTTP 500): Disk full");
    expect(text).toContain("Outcome unknown");
    expect(text).not.toContain("removed.gcode");
    expect(text).not.toContain("other.gcode");
    expect(screen.getByRole("button", { name: "Send a file" })).toBeTruthy();
    expect(writes(fetchMock)).toEqual([]);
  });

  it("says when the Build has no sends", async () => {
    renderAt(fakeCore().fetchMock);
    expect(await screen.findByText("No sends for this Build yet.")).toBeTruthy();
    expect(screen.queryByText(/arrives in a later slice/)).toBeNull();
  });

  it("opening the sheet and choosing a file and a printer sends nothing", async () => {
    const { fetchMock } = fakeCore();
    renderAt(fetchMock);
    const sheet = await openSheet();
    expect(
      within(sheet).getByRole<HTMLInputElement>("radio", { name: "Upload only" }).checked,
    ).toBe(true);
    expect(writes(fetchMock)).toEqual([]);
  });

  it("Upload only adds with start off, sends once, and the row goes from Sending to Sent", async () => {
    const { fetchMock } = fakeCore();
    renderAt(fetchMock);
    const sheet = await openSheet();
    fireEvent.click(within(sheet).getByRole("button", { name: "Send" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    const [add, dispatch, ...rest] = writes(fetchMock);
    expect(rest).toEqual([]);
    const form = add![1]!.body as FormData;
    expect([form.get("start"), form.get("wait_for_idle"), form.get("profile_id")]).toEqual([
      "false",
      "false",
      "2",
    ]);
    expect(dispatch![0]).toBe("/api/v2/printer-send-queue/q1/dispatch");
    expect(dispatch![1]!.body).toBeUndefined();
    const table = await screen.findByRole("table", { name: "Sends" });
    await waitFor(() => expect(table.textContent).toContain("Sent"), { timeout: 8_000 });
    expect(writes(fetchMock)).toHaveLength(2);
  }, 15_000);

  it("Upload and start print asks first; Cancel sends nothing, Start print sends once", async () => {
    const { fetchMock } = fakeCore();
    renderAt(fetchMock);
    let sheet = await openSheet();
    fireEvent.click(within(sheet).getByRole("radio", { name: "Upload and start print" }));
    fireEvent.click(within(sheet).getByRole("button", { name: "Send" }));
    let confirm = await screen.findByRole("dialog", { name: "Start this print?" });
    expect(confirm.textContent).toContain("plate.gcode");
    expect(confirm.textContent).toContain("Micron");
    fireEvent.click(within(confirm).getByRole("button", { name: "Cancel" }));
    sheet = await screen.findByRole("dialog", { name: "Send a file" });
    expect(writes(fetchMock)).toEqual([]);
    fireEvent.click(within(sheet).getByRole("button", { name: "Send" }));
    confirm = await screen.findByRole("dialog", { name: "Start this print?" });
    fireEvent.click(within(confirm).getByRole("button", { name: "Start print" }));
    await waitFor(() => expect(writes(fetchMock)).toHaveLength(2));
    const form = writes(fetchMock)[0]![1]!.body as FormData;
    expect([form.get("start"), form.get("wait_for_idle")]).toEqual(["true", "true"]);
    expect(JSON.stringify(fetchMock.mock.calls)).not.toContain("force");
  });

  it.each([
    ["Printing", PRINTING, /printing/],
    ["Unreachable", UNREACHABLE, /unreachable/],
  ])("a %s printer cannot start a print and says why", async (_, state, why) => {
    renderAt(fakeCore({ printers: [printer("p1", "Micron", state)] }).fetchMock);
    const sheet = await openSheet();
    const start = within(sheet).getByRole<HTMLInputElement>("radio", {
      name: "Upload and start print",
    });
    expect(start.disabled).toBe(true);
    expect(within(sheet).getByText(/Upload and start print is not available/).textContent).toMatch(
      why,
    );
  });

  it("a finished print whose bed needs clearing can start, and the dialog says so first", async () => {
    renderAt(fakeCore({ printers: [printer("p1", "Micron", BED)] }).fetchMock);
    const sheet = await openSheet();
    fireEvent.click(within(sheet).getByRole("radio", { name: "Upload and start print" }));
    fireEvent.click(within(sheet).getByRole("button", { name: "Send" }));
    const confirm = await screen.findByRole("dialog", { name: "Start this print?" });
    const lines = within(confirm).getAllByText(/./, { selector: "p" });
    expect(lines[0]!.textContent).toMatch(/bed needs clearing/);
  });

  it("a printer busy by Start print leaves a Not sent row and shows the core's reason", async () => {
    const detail = "Printer is printing — wait for Idle or force dispatch";
    const { fetchMock } = fakeCore({
      dispatch: () =>
        json(
          { type: "urn:print-partner:problem:conflict", title: "Conflict", status: 409, detail },
          409,
        ),
    });
    renderAt(fetchMock);
    const sheet = await openSheet();
    fireEvent.click(within(sheet).getByRole("radio", { name: "Upload and start print" }));
    fireEvent.click(within(sheet).getByRole("button", { name: "Send" }));
    fireEvent.click(
      within(await screen.findByRole("dialog", { name: "Start this print?" })).getByRole("button", {
        name: "Start print",
      }),
    );
    expect(await screen.findByText(`Not sent: ${detail}`)).toBeTruthy();
    const table = await screen.findByRole("table", { name: "Sends" });
    await waitFor(() => expect(table.textContent).toContain("Not sent"));
    expect(writes(fetchMock)).toHaveLength(2);
  });

  it("a lost Send now answer re-reads the queue and sends nothing again", async () => {
    const { fetchMock } = fakeCore({
      dispatch: () => Promise.reject(new TypeError("network down")),
    });
    renderAt(fetchMock);
    const sheet = await openSheet();
    fireEvent.click(within(sheet).getByRole("button", { name: "Send" }));
    expect(await screen.findByText(/answer was lost/)).toBeTruthy();
    expect(writes(fetchMock)).toHaveLength(2);
  });

  it("refuses a .stl file in the sheet without a request", async () => {
    const { fetchMock } = fakeCore();
    renderAt(fetchMock);
    const sheet = await openSheet("part.stl");
    expect(within(sheet).getByRole("alert").textContent).toMatch(/Only \.gcode/);
    fireEvent.click(within(sheet).getByRole("button", { name: "Send" }));
    expect(writes(fetchMock)).toEqual([]);
  });

  it("with Farm off shows the Module-off state and no sheet", async () => {
    renderAt(fakeCore({ farm: "off" }).fetchMock);
    expect(await screen.findByText("The Farm module is off.")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Send a file" })).toBeNull();
  });

  it("offline, Send says why it is disabled", async () => {
    const { fetchMock } = fakeCore();
    const online = fetchMock.getMockImplementation()!;
    let down = false;
    fetchMock.mockImplementation((path: string, init?: RequestInit) =>
      down ? Promise.reject(new TypeError("Failed to fetch")) : online(path, init),
    );
    vi.useFakeTimers({ shouldAdvanceTime: true });
    renderAt(fetchMock);
    const sheet = await openSheet();
    down = true;
    await act(() => vi.advanceTimersByTimeAsync(31_000));
    await waitFor(() => expect(within(sheet).queryByText(OFFLINE_REASON)).toBeTruthy());
    expect(within(sheet).getByRole("button", { name: "Send" }).getAttribute("aria-disabled")).toBe(
      "true",
    );
  }, 15_000);
});
