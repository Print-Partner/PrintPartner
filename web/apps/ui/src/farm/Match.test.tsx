import type { FarmPrinterList, Health, MatchSuggestionList } from "@print-partner/rust-client";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { createMemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App, createQueryClient } from "../App";
import { routes } from "../routes";

type Suggestion = MatchSuggestionList["suggestions"][number];
type Printer = FarmPrinterList["printers"][number];

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });

const suggestion = (
  id: string,
  confidence: Suggestion["confidence"],
  build: Suggestion["build"],
): Suggestion => ({
  print_id: id,
  printer_id: "voron",
  filename: `${id}.gcode`,
  completed_at: "2026-11-05T19:00:00.000Z",
  confidence,
  starts_checked: confidence === "exact",
  build,
  plate: build ? { plate_id: "plate-1", ordinal: 1 } : null,
  evidence: {
    basis: confidence === "exact" ? ["unit_tokens"] : confidence === "likely" ? ["file_name"] : [],
    file_name: { build_ids: build ? [build.id] : [] },
    object_list: { object_count: 2, part_build_ids: [], unit_build_ids: [], unit_token_count: 2 },
    time_window: null,
  },
});

// Synthetic backlog: one print per confidence, plus a second Likely one.
const BACKLOG = (): Suggestion[] => [
  suggestion("bracket", "exact", { id: 22, name: "Voron 2.4 kit" }),
  suggestion("gear", "likely", { id: 19, name: "Trident kit" }),
  suggestion("spacer", "likely", { id: 19, name: "Trident kit" }),
  suggestion("benchy", "none", null),
];

const printer: Printer = {
  id: "voron",
  name: "Voron 2.4",
  model: "Voron 2.4",
  integration: { id: "sim-moonraker", kind: "moonraker" },
  state: { execution: "idle", bed_clear: "clear", display: "ready" },
  reason: null,
  last_heard_at: null,
  progress_percent: null,
  loaded_filaments: [],
};

const health = (farm: "on" | "off"): Health => ({
  ok: true,
  authenticated: true,
  authentication_required: true,
  capabilities: [],
  multi_user: false,
  registration_open: false,
  modules:
    farm === "on"
      ? [{ module: "farm", state: "on" }]
      : [{ module: "farm", state: "off", reason: "turned_off" }],
});

/** A Rust core with the claim and dismiss routes: each resolves the print and drops its row. */
function fakeCore({ farm = "on" as "on" | "off", claimed = new Set<string>() } = {}) {
  const state = { suggestions: BACKLOG(), claimed, dismissed: new Set<string>() };
  const posts: { path: string; body: unknown }[] = [];
  const fetchMock = vi.fn(async (path: string, init?: RequestInit) => {
    if (init?.method === "POST") {
      const body = JSON.parse(String(init.body)) as { profile_id?: number };
      posts.push({ path, body });
      const [, id, action] = /unattributed\/([^/]+)\/(claim|dismiss)$/.exec(path)!;
      const row = state.suggestions.find((s) => s.print_id === id);
      if (action === "dismiss") {
        state.dismissed.add(id!);
        state.suggestions = state.suggestions.filter((s) => s.print_id !== id);
        return json({ ok: true });
      }
      if (state.claimed.has(id!))
        return json({ title: "Conflict", detail: "Print already claimed" }, 409);
      // Like the core, a claim claims every open print of the same file on that printer.
      const same = state.suggestions.filter(
        (s) => s.printer_id === row!.printer_id && s.filename === row!.filename,
      );
      for (const s of same) state.claimed.add(s.print_id);
      state.suggestions = state.suggestions.filter((s) => !same.includes(s));
      return json({
        ok: true,
        link: {
          id: `link-${id}`,
          filename: row!.filename,
          printer_id: row!.printer_id,
          profile_id: body.profile_id,
          state: "awaiting_verify",
        },
      });
    }
    if (path === "/health") return json(health(farm));
    if (path === "/api/v2/printer-checkoff/unattributed/suggestions")
      return json({ suggestions: state.suggestions });
    if (path === "/api/v2/printers") return json({ printers: [printer] });
    return json({ title: "Not Found" }, 404);
  });
  return { state, posts, fetchMock };
}

function renderMatch(core: ReturnType<typeof fakeCore>) {
  vi.stubGlobal("fetch", core.fetchMock);
  const router = createMemoryRouter(routes, { initialEntries: ["/farm/match"] });
  render(<App router={router} queryClient={createQueryClient()} />);
}

const group = (name: RegExp) => screen.getByRole("region", { name });
const box = (name: string) =>
  screen.getByRole("checkbox", { name: new RegExp(`^${name}\\.gcode`) });
const result = () => screen.getByRole("status", { name: "Match review result" });

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("UI5-03 Match review groups by confidence and ticks nothing by itself", () => {
  it("groups Exact, Likely and No suggestion, with only Exact checked and nothing sent", async () => {
    const core = fakeCore();
    renderMatch(core);
    expect(await screen.findByRole("heading", { level: 2, name: "Exact (1)" })).toBeTruthy();
    expect(screen.getAllByRole("heading", { level: 2 }).map((h) => h.textContent)).toEqual([
      "Exact (1)",
      "Likely (2)",
      "No suggestion (1)",
    ]);
    expect(box("bracket")).toHaveProperty("checked", true);
    expect(box("gear")).toHaveProperty("checked", false);
    expect(box("spacer")).toHaveProperty("checked", false);
    expect(box("benchy")).toHaveProperty("checked", false);
    expect(within(group(/^Likely/)).getByText("Nothing checked in Likely.")).toBeTruthy();
    expect(await screen.findByLabelText(/^bracket\.gcode on Voron 2\.4/)).toBeTruthy();
    expect(
      screen.getByText(/Claim for Voron 2\.4 kit, plate 1\. From Required-unit names/),
    ).toBeTruthy();
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(core.posts).toEqual([]);
  });

  it("claims the checked Exact print for its Build with one press", async () => {
    const core = fakeCore();
    renderMatch(core);
    fireEvent.click(await screen.findByRole("button", { name: "Apply Exact: claim 1 print" }));
    expect(await within(result()).findByText("Applied: claimed 1 print.")).toBeTruthy();
    expect(core.posts).toEqual([
      { path: "/api/v2/printer-checkoff/unattributed/bracket/claim", body: { profile_id: 22 } },
    ]);
    await waitFor(() => expect(screen.queryByRole("heading", { name: /^Exact/ })).toBeNull());
  });

  it("claims Likely prints only once they are checked by hand", async () => {
    const core = fakeCore();
    renderMatch(core);
    fireEvent.click(await screen.findByRole("button", { name: "Check all Likely" }));
    fireEvent.click(box("spacer"));
    fireEvent.click(screen.getByRole("button", { name: "Apply Likely: claim 1 print" }));
    expect(await within(result()).findByText("Applied: claimed 1 print.")).toBeTruthy();
    expect(core.posts.map((p) => p.path)).toEqual([
      "/api/v2/printer-checkoff/unattributed/gear/claim",
    ]);
    expect(core.state.claimed).toEqual(new Set(["gear"]));
  });

  it("dismisses a checked print with no suggestion", async () => {
    const core = fakeCore();
    renderMatch(core);
    fireEvent.click(await screen.findByRole("checkbox", { name: /^benchy\.gcode/ }));
    fireEvent.click(screen.getByRole("button", { name: "Apply No suggestion: dismiss 1 print" }));
    expect(await within(result()).findByText("Applied: dismissed 1 print.")).toBeTruthy();
    expect(core.posts).toEqual([
      { path: "/api/v2/printer-checkoff/unattributed/benchy/dismiss", body: {} },
    ]);
  });

  it("sends one claim when Apply is pressed twice", async () => {
    const core = fakeCore();
    renderMatch(core);
    const apply = await screen.findByRole("button", { name: "Apply Exact: claim 1 print" });
    fireEvent.click(apply);
    fireEvent.click(apply);
    await within(result()).findByText("Applied: claimed 1 print.");
    expect(core.posts).toHaveLength(1);
  });

  it("says a print was already claimed instead of counting it as claimed now", async () => {
    const core = fakeCore({ claimed: new Set(["bracket"]) });
    renderMatch(core);
    fireEvent.click(await screen.findByRole("button", { name: "Apply Exact: claim 1 print" }));
    expect(await within(result()).findByText("1 print was already claimed.")).toBeTruthy();
  });

  it("keeps a refused print checked and shows the core's reason", async () => {
    const core = fakeCore();
    const real = core.fetchMock.getMockImplementation()!;
    core.fetchMock.mockImplementation(async (path: string, init?: RequestInit) =>
      path.endsWith("/bracket/claim")
        ? json({ title: "Conflict", detail: "Accepted Plan has no Required units" }, 409)
        : real(path, init),
    );
    renderMatch(core);
    fireEvent.click(await screen.findByRole("button", { name: "Apply Exact: claim 1 print" }));
    expect(await within(result()).findByText("1 print could not be applied.")).toBeTruthy();
    expect(screen.getByText("Accepted Plan has no Required units")).toBeTruthy();
    expect(box("bracket")).toHaveProperty("checked", true);
  });

  it("keeps other groups' choices and refusals when one group is applied", async () => {
    const core = fakeCore();
    const real = core.fetchMock.getMockImplementation()!;
    core.fetchMock.mockImplementation(async (path: string, init?: RequestInit) =>
      path.endsWith("/spacer/claim")
        ? json({ title: "Conflict", detail: "Accepted Plan has no Required units" }, 409)
        : real(path, init),
    );
    renderMatch(core);
    fireEvent.click(await screen.findByRole("checkbox", { name: /^spacer\.gcode/ }));
    fireEvent.click(screen.getByRole("button", { name: "Apply Likely: claim 1 print" }));
    expect(await screen.findByText("Accepted Plan has no Required units")).toBeTruthy();
    fireEvent.click(box("bracket"));
    fireEvent.click(box("benchy"));
    fireEvent.click(screen.getByRole("button", { name: "Apply No suggestion: dismiss 1 print" }));
    expect(await within(result()).findByText("Applied: dismissed 1 print.")).toBeTruthy();
    await waitFor(() =>
      expect(screen.queryByRole("checkbox", { name: /^benchy\.gcode/ })).toBeNull(),
    );
    expect(box("bracket")).toHaveProperty("checked", false);
    expect(box("spacer")).toHaveProperty("checked", true);
    expect(screen.getByText("Accepted Plan has no Required units")).toBeTruthy();
    expect(core.posts.map((p) => p.path)).toEqual([
      "/api/v2/printer-checkoff/unattributed/benchy/dismiss",
    ]);
  });

  it("keeps the outcome of the Apply that empties the backlog", async () => {
    const core = fakeCore();
    core.state.suggestions = core.state.suggestions.filter((s) => s.print_id === "bracket");
    core.state.claimed.add("bracket");
    renderMatch(core);
    fireEvent.click(await screen.findByRole("button", { name: "Apply Exact: claim 1 print" }));
    core.state.suggestions = [];
    expect(await screen.findByText("Every finished print is matched.")).toBeTruthy();
    expect(within(result()).getByText("1 print was already claimed.")).toBeTruthy();
  });

  it("hides a claimed print at once, before the reread answers", async () => {
    const core = fakeCore();
    const real = core.fetchMock.getMockImplementation()!;
    let hold = false;
    core.fetchMock.mockImplementation((path: string, init?: RequestInit) =>
      hold && path === "/api/v2/printer-checkoff/unattributed/suggestions"
        ? new Promise<Response>(() => {})
        : real(path, init),
    );
    renderMatch(core);
    const apply = await screen.findByRole("button", { name: "Apply Exact: claim 1 print" });
    hold = true;
    fireEvent.click(apply);
    expect(await within(result()).findByText("Applied: claimed 1 print.")).toBeTruthy();
    expect(screen.queryByRole("checkbox", { name: /^bracket\.gcode/ })).toBeNull();
    expect(screen.queryByRole("button", { name: /^Apply Exact/ })).toBeNull();
    expect(core.posts).toHaveLength(1);
  });

  it("counts reprints of one file as claimed by the Apply that claimed them", async () => {
    const core = fakeCore();
    core.state.suggestions.push({
      ...suggestion("bracket", "exact", { id: 22, name: "Voron 2.4 kit" }),
      print_id: "bracket-reprint",
    });
    renderMatch(core);
    fireEvent.click(await screen.findByRole("button", { name: "Apply Exact: claim 2 prints" }));
    expect(await within(result()).findByText("Applied: claimed 2 prints.")).toBeTruthy();
    expect(core.posts).toHaveLength(1);
    expect(core.state.claimed).toEqual(new Set(["bracket", "bracket-reprint"]));
  });

  it("says the Farm module is off instead of reading suggestions", async () => {
    const core = fakeCore({ farm: "off" });
    renderMatch(core);
    expect(await screen.findByText("The Farm module is off.")).toBeTruthy();
    expect(
      screen.getByText("When it is on, this screen shows finished prints to match."),
    ).toBeTruthy();
    await waitFor(() =>
      expect(core.fetchMock.mock.calls.map(([p]) => p)).not.toContain(
        "/api/v2/printer-checkoff/unattributed/suggestions",
      ),
    );
    expect(screen.queryByRole("checkbox")).toBeNull();
  });
});
