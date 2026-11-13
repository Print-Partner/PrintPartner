import { cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { BED, fakeCore, item, json, printer, problem, renderAt, writes } from "./sendQueueFake";

// GRE-1524: row actions. "The simulator" in the acceptance is the fake core's request log:
// a dispatch is the only request that would reach a printer.
const dispatches = (fetchMock: ReturnType<typeof fakeCore>["fetchMock"]) =>
  writes(fetchMock).filter(([path]) => String(path).endsWith("/dispatch"));

const row = async (filename: string) =>
  within(await screen.findByRole("group", { name: `Actions for ${filename}` }));

const UNKNOWN = item("lost", {
  filename: "lost.gcode",
  state: "error",
  outcome_unknown: true,
  upload_job_id: "job-7",
  error: "Printer outcome is unknown.",
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("Production row actions", () => {
  it("a Not sent row offers Send now and Remove; Remove takes it away without sending", async () => {
    const { fetchMock } = fakeCore({ items: [item("q", { filename: "waiting.gcode" })] });
    renderAt(fetchMock);
    const actions = await row("waiting.gcode");
    expect(actions.getAllByRole("button").map((b) => b.textContent)).toEqual([
      "Send now",
      "Remove",
    ]);
    fireEvent.click(actions.getByRole("button", { name: "Remove" }));
    await waitFor(() => expect(screen.queryByText("waiting.gcode")).toBeNull());
    expect(writes(fetchMock).map(([path, init]) => [path, init?.method])).toEqual([
      ["/api/v2/printer-send-queue/q", "DELETE"],
    ]);
    expect(dispatches(fetchMock)).toEqual([]);
  });

  it("a Failed row sends again only when Send now is pressed", async () => {
    const failed = item("f", { filename: "failed.gcode", state: "error", error: "Disk full" });
    const { fetchMock } = fakeCore({ items: [failed] });
    renderAt(fetchMock);
    const actions = await row("failed.gcode");
    expect(dispatches(fetchMock)).toEqual([]);
    fireEvent.click(actions.getByRole("button", { name: "Send now" }));
    await waitFor(() => expect(dispatches(fetchMock)).toHaveLength(1));
    expect(dispatches(fetchMock)[0]![1]!.body).toBeUndefined();
  });

  it("an Outcome unknown row offers the two answers and Remove, and no Send now", async () => {
    renderAt(fakeCore({ items: [UNKNOWN] }).fetchMock);
    const actions = await row("lost.gcode");
    const labels = actions.getAllByRole("button").map((b) => b.textContent);
    expect(labels).toEqual(["It printed", "It did not print", "Remove"]);
  });

  it.each([
    ["It did not print", false, "Failed", ["Send now", "Remove"]],
    ["It printed", true, "Sent", []],
  ])(
    "%s sends the item's job and reads back as %s, with no printer request",
    async (answer, printed, state, after) => {
      const { fetchMock } = fakeCore({ items: [UNKNOWN] });
      renderAt(fetchMock);
      fireEvent.click((await row("lost.gcode")).getByRole("button", { name: answer as string }));
      const table = await screen.findByRole("table", { name: "Sends" });
      await waitFor(() => expect(table.textContent).toContain(state));
      const [[path, init]] = writes(fetchMock) as [[string, RequestInit]];
      expect(path).toBe("/api/v2/printer-send-queue/lost/reconcile");
      expect(JSON.parse(String(init.body))).toEqual({ printed, job_id: "job-7" });
      const buttons = screen.queryByRole("group", { name: "Actions for lost.gcode" });
      expect(
        buttons
          ? within(buttons)
              .getAllByRole("button")
              .map((b) => b.textContent)
          : [],
      ).toEqual(after);
      expect(dispatches(fetchMock)).toEqual([]);
    },
  );

  it("an answer for a send that has changed is refused; the row re-reads and says why", async () => {
    const { fetchMock, queue } = fakeCore({
      items: [UNKNOWN],
      reconcile: () => {
        queue[0]!.state = "done";
        queue[0]!.outcome_unknown = false;
        return problem(409, "send_changed", "This answer is for an earlier send of this item");
      },
    });
    renderAt(fetchMock);
    fireEvent.click((await row("lost.gcode")).getByRole("button", { name: "It did not print" }));
    expect(await screen.findByText("This answer is for an earlier send of this item")).toBeTruthy();
    const table = screen.getByRole("table", { name: "Sends" });
    await waitFor(() => expect(table.textContent).toContain("Sent"));
    expect(dispatches(fetchMock)).toEqual([]);
  });

  it("neither answer is the default: no primary style, no focus, and Enter records nothing", async () => {
    const { fetchMock } = fakeCore({ items: [UNKNOWN] });
    renderAt(fetchMock);
    const actions = await row("lost.gcode");
    for (const name of ["It printed", "It did not print"]) {
      const button = actions.getByRole("button", { name });
      expect(button.className).not.toContain("pp-button-primary");
      expect(document.activeElement).not.toBe(button);
    }
    const tableRow = screen.getByText("lost.gcode").closest("tr")!;
    fireEvent.keyDown(tableRow, { key: "Enter" });
    fireEvent.keyDown(document.body, { key: "Enter" });
    expect(writes(fetchMock)).toEqual([]);
  });

  it("Send now on an Upload and start print row asks first, with the bed warning", async () => {
    const start = item("s", { filename: "start.gcode", start: true, wait_for_idle: true });
    const { fetchMock } = fakeCore({
      items: [start],
      printers: [printer("p1", "Micron", BED)],
    });
    renderAt(fetchMock);
    fireEvent.click((await row("start.gcode")).getByRole("button", { name: "Send now" }));
    let confirm = await screen.findByRole("dialog", { name: "Start this print?" });
    expect(within(confirm).getAllByText(/./, { selector: "p" })[0]!.textContent).toMatch(
      /bed needs clearing/,
    );
    fireEvent.click(within(confirm).getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(writes(fetchMock)).toEqual([]);
    fireEvent.click((await row("start.gcode")).getByRole("button", { name: "Send now" }));
    confirm = await screen.findByRole("dialog", { name: "Start this print?" });
    fireEvent.click(within(confirm).getByRole("button", { name: "Start print" }));
    await waitFor(() => expect(dispatches(fetchMock)).toHaveLength(1));
  });

  it("Upload only Send now needs no confirmation", async () => {
    const { fetchMock } = fakeCore({ items: [item("u", { filename: "upload.gcode" })] });
    renderAt(fetchMock);
    fireEvent.click((await row("upload.gcode")).getByRole("button", { name: "Send now" }));
    await waitFor(() => expect(dispatches(fetchMock)).toHaveLength(1));
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("a refusal shows the core's reason on the row; a 5xx shows could not confirm; neither repeats", async () => {
    let answer = problem(409, "busy", "Printer is printing — wait for Idle or force dispatch");
    const { fetchMock } = fakeCore({
      items: [item("q", { filename: "busy.gcode" })],
      dispatch: () => answer,
    });
    renderAt(fetchMock);
    fireEvent.click((await row("busy.gcode")).getByRole("button", { name: "Send now" }));
    expect(await screen.findByText(/Printer is printing/)).toBeTruthy();
    answer = json(
      { type: "urn:print-partner:problem:internal", title: "Internal", status: 500 },
      500,
    );
    fireEvent.click((await row("busy.gcode")).getByRole("button", { name: "Send now" }));
    expect(await screen.findByText(/could not confirm what the core did/)).toBeTruthy();
    expect(dispatches(fetchMock)).toHaveLength(2);
  });

  it("one write at a time: the row's buttons are off while its write is in flight", async () => {
    let release: (r: Response) => void = () => {};
    const { fetchMock } = fakeCore({
      items: [item("q", { filename: "slow.gcode" })],
      dispatch: () => new Promise<Response>((resolve) => (release = resolve)),
    });
    renderAt(fetchMock);
    fireEvent.click((await row("slow.gcode")).getByRole("button", { name: "Send now" }));
    const actions = await row("slow.gcode");
    await waitFor(() =>
      expect(actions.getByRole("button", { name: "Send now" }).getAttribute("aria-disabled")).toBe(
        "true",
      ),
    );
    fireEvent.click(actions.getByRole("button", { name: "Send now" }));
    fireEvent.click(actions.getByRole("button", { name: "Remove" }));
    expect(writes(fetchMock)).toHaveLength(1);
    release(problem(409, "busy", "Busy"));
    expect(await screen.findByText("Busy")).toBeTruthy();
  });

  it("Sending and Sent rows offer nothing", async () => {
    renderAt(
      fakeCore({
        items: [
          item("a", { filename: "sending.gcode", state: "sending", upload_job_id: "j" }),
          item("b", { filename: "sent.gcode", state: "done", upload_job_id: "j2" }),
        ],
      }).fetchMock,
    );
    await screen.findByRole("table", { name: "Sends" });
    expect(screen.queryByRole("group", { name: /^Actions for/ })).toBeNull();
  });
});
