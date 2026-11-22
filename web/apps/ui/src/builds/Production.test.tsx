import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { cleanup } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { OFFLINE_REASON } from "../frame/connection";
import {
  BED,
  fakeCore,
  item,
  json,
  NO_UNITS,
  openSheet,
  part,
  printer,
  PRINTING,
  renderAt,
  UNREACHABLE,
  writes,
  type State,
} from "./sendQueueFake.test-utils";

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

  it.each([
    ["needs clearing", BED],
    [
      "is not confirmed clear",
      { execution: "idle", bed_clear: "unknown", display: "bed_to_clear" } as State,
    ],
  ])(
    "an idle printer whose bed %s can start, and the dialog says it needs clearing first",
    async (_, state) => {
      renderAt(fakeCore({ printers: [printer("p1", "Micron", state)] }).fetchMock);
      const sheet = await openSheet();
      fireEvent.click(within(sheet).getByRole("radio", { name: "Upload and start print" }));
      fireEvent.click(within(sheet).getByRole("button", { name: "Send" }));
      const confirm = await screen.findByRole("dialog", { name: "Start this print?" });
      const lines = within(confirm).getAllByText(/./, { selector: "p" });
      expect(lines[0]!.textContent).toMatch(/bed needs clearing/);
    },
  );

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
    expect(await screen.findByText(/could not confirm what the core did/)).toBeTruthy();
    expect(writes(fetchMock)).toHaveLength(2);
  });

  it.each([
    ["Add", "add"],
    ["Send now", "dispatch"],
  ])(
    "a 5xx from %s is uncertain: no Not sent claim, the queue is re-read, nothing repeats",
    async (_, which) => {
      const failure = () =>
        json(
          {
            type: "urn:print-partner:problem:internal",
            title: "Internal Server Error",
            status: 500,
          },
          500,
        );
      const { fetchMock } = fakeCore(which === "dispatch" ? { dispatch: failure } : {});
      if (which === "add") {
        const answer = fetchMock.getMockImplementation()!;
        fetchMock.mockImplementation((path: string, init?: RequestInit) =>
          path === "/api/v2/printer-send-queue" && init?.method === "POST"
            ? Promise.resolve(failure())
            : answer(path, init),
        );
      }
      renderAt(fetchMock);
      const sheet = await openSheet();
      fireEvent.click(within(sheet).getByRole("button", { name: "Send" }));
      expect(await screen.findByText(/could not confirm what the core did/)).toBeTruthy();
      expect(screen.queryByText(/^Not sent:/)).toBeNull();
      expect(writes(fetchMock)).toHaveLength(which === "dispatch" ? 2 : 1);
    },
  );

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

describe("Send a file links the Checkoff units the file prints (GRE-1896)", () => {
  const ORCA = [
    "EXCLUDE_OBJECT_DEFINE NAME=bracket.stl_id_0_copy_0 CENTER=10,10",
    "EXCLUDE_OBJECT_DEFINE NAME=bracket.stl_id_0_copy_1 CENTER=30,10",
    "G28",
  ].join("\n");
  const sentUnits = (fetchMock: ReturnType<typeof fakeCore>["fetchMock"]) =>
    (writes(fetchMock)[0]![1]!.body as FormData).get("checkoff_units");

  it("matches the file's objects to units still to print and sends them with Add", async () => {
    const { fetchMock } = fakeCore({
      checkoff: { ...NO_UNITS, parts: [part(11, "bracket.stl", 3, 1), part(12, "lid.stl", 1)] },
    });
    renderAt(fetchMock);
    const sheet = await openSheet("plate.gcode", "Micron", ORCA);
    const units = within(sheet).getByRole("group", { name: "Checkoff units" });
    expect(within(units).getByRole("listitem").textContent).toBe("bracket.stl × 2");
    expect(writes(fetchMock)).toEqual([]);
    fireEvent.click(within(sheet).getByRole("button", { name: "Send" }));
    await waitFor(() => expect(writes(fetchMock)).toHaveLength(2));
    expect(JSON.parse(String(sentUnits(fetchMock)))).toEqual([
      { part_id: 11, unit_index: 1, object_name: "bracket.stl_id_0_copy_0" },
      { part_id: 11, unit_index: 2, object_name: "bracket.stl_id_0_copy_1" },
    ]);
  });

  it("says a file without matching objects links no units, and sends without them", async () => {
    const { fetchMock } = fakeCore({ checkoff: { ...NO_UNITS, parts: [part(12, "lid.stl", 1)] } });
    renderAt(fetchMock);
    const sheet = await openSheet("plate.gcode", "Micron", ORCA);
    const units = within(sheet).getByRole("group", { name: "Checkoff units" });
    expect(units.textContent).toContain("records no filament grams");
    expect(units.textContent).toContain(
      "Not matched to a unit: bracket.stl_id_0_copy_0, bracket.stl_id_0_copy_1",
    );
    fireEvent.click(within(sheet).getByRole("button", { name: "Send" }));
    await waitFor(() => expect(writes(fetchMock)).toHaveLength(2));
    expect(sentUnits(fetchMock)).toBeNull();
  });

  it("when the Build's units cannot be read, says so and still sends, without units", async () => {
    const { fetchMock } = fakeCore({ checkoff: json({ status: 500, title: "Down" }, 500) });
    renderAt(fetchMock);
    const sheet = await openSheet("plate.gcode", "Micron", ORCA);
    await waitFor(() =>
      expect(within(sheet).getByRole("alert").textContent).toMatch(
        /Could not read this Build's Checkoff units/,
      ),
    );
    fireEvent.click(within(sheet).getByRole("button", { name: "Send" }));
    await waitFor(() => expect(writes(fetchMock)).toHaveLength(2));
    expect(sentUnits(fetchMock)).toBeNull();
  });
});
