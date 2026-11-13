import type { FarmPrinterList, Health } from "@print-partner/rust-client";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { createMemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App, createQueryClient } from "../App";
import { routes } from "../routes";

type Printer = FarmPrinterList["printers"][number];
type Answer = (body: unknown) => Response | Promise<Response>;

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
const problem = (status: number, detail: string) => json({ status, detail }, status);

const HEALTH: Health = {
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
  modules: [{ module: "farm", state: "on" }],
};

const printer = (id: string, name: string, integration: Printer["integration"]): Printer => ({
  id,
  name,
  model: "Voron 2.4",
  integration,
  state: { execution: "idle", bed_clear: "clear", display: "ready" },
  reason: null,
  last_heard_at: null,
  progress_percent: null,
  loaded_filaments: [],
});
const VORON = printer("p-1", "Voron", { id: "i-1", kind: "moonraker" });
const VIEW = { id: "i-9", kind: "moonraker", name: "Micron", enabled: true, base_url: "http://micron.test", username: null, has_api_key: true, has_password: false };
const CONFIG = { id: "p-9", name: "Micron", model: "Micron+", bed_width_mm: 180, bed_depth_mm: 180, bed_height_mm: 160, margin_mm: 4, max_filament_slots: 1, integration_id: "i-9" };
const PROBE = { kind: "moonraker", reachable: true, credentials_needed: false, reason: "Printer responded successfully.", capabilities: ["status"] };
const LIST = "GET /api/v2/printers";

/** A fake core keyed by "METHOD path". It records every write as `[key, body]`. */
function renderPrinters(answers: Record<string, Answer>) {
  const writes: [string, unknown][] = [];
  const fetchMock = vi.fn(async (path: string, init?: RequestInit) => {
    const key = `${init?.method ?? "GET"} ${path}`;
    const body: unknown = init?.body ? JSON.parse(init.body as string) : undefined;
    if (key !== "GET /health" && key !== LIST) writes.push([key, body]);
    if (key === "GET /health") return json(HEALTH);
    return (answers[key] ?? (() => problem(500, `no answer for ${key}`)))(body);
  });
  vi.stubGlobal("fetch", fetchMock);
  const queryClient = createQueryClient();
  queryClient.setDefaultOptions({ queries: { ...queryClient.getDefaultOptions().queries, retry: false } });
  render(<App router={createMemoryRouter(routes, { initialEntries: ["/settings"] })} queryClient={queryClient} />);
  return writes;
}

const section = () => screen.findByRole("region", { name: "Printers" });
const type = (label: string | RegExp, value: string) =>
  fireEvent.change(screen.getByLabelText(label), { target: { value } });

/** Opens the add form and fills it for a Moonraker printer at `address`. */
async function fillAdd(address: string) {
  const add = await within(await section()).findByRole("button", { name: "Add a printer" });
  await waitFor(() => expect(add.getAttribute("aria-disabled")).toBeNull());
  fireEvent.click(add);
  type("Address", address);
  type("API key (optional)", "moon-key-canary");
  type("Name", "Micron");
  type("Model", "Micron+");
  for (const [side, mm] of [["width", "180"], ["depth", "180"], ["height", "160"]]) type(`Bed ${side} (mm)`, mm);
}
const press = (name: string) => fireEvent.click(screen.getByRole("button", { name }));

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("Printers section states", () => {
  it("shows loading, then the empty state with the add control", async () => {
    let answer: (response: Response) => void = () => {};
    renderPrinters({ [LIST]: () => new Promise<Response>((resolve) => (answer = resolve)) });
    expect(within(await section()).getByText("Loading printers.")).toBeTruthy();
    answer(json({ printers: [] }));
    expect(await screen.findByText("No printers yet. Add one to see it on Farm.")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Add a printer" })).toBeTruthy();
  });

  it("lists each printer with its model and connection", async () => {
    renderPrinters({ [LIST]: () => json({ printers: [VORON, printer("p-2", "Annex", null)] }) });
    const rows = within(await section());
    await rows.findByText("Voron");
    expect(rows.getAllByRole("term").map((dt) => dt.textContent)).toEqual(["Voron", "Annex"]);
    expect(rows.getByText("Moonraker")).toBeTruthy();
    expect(rows.getByText("No connection")).toBeTruthy();
  });

  it("names a failed read and reads again on request, keeping the last list when a refresh fails", async () => {
    let failing = true;
    renderPrinters({ [LIST]: () => (failing ? problem(503, "Service unavailable") : json({ printers: [VORON] })) });
    const alert = await within(await section()).findByRole("alert");
    expect(alert.textContent).toContain("Could not load the printers: Service unavailable");
    failing = false;
    press("Reload printers");
    await screen.findByText("Voron");
    // A later refresh fails: the list stays, marked as possibly out of date.
    failing = true;
    press("Delete Voron");
    press("Cancel");
    const stale = await within(await section()).findByRole("alert");
    expect(stale.textContent).toContain("this list may be out of date");
    expect(screen.getByText("Voron")).toBeTruthy();
  });
});

describe("Adding a printer", () => {
  it("saves only after a check, as an integration and then a printer that links it", async () => {
    let printers: Printer[] = [];
    const writes = renderPrinters({
      [LIST]: () => json({ printers }),
      "POST /api/v2/printers/probe": () => json(PROBE),
      "POST /api/v2/integrations": () => json(VIEW),
      "POST /api/v2/printers": () => {
        printers = [printer("p-9", "Micron", { id: "i-9", kind: "moonraker" })];
        return json(CONFIG);
      },
    });
    await fillAdd("http://micron.test");
    // Nothing is saved before the address was checked.
    press("Save printer");
    expect(screen.getByText("Check the address before saving.")).toBeTruthy();
    expect(writes).toEqual([]);
    press("Check address");
    expect(await screen.findByText("The printer answered.")).toBeTruthy();
    press("Save printer");
    expect(await screen.findByText("Added Micron.")).toBeTruthy();
    expect(writes).toEqual([
      ["POST /api/v2/printers/probe", { kind: "moonraker", address: "http://micron.test" }],
      ["POST /api/v2/integrations", { kind: "moonraker", name: "Micron", base_url: "http://micron.test", api_key: "moon-key-canary" }],
      ["POST /api/v2/printers", { name: "Micron", model: "Micron+", bed_width_mm: 180, bed_depth_mm: 180, bed_height_mm: 160, integration_id: "i-9" }],
    ]);
    // The list was read again, and the key is nowhere on the page.
    expect(within(await section()).getByText("Micron")).toBeTruthy();
    expect(document.body.textContent).not.toContain("moon-key-canary");
  });

  it("shows why a check failed and offers to save anyway", async () => {
    const writes = renderPrinters({
      [LIST]: () => json({ printers: [] }),
      "POST /api/v2/printers/probe": () => json({ ...PROBE, kind: null, reachable: false, reason: "Printer probe timed out." }),
      "POST /api/v2/integrations": () => json(VIEW),
      "POST /api/v2/printers": () => json(CONFIG),
    });
    await fillAdd("http://micron.test");
    press("Check address");
    expect(await screen.findByText(/^Printer probe timed out\. You can save it anyway/)).toBeTruthy();
    expect(writes).toHaveLength(1);
    // A changed address needs its own check.
    type("Address", "http://other.test");
    expect(screen.getByText("Check the address before saving.")).toBeTruthy();
    type("Address", "http://micron.test");
    press("Save anyway");
    expect(await screen.findByText("Added Micron.")).toBeTruthy();
    expect(writes.map(([key]) => key)).toEqual(["POST /api/v2/printers/probe", "POST /api/v2/integrations", "POST /api/v2/printers"]);
  });

  it("keeps a saved connection when the printer write fails, then saves only the printer or discards", async () => {
    const writes = renderPrinters({
      [LIST]: () => json({ printers: [] }),
      "POST /api/v2/printers/probe": () => json(PROBE),
      "POST /api/v2/integrations": () => json(VIEW),
      "POST /api/v2/printers": () => problem(409, "The printer limit is reached. Delete one first."),
      "DELETE /api/v2/integrations/i-9": () => json(VIEW),
    });
    await fillAdd("http://micron.test");
    press("Check address");
    await screen.findByText("The printer answered.");
    press("Save printer");
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("The connection was saved, but the printer was not: The printer limit is reached.");
    // Nothing was resent, and Escape does not drop the saved connection.
    expect(writes.map(([key]) => key).slice(1)).toEqual(["POST /api/v2/integrations", "POST /api/v2/printers"]);
    fireEvent.keyDown(document.activeElement ?? document.body, { key: "Escape" });
    expect(screen.getByRole("dialog", { name: "Add a printer" })).toBeTruthy();

    // "Save printer" sends the printer again, not the integration.
    press("Save printer");
    await waitFor(() => expect(writes).toHaveLength(4));
    expect(writes[3][0]).toBe("POST /api/v2/printers");
    // "Discard" deletes the saved integration and closes.
    press("Discard");
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(writes[4]).toEqual(["DELETE /api/v2/integrations/i-9", undefined]);
  });
});

describe("Deleting a printer", () => {
  it("asks first, then deletes the printer and its connection", async () => {
    let printers = [VORON];
    const writes = renderPrinters({
      [LIST]: () => json({ printers }),
      "DELETE /api/v2/printers/p-1": () => {
        printers = [];
        return json({ ...CONFIG, id: "p-1" });
      },
      "DELETE /api/v2/integrations/i-1": () => json({ ...VIEW, id: "i-1" }),
    });
    fireEvent.click(await screen.findByRole("button", { name: "Delete Voron" }));
    const dialog = screen.getByRole("dialog", { name: "Delete Voron?" });
    expect(within(dialog).getByText(/deletes the printer and its Moonraker connection/)).toBeTruthy();
    expect(writes).toEqual([]);
    press("Delete printer");
    expect(await screen.findByText("Deleted Voron.")).toBeTruthy();
    expect(writes.map(([key]) => key)).toEqual(["DELETE /api/v2/printers/p-1", "DELETE /api/v2/integrations/i-1"]);
    expect(await screen.findByText("No printers yet. Add one to see it on Farm.")).toBeTruthy();
  });

  it("leaves a connection another printer uses", async () => {
    const writes = renderPrinters({
      [LIST]: () => json({ printers: [VORON, printer("p-2", "Twin", VORON.integration)] }),
      "DELETE /api/v2/printers/p-1": () => json({ ...CONFIG, id: "p-1" }),
    });
    fireEvent.click(await screen.findByRole("button", { name: "Delete Voron" }));
    expect(screen.getByText("This deletes the printer. Its connection is used by another printer and stays.")).toBeTruthy();
    press("Delete printer");
    expect(await screen.findByText("Deleted Voron. Its connection is used by another printer and was kept.")).toBeTruthy();
    expect(writes.map(([key]) => key)).toEqual(["DELETE /api/v2/printers/p-1"]);
  });

  it("reads the list again before deleting a connection, and keeps one a printer added elsewhere uses", async () => {
    // The shown list has only Voron; another window has since added Twin on the same connection.
    let printers = [VORON];
    const writes = renderPrinters({
      [LIST]: () => json({ printers }),
      "DELETE /api/v2/printers/p-1": () => {
        printers = [printer("p-2", "Twin", VORON.integration)];
        return json({ ...CONFIG, id: "p-1" });
      },
    });
    fireEvent.click(await screen.findByRole("button", { name: "Delete Voron" }));
    press("Delete printer");
    expect(await screen.findByText("Deleted Voron. Its connection is used by another printer and was kept.")).toBeTruthy();
    expect(writes.map(([key]) => key)).toEqual(["DELETE /api/v2/printers/p-1"]);
  });

  it("keeps the connection when the list cannot be read to check it", async () => {
    let readable = true;
    const writes = renderPrinters({
      [LIST]: () => (readable ? json({ printers: [VORON] }) : problem(503, "Service unavailable")),
      "DELETE /api/v2/printers/p-1": () => {
        readable = false;
        return json({ ...CONFIG, id: "p-1" });
      },
    });
    fireEvent.click(await screen.findByRole("button", { name: "Delete Voron" }));
    press("Delete printer");
    expect(await screen.findByText(/^Deleted Voron\. Its connection was kept, because the printers could not be read/)).toBeTruthy();
    expect(writes.map(([key]) => key)).toEqual(["DELETE /api/v2/printers/p-1"]);
  });

  it("says the printer is gone when its connection could not be deleted, and offers only that write again", async () => {
    const writes = renderPrinters({
      [LIST]: () => json({ printers: [VORON] }),
      "DELETE /api/v2/printers/p-1": () => json({ ...CONFIG, id: "p-1" }),
      "DELETE /api/v2/integrations/i-1": () => problem(503, "Service unavailable"),
    });
    fireEvent.click(await screen.findByRole("button", { name: "Delete Voron" }));
    press("Delete printer");
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("The printer was deleted, but its connection was not: Service unavailable.");
    press("Delete connection");
    await waitFor(() => expect(writes).toHaveLength(3));
    expect(writes.map(([key]) => key)).toEqual([
      "DELETE /api/v2/printers/p-1",
      "DELETE /api/v2/integrations/i-1",
      "DELETE /api/v2/integrations/i-1",
    ]);
  });
});
