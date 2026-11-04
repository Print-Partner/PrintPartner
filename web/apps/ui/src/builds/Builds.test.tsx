import type { Health, ProfileSummary } from "@print-partner/rust-client";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { createMemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App, createQueryClient } from "../App";
import { OFFLINE_REASON } from "../frame/connection";
import { routes } from "../routes";

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });

const summary = (id: number, name: string, extra: Partial<ProfileSummary> = {}): ProfileSummary => ({
  id,
  name,
  order_number: null,
  special_request: null,
  part_count: 4,
  accepted_progress: { kind: "ready", total_units: 40, remaining_units: 28 },
  build_stale: false,
  freshness: { status: "current", accepted_input_set_id: 1, accepted_at: "2026-10-02T00:05:00Z" },
  archived_at: null,
  last_used_at: "2026-10-03T12:00:00Z",
  ...extra,
});

const BUILDS = [
  summary(2, "Synthetic Build 01", { order_number: "SO-1001", special_request: "Matte black" }),
  summary(5, "Synthetic Build 04", { accepted_progress: { kind: "empty" } }),
  summary(9, "Synthetic Build 08", {
    accepted_progress: { kind: "unavailable", reason: "uninitialized" },
    archived_at: "2026-10-20T00:00:00Z",
  }),
];

const health = (orders: "on" | "off"): Health => ({
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
    orders === "on"
      ? [{ module: "orders", state: "on" }]
      : [{ module: "orders", state: "off", reason: "default_off" }],
});

const action = (planId: number, kind: string, label: string, reason: string) => ({
  kind,
  label,
  reason,
  stage_id: "checkoff",
  target: { plan_id: planId, tab: "checkoff" },
});

// The feed ranks Build 5 first, unlike the list's order, so a test can tell they differ.
const TASKS = {
  tasks: [
    {
      plan_id: 5,
      build_name: "Synthetic Build 04",
      due_date: null,
      attention: false,
      rank_reason: "No Sources",
      next_action: action(5, "attach_sources", "Attach Sources", "This Build has no Sources"),
    },
    {
      plan_id: 2,
      build_name: "Synthetic Build 01",
      due_date: null,
      attention: true,
      rank_reason: "Prints await verification",
      next_action: action(2, "verify_prints", "Verify prints", "2 prints are waiting for verification"),
    },
  ],
};

const stage = (id: string, taskCount?: number) => ({
  id,
  group: id === "sources" || id === "plan" ? "prepare" : "make",
  label: id,
  status: { kind: taskCount ? "needs_attention" : "ready", summary: "", task_count: taskCount },
});

const workflow = (id: number) => ({
  build: { id, name: "Synthetic Build 01" },
  sources: { kind: "ready", attached_count: 2 },
  accepted_plan: { kind: "ready", plan_version: 1, revision_id: 1, total_units: 40, remaining_units: 28 },
  working_plan: { kind: "none" },
  stock: { kind: "not_ready" },
  stages: [stage("sources"), stage("plan", 1), stage("production"), stage("checkoff", 2)],
  next_action: TASKS.tasks[1].next_action,
  active_work: {
    awaiting_verification: 2,
    failed_jobs: 0,
    failed_verifications: 0,
    printing_jobs: 0,
    queued_jobs: 0,
    remaining_units: 28,
    sending_jobs: 0,
    total_units: 40,
  },
});

type Handler = (path: string) => Response | Promise<Response>;

/** A fake core: /health with the Orders module as given, plus `handler` for the rest. */
function core(handler: Handler, orders: "on" | "off" = "on") {
  return vi.fn((path: string) =>
    Promise.resolve(path === "/health" ? json(health(orders)) : handler(path)),
  );
}

const plans: Handler = (path) => {
  if (path === "/api/v2/plans") return json({ profiles: BUILDS });
  if (path === "/api/v2/tasks") return json(TASKS);
  const flow = /^\/api\/v2\/plans\/(\d+)\/workflow$/.exec(path);
  if (flow) return json(workflow(Number(flow[1])));
  const match = /^\/api\/v2\/plans\/(\d+)(\/layers)?$/.exec(path);
  const build = BUILDS.find((b) => b.id === Number(match?.[1]));
  if (!build) return json({ type: "urn:print-partner:problem:not-found", title: "Not Found", status: 404 }, 404);
  if (!match?.[2]) return json(build);
  return json({
    profile_id: build.id,
    layers: [
      { id: 12, layer_order: 1, layer_type: "addon", project_id: 7, project_name: "Synthetic Source 02" },
      { id: 11, layer_order: 0, layer_type: "base", project_id: 4, project_name: "Synthetic Source 05" },
    ],
  });
};

function renderAt(path: string, fetchImpl: ReturnType<typeof core>) {
  vi.stubGlobal("fetch", fetchImpl);
  const router = createMemoryRouter(routes, { initialEntries: [path] });
  const queryClient = createQueryClient();
  // Keep the app's retry counts, without the backoff between attempts.
  queryClient.setDefaultOptions({
    queries: { ...queryClient.getDefaultOptions().queries, retryDelay: 0 },
  });
  render(<App router={router} queryClient={queryClient} />);
  return { router, queryClient };
}

const table = () => screen.getByRole("table", { name: "Builds" });
const requested = (fetchMock: ReturnType<typeof core>) => fetchMock.mock.calls.map(([p]) => p);

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe("UI2-01 Builds list", () => {
  it("shows name, order, progress and next action from the Rust API, in the list's order", async () => {
    const fetchMock = core(plans);
    renderAt("/builds", fetchMock);
    await screen.findByRole("link", { name: "Synthetic Build 01" });
    await screen.findByRole("columnheader", { name: "Order" });
    await screen.findByText("Attach Sources");
    const rows = within(table()).getAllByRole("row");
    expect(rows.map((r) => [...r.querySelectorAll("th, td")].map((c) => c.textContent))).toEqual([
      ["Build", "Order", "Progress", "Next action"],
      ["Synthetic Build 01", "SO-1001", "12 of 40 units printed", "Verify printsNeeds you"],
      ["Synthetic Build 04", "None", "No accepted units", "Attach Sources"],
      ["Synthetic Build 08Archived", "None", "Progress unavailable: not computed yet", "Nothing open"],
    ]);
    expect(requested(fetchMock)).toContain("/api/v2/tasks");
    expect(screen.getByRole("link", { name: "Synthetic Build 01" }).getAttribute("href")).toBe("/builds/2");
    expect(requested(fetchMock)).toContain("/api/v2/plans");
    expect(
      requested(fetchMock).every(
        (p) => p === "/health" || p === "/api/v2/tasks" || p.startsWith("/api/v2/plans"),
      ),
    ).toBe(true);
  });

  it("keeps the Builds readable when next actions fail, and retries them", async () => {
    let tasksUp = false;
    renderAt("/builds", core((p) => (p === "/api/v2/tasks" && !tasksUp ? json({}, 503) : plans(p))));
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("Could not load next actions.");
    expect(screen.getAllByText("Not loaded")).toHaveLength(3);
    expect(screen.getByRole("link", { name: "Synthetic Build 01" })).toBeTruthy();
    tasksUp = true;
    fireEvent.click(within(alert).getByRole("button", { name: "Retry" }));
    expect(await screen.findByText("Attach Sources")).toBeTruthy();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("moves focus between Builds with Up, Down, Home and End", async () => {
    renderAt("/builds", core(plans));
    const first = await screen.findByRole("link", { name: "Synthetic Build 01" });
    first.focus();
    const key = (k: string) => fireEvent.keyDown(document.activeElement!, { key: k });
    key("ArrowDown");
    expect(document.activeElement?.textContent).toBe("Synthetic Build 04");
    key("End");
    expect(document.activeElement?.textContent).toBe("Synthetic Build 08");
    key("ArrowDown");
    expect(document.activeElement?.textContent).toBe("Synthetic Build 08");
    key("Home");
    expect(document.activeElement).toBe(first);
    key("ArrowUp");
    expect(document.activeElement).toBe(first);
  });

  it("keeps the focused Build focused when the list refreshes", async () => {
    const { queryClient } = renderAt("/builds", core(plans));
    const link = await screen.findByRole("link", { name: "Synthetic Build 04" });
    link.focus();
    await act(() => queryClient.refetchQueries({ queryKey: ["core", "builds"] }));
    expect(document.activeElement).toBe(link);
  });
});

describe("UI2-03 Builds list states", () => {
  it("loading shows no rows and no numbers", () => {
    renderAt("/builds", core(() => new Promise<Response>(() => {})));
    const section = screen.getByRole("heading", { level: 1, name: "Builds" }).closest("section")!;
    expect(section.dataset.state).toBe("loading");
    expect(section.textContent).not.toMatch(/\d/);
  });

  it("empty says so", async () => {
    renderAt("/builds", core(() => json({ profiles: [] })));
    expect(await screen.findByText("No Builds yet. Builds you create appear here.")).toBeTruthy();
  });

  it("error with nothing loaded offers Retry and recovers", async () => {
    let fail = true;
    renderAt(
      "/builds",
      core((p) => (fail ? json({}, 503) : plans(p))),
    );
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("Could not load Builds.");
    expect(alert.textContent).toContain("Rust core answered 503 for /api/v2/plans");
    fail = false;
    fireEvent.click(within(alert).getByRole("button", { name: "Retry" }));
    expect(await screen.findByRole("link", { name: "Synthetic Build 01" })).toBeTruthy();
  });

  it("error keeps the last-known Builds with their time", async () => {
    let fail = false;
    const { queryClient } = renderAt("/builds", core((p) => (fail ? json({}, 500) : plans(p))));
    await screen.findByRole("link", { name: "Synthetic Build 01" });
    fail = true;
    await act(() => queryClient.refetchQueries({ queryKey: ["core", "builds"] }).catch(() => {}));
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("Could not refresh Builds. Showing data from");
    expect(alert.querySelector("time")).toBeTruthy();
    expect(screen.getByRole("link", { name: "Synthetic Build 01" })).toBeTruthy();
  });

  it("an answer that breaks the contract is an error, not a partly drawn list", async () => {
    renderAt("/builds", core(() => json({ profiles: [{ id: "2", name: 5 }] })));
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("does not match the client contract");
    expect(screen.queryByRole("table")).toBeNull();
  });

  it("Orders module off hides order numbers and says how to show them", async () => {
    renderAt("/builds", core(plans, "off"));
    await screen.findByText("The Orders module is off.");
    expect(screen.getByText("When it is on, this list shows each Build's order number.")).toBeTruthy();
    expect(screen.getByRole("link", { name: "Turn it on in Settings" }).getAttribute("href")).toBe("/settings");
    expect(within(table()).queryByRole("columnheader", { name: "Order" })).toBeNull();
    expect(screen.queryByText("SO-1001")).toBeNull();
  });

  it("shows order numbers once a failed Module read recovers", async () => {
    let healthUp = false;
    const fetchMock = vi.fn((path: string) =>
      Promise.resolve(path === "/health" ? (healthUp ? json(health("on")) : json({}, 503)) : plans(path)),
    );
    vi.useFakeTimers({ shouldAdvanceTime: true });
    renderAt("/builds", fetchMock as unknown as ReturnType<typeof core>);
    await screen.findByRole("link", { name: "Synthetic Build 01" });
    await act(() => vi.advanceTimersByTimeAsync(1_000));
    expect(within(table()).queryByRole("columnheader", { name: "Order" })).toBeNull();
    healthUp = true;
    await act(() => vi.advanceTimersByTimeAsync(6_000));
    expect(await within(table()).findByRole("columnheader", { name: "Order" })).toBeTruthy();
    expect(screen.getByText("SO-1001")).toBeTruthy();
  });

  it("offline keeps the loaded Builds readable and says why", async () => {
    let down = false;
    const fetchMock = vi.fn((path: string) => {
      if (down) return Promise.reject(new TypeError("Failed to fetch"));
      return Promise.resolve(path === "/health" ? json(health("on")) : plans(path));
    });
    vi.useFakeTimers({ shouldAdvanceTime: true });
    renderAt("/builds", fetchMock as unknown as ReturnType<typeof core>);
    await screen.findByRole("link", { name: "Synthetic Build 01" });
    down = true;
    await act(() => vi.advanceTimersByTimeAsync(31_000));
    await waitFor(() => expect(screen.getByText("Offline")).toBeTruthy());
    const section = screen.getByRole("heading", { level: 1, name: "Builds" }).closest("section")!;
    expect(section.dataset.state).toBe("offline");
    expect(within(section).getByRole("status").textContent).toContain(OFFLINE_REASON);
    expect(screen.getByRole("link", { name: "Synthetic Build 01" })).toBeTruthy();
  });
});

describe("UI2-02 Build Overview", () => {
  it("opens from the list with the six tabs and no Machine tab", async () => {
    const { router } = renderAt("/builds", core(plans));
    fireEvent.click(await screen.findByRole("link", { name: "Synthetic Build 01" }));
    expect(router.state.location.pathname).toBe("/builds/2");
    await screen.findByRole("heading", { level: 1, name: "Synthetic Build 01" });
    const tabs = within(screen.getByRole("navigation", { name: "Build sections" })).getAllByRole("link");
    expect(tabs.map((t) => t.textContent)).toEqual([
      "Overview",
      "Sources",
      "Plan",
      "Production",
      "Checkoff",
      "History",
    ]);
    expect(tabs[0].getAttribute("aria-current")).toBe("page");
    expect(screen.queryByText("Machine")).toBeNull();
  });

  it("shows progress, Plan state, order fields and layers in layer order", async () => {
    renderAt("/builds/2", core(plans));
    await screen.findByText("Order", { selector: "dt" });
    const fact = (term: string) => screen.getByText(term, { selector: "dt" }).nextElementSibling?.textContent;
    expect(fact("Progress")).toBe("12 of 40 units printed");
    expect(fact("Parts")).toBe("4");
    expect(fact("Plan")).toMatch(/^Current, accepted /);
    expect(fact("Order")).toBe("SO-1001");
    expect(fact("Customer request")).toBe("Matte black");
    await screen.findByText("Open decisions", { selector: "dt" });
    expect(fact("Next action")).toBe("Verify prints2 prints are waiting for verification");
    expect(fact("Open decisions")).toBe("3");
    const layers = within(screen.getByRole("list", { name: "Layers" }));
    expect(layers.getAllByRole("listitem").map((l) => l.textContent)).toEqual([
      "Synthetic Source 05 base",
      "Synthetic Source 02 addon",
    ]);
  });

  it("says when the Build's workflow could not load, with a Retry", async () => {
    renderAt("/builds/2", core((p) => (p.endsWith("/workflow") ? json({}, 500) : plans(p))));
    await screen.findByText(/Could not load: Rust core answered 500/);
    expect(screen.getByRole("button", { name: "Retry" })).toBeTruthy();
    expect(screen.getByRole("heading", { level: 2, name: "Layers" })).toBeTruthy();
  });

  it("lists stale reasons in words", async () => {
    const stale = summary(2, "Synthetic Build 01", {
      freshness: {
        status: "stale",
        accepted_input_set_id: 3,
        accepted_at: "2026-10-02T00:05:00Z",
        reasons: [
          {
            kind: "source_revision_changed",
            accepted_revision_id: 1,
            current_revision_id: 2,
            source_id: 4,
            source_name: "Synthetic Source 05",
          },
        ],
        untracked_sources: [],
      },
    });
    renderAt("/builds/2", core((p) => (p === "/api/v2/plans/2" ? json(stale) : plans(p))));
    expect(
      await screen.findByText("Synthetic Source 05 has a newer revision than the accepted Plan"),
    ).toBeTruthy();
  });

  it("Orders module off hides the order fields on the Overview", async () => {
    renderAt("/builds/2", core(plans, "off"));
    await screen.findByText("When it is on, the Overview shows this Build's order number and customer request.");
    expect(screen.queryByText("SO-1001")).toBeNull();
    expect(screen.queryByText("Matte black")).toBeNull();
  });

  it("a Build the core does not have is Not found", async () => {
    renderAt("/builds/4", core(plans));
    expect(await screen.findByRole("heading", { level: 1, name: "Not found" })).toBeTruthy();
  });

  it.each(["/builds/0", "/builds/abc", "/builds/9007199254740993"])("%s is Not found without a request", (path) => {
    const fetchMock = core(plans);
    renderAt(path, fetchMock);
    expect(screen.getByRole("heading", { level: 1, name: "Not found" })).toBeTruthy();
    expect(requested(fetchMock).filter((p) => p.startsWith("/api"))).toEqual([]);
  });

  it("an error keeps the last-known Overview with a Retry", async () => {
    let fail = false;
    const { queryClient } = renderAt("/builds/2", core((p) => (fail ? json({}, 503) : plans(p))));
    await screen.findByRole("heading", { level: 2, name: "Layers" });
    fail = true;
    await act(() => queryClient.refetchQueries({ queryKey: ["core", "build", 2] }).catch(() => {}));
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("Could not refresh Synthetic Build 01.");
    expect(within(alert).getByRole("button", { name: "Retry" })).toBeTruthy();
    expect(screen.getByRole("heading", { level: 2, name: "Layers" })).toBeTruthy();
  });

  it("other tabs say which slice brings them and keep the tab bar", async () => {
    renderAt("/builds/2/checkoff", core(plans));
    expect(screen.getByRole("heading", { level: 1, name: "Checkoff" })).toBeTruthy();
    expect(screen.getByText(/This tab arrives in a later slice/)).toBeTruthy();
    const tabs = within(screen.getByRole("navigation", { name: "Build sections" })).getAllByRole("link");
    expect(tabs.find((t) => t.getAttribute("aria-current") === "page")?.textContent).toBe("Checkoff");
  });
});
