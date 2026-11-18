import type {
  CreateBuildRequest,
  CreateSourceRequest,
  Health,
  ProfileLayers,
  ProfileSummary,
  SourceAttachmentRequest,
  SourceList,
} from "@print-partner/rust-client";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { createMemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App, createQueryClient } from "../App";
import { OFFLINE_REASON } from "../frame/connection";
import { routes } from "../routes";
import { refusalText } from "../screen/SourceSelect";
import { buildRequest, nameProblem } from "./CreateBuildDialog";
import { FIRST_BUILD_STEPS } from "./planCopy";

// GRE-1401: a scripted core on disposable, in-memory data. It keeps Sources, Builds and
// layers, and answers create and set-base as Rust does: a Build name already in use and an
// unknown base Source are 400s with the core's wording, and nothing is written.
const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
const problem = (status: number, detail: string) =>
  json(
    { type: "urn:print-partner:problem:validation", title: "Bad Request", status, detail },
    status,
  );

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
  modules: [],
};

type Source = SourceList["sources"][number];
type Layer = ProfileLayers["layers"][number];

const storedSource = (id: number, name: string): Source => ({
  id,
  name,
  url: "",
  source_kind: "archive",
  source_type: "local",
  role: "unassigned",
  category: null,
  branch: "main",
  tag: null,
  local_path: null,
  content_available: false,
  last_synced_at: null,
  last_commit_sha: null,
  current_source_revision_id: null,
  docs_url: null,
  manifest_community_slug: null,
  metadata: null,
  naming_use_defaults: true,
  update_status: null,
  update_checked_at: null,
  doc_count: 0,
});

/** A Build as Rust answers one with no accepted Plan yet. */
const storedBuild = (id: number, name: string): ProfileSummary => ({
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

interface Seed {
  sources?: string[];
  /** Builds and their base Source's name, or null for a Build with no layers. */
  builds?: [string, string | null][];
  /** Replaces the create's reply after the Build is stored, as a lost or failed answer. */
  answer?: (stored: ProfileSummary) => Response;
  /** How the Source list answers: held until `releaseSources`, or a 500. */
  sourceList?: "held" | "failed";
}

function scriptedCore({
  sources: sourceNames = [],
  builds: buildSeeds = [],
  answer,
  sourceList,
}: Seed = {}) {
  const sources = sourceNames.map((name, i) => storedSource(i + 1, name));
  const builds: ProfileSummary[] = [];
  const layers = new Map<number, Layer[]>();
  const sourcePosts: CreateSourceRequest[] = [];
  const buildPosts: CreateBuildRequest[] = [];
  const basePuts: { id: number; body: SourceAttachmentRequest }[] = [];
  let down = false;
  const baseLayer = (id: number, source: Source): Layer => ({
    id: id * 10,
    layer_order: 0,
    layer_type: "base",
    project_id: source.id,
    project_name: source.name,
  });
  const addBuild = (name: string, sourceId: number | null) => {
    const build = storedBuild(1000 + builds.length, name);
    builds.push(build);
    const source = sources.find((s) => s.id === sourceId);
    layers.set(build.id, source ? [baseLayer(build.id, source)] : []);
    return build;
  };
  for (const [name, base] of buildSeeds) {
    addBuild(name, sources.find((s) => s.name === base)?.id ?? null);
  }
  type Body = CreateSourceRequest & CreateBuildRequest & SourceAttachmentRequest;
  const sourceRoute = (path: string, method: string, body: Body): Response | null => {
    if (path === "/api/v1/sources/activity?limit=100") return json({ events: [] });
    if (path === "/api/v1/sources" && method === "POST") {
      sourcePosts.push(body);
      const source = storedSource(sources.length + 1, body.name);
      sources.push(source);
      return json(source);
    }
    if (path === "/api/v1/sources") return json({ sources });
    const files = /^\/api\/v1\/sources\/(\d+)\/files$/.exec(path);
    return files && json({ source_id: Number(files[1]), revision_id: null, files: [] });
  };
  const createBuild = (request: CreateBuildRequest): Response => {
    buildPosts.push(request);
    if (builds.some((b) => b.name === request.name)) {
      return problem(400, `Profile already exists: ${request.name}`);
    }
    const base = request.base_project_id ?? null;
    if (base !== null && !sources.some((s) => s.id === base)) {
      return problem(400, "Project not found");
    }
    const build = addBuild(request.name, base);
    return answer ? answer(build) : json({ ...build, layers: layers.get(build.id) });
  };
  const setBase = (build: ProfileSummary, request: SourceAttachmentRequest): Response => {
    basePuts.push({ id: build.id, body: request });
    const source = sources.find((s) => s.id === request.project_id);
    if (!source) return problem(404, "Project not found");
    layers.set(build.id, [baseLayer(build.id, source)]);
    return json({ profile_id: build.id, layers: layers.get(build.id) });
  };
  const planRoute = (path: string, method: string, body: Body): Response => {
    if (path === "/api/v2/plans") {
      return method === "POST" ? createBuild(body) : json({ profiles: builds });
    }
    const plan =
      /^\/api\/v2\/plans\/(\d+)(\/layers(\/base)?|\/workflow|\/filament-shortfalls)?$/.exec(path);
    const build = builds.find((b) => b.id === Number(plan?.[1]));
    if (!plan || !build) return problem(404, "Profile not found");
    if (plan[3] && method === "PUT") return setBase(build, body);
    if (plan[2] === "/layers") return json({ profile_id: build.id, layers: layers.get(build.id) });
    if (plan[2] === "/filament-shortfalls") {
      return json({ kind: "module_off", module: "spoolman", reason: "default_off" });
    }
    if (plan[2] === "/workflow") return problem(503, "Workflow not scripted");
    return json(build);
  };
  const route = (path: string, init?: RequestInit): Response => {
    const method = init?.method ?? "GET";
    const body = (init?.body ? JSON.parse(String(init.body)) : undefined) as Body;
    if (path === "/health") return json(health);
    if (path === "/api/v2/tasks") return json({ tasks: [], unavailable: [] });
    if (path === "/api/v2/printers") return json({ printers: [] });
    return sourceRoute(path, method, body) ?? planRoute(path, method, body);
  };
  let listMode = sourceList;
  const held: (() => void)[] = [];
  const fetchMock = vi.fn((path: string, init?: RequestInit): Promise<Response> => {
    if (down) return Promise.reject(new TypeError("Failed to fetch"));
    if (path === "/api/v1/sources" && !init?.method && listMode === "held") {
      return new Promise((resolve) => held.push(() => resolve(route(path, init))));
    }
    if (path === "/api/v1/sources" && !init?.method && listMode === "failed") {
      return Promise.resolve(problem(500, "Library unreadable"));
    }
    return Promise.resolve(route(path, init));
  });
  return {
    fetchMock,
    sources,
    builds,
    layers,
    sourcePosts,
    buildPosts,
    basePuts,
    goDown: () => (down = true),
    /** Answers the held Source list requests, and every later one, from the stored list. */
    releaseSources: () => {
      listMode = undefined;
      for (const answerHeld of held.splice(0)) answerHeld();
    },
  };
}

function renderAt(path: string, fetchMock: ReturnType<typeof scriptedCore>["fetchMock"]) {
  vi.stubGlobal("fetch", fetchMock);
  const router = createMemoryRouter(routes, { initialEntries: [path] });
  const queryClient = createQueryClient();
  queryClient.setDefaultOptions({
    queries: { ...queryClient.getDefaultOptions().queries, retry: false },
  });
  render(<App router={router} queryClient={queryClient} />);
  return router;
}

afterEach(() => {
  cleanup();
  localStorage.clear();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

/** A create is sent a microtask or more after the click, so "nothing sent" waits first. */
const settle = () => new Promise((resolve) => setTimeout(resolve, 50));

const dialog = (name: string) => screen.getByRole("dialog", { name });
const heading = (name: string) => screen.findByRole("heading", { level: 1, name });

describe("UI13-01: first run walks to a first Source and Build", () => {
  it("goes from an empty install to a Build on a new Source", async () => {
    const core = scriptedCore();
    const router = renderAt("/builds", core.fetchMock);

    // Builds is empty: it names Create Build and lists the same first-Build steps as Help.
    expect(await screen.findByText(/^No Builds yet\. Press Create Build/)).toBeTruthy();
    const steps = screen.getByRole("list", { name: "Your first Build" });
    expect(
      within(steps)
        .getAllByRole("listitem")
        .map((li) => li.textContent),
    ).toEqual([...FIRST_BUILD_STEPS]);

    // With no Sources, the dialog leads to Library instead of offering an empty list.
    fireEvent.click(screen.getByRole("button", { name: "Create Build" }));
    const empty = dialog("Create Build");
    const toLibrary = await within(empty).findByRole("link", { name: "Add a Source in Library" });
    expect(within(empty).queryByRole("combobox")).toBeNull();
    fireEvent.click(toLibrary);
    await heading("Library");
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(router.state.location.pathname).toBe("/library");

    // Library is empty: Add Source creates the first Source.
    expect(await screen.findByText(/^No Sources yet\. Press Add Source/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Add Source" }));
    fireEvent.change(within(dialog("Add Source")).getByLabelText("Name"), {
      target: { value: "Disposable Kit" },
    });
    fireEvent.click(within(dialog("Add Source")).getByRole("button", { name: "Create" }));
    await screen.findByRole("rowheader", { name: /Disposable Kit/ });
    expect(core.sourcePosts).toEqual([{ name: "Disposable Kit", source_kind: "archive" }]);

    // Back on Builds, Create Build offers that Source as the base and sends one create.
    fireEvent.click(screen.getByRole("link", { name: "Builds" }));
    await heading("Builds");
    fireEvent.click(screen.getByRole("button", { name: "Create Build" }));
    const form = dialog("Create Build");
    const base = (await within(form).findByLabelText("Base Source")) as HTMLSelectElement;
    expect(base.selectedOptions[0]!.textContent).toBe("Disposable Kit");
    expect([...base.options].map((o) => o.textContent)).toEqual([
      "No base Source",
      "Disposable Kit",
    ]);
    fireEvent.change(within(form).getByLabelText("Name"), { target: { value: " First Build " } });
    fireEvent.click(within(form).getByRole("button", { name: "Create" }));

    // The new Build opens on its Overview with the Source as its base layer.
    await heading("First Build");
    expect(router.state.location.pathname).toBe("/builds/1000");
    expect(core.buildPosts).toEqual([{ name: "First Build", base_project_id: 1 }]);
    const layers = screen.getByRole("list", { name: "Layers" });
    expect(layers.textContent).toContain("Disposable Kit");
    expect(layers.textContent).toContain("base");
    expect(screen.queryByRole("dialog")).toBeNull();

    // The list now holds it.
    fireEvent.click(screen.getByRole("link", { name: "Builds" }));
    const table = await screen.findByRole("table", { name: "Builds" });
    expect(within(table).getByRole("link", { name: "First Build" })).toBeTruthy();
  });

  it("creates a Build with no base Source when the user picks none", async () => {
    const core = scriptedCore({ sources: ["Kit A"] });
    renderAt("/builds", core.fetchMock);
    fireEvent.click(await screen.findByRole("button", { name: "Create Build" }));
    const form = dialog("Create Build");
    fireEvent.change(await within(form).findByLabelText("Base Source"), { target: { value: "" } });
    fireEvent.change(within(form).getByLabelText("Name"), { target: { value: "Loose" } });
    fireEvent.click(within(form).getByRole("button", { name: "Create" }));
    await heading("Loose");
    expect(core.buildPosts).toEqual([{ name: "Loose", base_project_id: null }]);
    // Overview names the next action for a Build with no layers.
    fireEvent.click(screen.getByRole("link", { name: "Choose its base Source" }));
    await heading("Sources");
  });

  it("a name already in use keeps the dialog open with the reason, and sends once", async () => {
    const core = scriptedCore({ sources: ["Kit A"], builds: [["Taken", "Kit A"]] });
    const router = renderAt("/builds", core.fetchMock);
    fireEvent.click(await screen.findByRole("button", { name: "Create Build" }));
    const form = dialog("Create Build");
    await within(form).findByLabelText("Base Source");
    fireEvent.change(within(form).getByLabelText("Name"), { target: { value: "Taken" } });
    const create = within(form).getByRole("button", { name: "Create" });
    fireEvent.click(create);
    fireEvent.click(create);
    expect((await within(form).findByRole("alert")).textContent).toBe(
      "A Build with this name already exists. Choose another name.",
    );
    expect(core.buildPosts).toHaveLength(1);
    expect(router.state.location.pathname).toBe("/builds");
    // Editing the name clears the stale refusal.
    fireEvent.change(within(form).getByLabelText("Name"), { target: { value: "Taken 2" } });
    expect(within(form).queryByRole("alert")).toBeNull();
  });

  it("a lost answer says the create could not be confirmed and reads the list again", async () => {
    const core = scriptedCore({
      sources: ["Kit A"],
      answer: () => problem(502, "Bad Gateway"),
    });
    renderAt("/builds", core.fetchMock);
    fireEvent.click(await screen.findByRole("button", { name: "Create Build" }));
    const form = dialog("Create Build");
    await within(form).findByLabelText("Base Source");
    fireEvent.change(within(form).getByLabelText("Name"), { target: { value: "Maybe" } });
    fireEvent.click(within(form).getByRole("button", { name: "Create" }));
    expect((await within(form).findByRole("alert")).textContent).toMatch(
      /^Could not confirm the Build was created: .*check it before you press Create again\.$/,
    );
    // The list was read again, so the stored Build shows once the dialog is closed.
    fireEvent.click(within(form).getByRole("button", { name: "Cancel" }));
    const table = await screen.findByRole("table", { name: "Builds" });
    expect(within(table).getByRole("link", { name: "Maybe" })).toBeTruthy();
    expect(core.buildPosts).toHaveLength(1);
  });

  it("a name over the core's limit says so before anything is sent", async () => {
    const core = scriptedCore({ sources: ["Kit A"] });
    renderAt("/builds", core.fetchMock);
    fireEvent.click(await screen.findByRole("button", { name: "Create Build" }));
    const form = dialog("Create Build");
    await within(form).findByLabelText("Base Source");
    const create = within(form).getByRole("button", { name: "Create" });
    // 4,097 characters, each two UTF-16 units: the limit counts characters.
    fireEvent.change(within(form).getByLabelText("Name"), {
      target: { value: "\u{1f9f5}".repeat(4097) },
    });
    expect(within(form).getByText("Shorten the name to 4,096 characters or fewer.")).toBeTruthy();
    expect(create.getAttribute("aria-disabled")).toBe("true");
    fireEvent.click(create);
    await settle();
    expect(core.buildPosts).toEqual([]);
    expect(within(form).queryByRole("alert")).toBeNull();

    // At the limit, with surrounding spaces trimmed, Create sends it.
    const longest = "\u{1f9f5}".repeat(4096);
    fireEvent.change(within(form).getByLabelText("Name"), { target: { value: ` ${longest} ` } });
    expect(create.getAttribute("aria-disabled")).toBeNull();
    fireEvent.click(create);
    await waitFor(() => expect(core.buildPosts).toEqual([{ name: longest, base_project_id: 1 }]));
  });

  it("Create sends nothing without a name, and offline it says why", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const core = scriptedCore({ sources: ["Kit A"] });
    renderAt("/builds", core.fetchMock);
    fireEvent.click(await screen.findByRole("button", { name: "Create Build" }));
    const form = dialog("Create Build");
    const create = within(form).getByRole("button", { name: "Create" });
    expect(within(form).getByText("Enter a name.")).toBeTruthy();
    fireEvent.click(create);
    fireEvent.change(within(form).getByLabelText("Name"), { target: { value: "Rack" } });
    core.goDown();
    await vi.advanceTimersByTimeAsync(31_000);
    await waitFor(() => expect(within(form).getByText(OFFLINE_REASON)).toBeTruthy());
    expect(create.getAttribute("aria-disabled")).toBe("true");
    fireEvent.click(create);
    await settle();
    expect(core.buildPosts).toEqual([]);
  });
});

describe("Create Build waits for the Source list before it picks a base", () => {
  it("sends nothing while the list loads, then defaults to the first Source", async () => {
    const core = scriptedCore({ sources: ["Kit A"], sourceList: "held" });
    renderAt("/builds", core.fetchMock);
    fireEvent.click(await screen.findByRole("button", { name: "Create Build" }));
    const form = dialog("Create Build");
    expect(within(form).getByText("Loading the Library's Sources.")).toBeTruthy();
    fireEvent.change(within(form).getByLabelText("Name"), { target: { value: "Rack" } });
    const create = within(form).getByRole("button", { name: "Create" });
    expect(within(form).getByText("Waiting for the Library's Sources.")).toBeTruthy();
    expect(create.getAttribute("aria-disabled")).toBe("true");
    fireEvent.click(create);
    await settle();
    expect(core.buildPosts).toEqual([]);

    core.releaseSources();
    const base = (await within(form).findByLabelText("Base Source")) as HTMLSelectElement;
    expect(base.selectedOptions[0]!.textContent).toBe("Kit A");
    expect(create.getAttribute("aria-disabled")).toBeNull();
    fireEvent.click(create);
    await heading("Rack");
    expect(core.buildPosts).toEqual([{ name: "Rack", base_project_id: 1 }]);
  });

  it("sends nothing after the list failed, and Try again reads it again", async () => {
    const core = scriptedCore({ sources: ["Kit A"], sourceList: "failed" });
    renderAt("/builds", core.fetchMock);
    fireEvent.click(await screen.findByRole("button", { name: "Create Build" }));
    const form = dialog("Create Build");
    fireEvent.change(within(form).getByLabelText("Name"), { target: { value: "Rack" } });
    expect(await within(form).findByText(/^Could not load the Library's Sources: /)).toBeTruthy();
    expect(
      within(form).getByText("The Library's Sources did not load. Press Try again."),
    ).toBeTruthy();
    const create = within(form).getByRole("button", { name: "Create" });
    expect(create.getAttribute("aria-disabled")).toBe("true");
    fireEvent.click(create);
    await settle();
    expect(core.buildPosts).toEqual([]);

    // Try again while the core still fails keeps Create held.
    fireEvent.click(within(form).getByRole("button", { name: "Try again" }));
    expect(await within(form).findByText(/^Could not load the Library's Sources: /)).toBeTruthy();
    fireEvent.click(create);
    await settle();
    expect(core.buildPosts).toEqual([]);

    core.releaseSources();
    fireEvent.click(within(form).getByRole("button", { name: "Try again" }));
    const base = (await within(form).findByLabelText("Base Source")) as HTMLSelectElement;
    expect(base.selectedOptions[0]!.textContent).toBe("Kit A");
    expect(create.getAttribute("aria-disabled")).toBeNull();
    fireEvent.click(create);
    await heading("Rack");
    expect(core.buildPosts).toEqual([{ name: "Rack", base_project_id: 1 }]);
  });

  it("a reopened dialog reads a failed list again", async () => {
    const core = scriptedCore({ sources: ["Kit A"], sourceList: "failed" });
    renderAt("/builds", core.fetchMock);
    fireEvent.click(await screen.findByRole("button", { name: "Create Build" }));
    let form = dialog("Create Build");
    expect(await within(form).findByText(/^Could not load the Library's Sources: /)).toBeTruthy();

    core.releaseSources();
    fireEvent.click(within(form).getByRole("button", { name: "Cancel" }));
    fireEvent.click(screen.getByRole("button", { name: "Create Build" }));
    form = dialog("Create Build");
    const base = (await within(form).findByLabelText("Base Source")) as HTMLSelectElement;
    expect(base.selectedOptions[0]!.textContent).toBe("Kit A");
    fireEvent.change(within(form).getByLabelText("Name"), { target: { value: "Rack" } });
    fireEvent.click(within(form).getByRole("button", { name: "Create" }));
    await heading("Rack");
    expect(core.buildPosts).toEqual([{ name: "Rack", base_project_id: 1 }]);
  });
});

describe("UI13-03: every empty state names the next useful action", () => {
  it("a Build with no layers offers Set base Source on its Sources tab", async () => {
    const core = scriptedCore({ sources: ["Kit A", "Kit B"], builds: [["Bare", null]] });
    renderAt("/builds/1000/sources", core.fetchMock);
    const form = await screen.findByRole("form", { name: "Set base Source" });
    expect(form.textContent).toContain("This Build has no layers, so it uses no Sources.");
    const set = within(form).getByRole("button", { name: "Set base Source" });
    expect(within(form).getByText("Choose a Source.")).toBeTruthy();
    fireEvent.click(set);
    await settle();
    expect(core.basePuts).toEqual([]);
    fireEvent.change(await within(form).findByLabelText("Base Source"), {
      target: { value: "2" },
    });
    fireEvent.click(set);
    const table = await screen.findByRole("table", { name: "Build Sources" });
    expect(within(table).getByRole("rowheader", { name: "Kit B" })).toBeTruthy();
    expect(core.basePuts).toEqual([{ id: 1000, body: { project_id: 2 } }]);
  });

  it("a Build with no layers and an empty Library leads to Library", async () => {
    const core = scriptedCore({ builds: [["Bare", null]] });
    renderAt("/builds/1000/sources", core.fetchMock);
    const form = await screen.findByRole("form", { name: "Set base Source" });
    fireEvent.click(await within(form).findByRole("link", { name: "Add a Source in Library" }));
    await heading("Library");
  });

  it("Library with no Source activity says how activity is recorded", async () => {
    const core = scriptedCore({ sources: ["Kit A"] });
    renderAt("/library", core.fetchMock);
    expect(
      await screen.findByText(/^No Source activity yet\. Sync a GitHub Source from its row/),
    ).toBeTruthy();
  });

  it("Farm with no printers links to adding one in Settings", async () => {
    const router = renderAt("/farm", scriptedCore().fetchMock);
    fireEvent.click(await screen.findByRole("link", { name: "Add a printer in Settings" }));
    await heading("Settings");
    expect(router.state.location.hash).toBe("#printers");
  });

  it("the command palette says what to try when nothing matches, and clears", async () => {
    const core = scriptedCore();
    renderAt("/today", core.fetchMock);
    fireEvent.click(screen.getByRole("button", { name: /Commands/ }));
    const palette = dialog("Command palette");
    const input = within(palette).getByRole("combobox");
    fireEvent.change(input, { target: { value: "frobnicate" } });
    const prompt = await within(palette).findByText(/^No command matches/);
    expect(prompt.textContent).toBe("No command matches “frobnicate”.");
    expect(within(palette).getByText(/Try a screen name such as Builds or Library/)).toBeTruthy();
    expect(within(palette).queryAllByRole("option")).toEqual([]);
    fireEvent.click(within(palette).getByRole("button", { name: "Clear search" }));
    expect((input as HTMLInputElement).value).toBe("");
    expect(document.activeElement).toBe(input);
    expect(within(palette).getAllByRole("option").length).toBeGreaterThan(0);
    expect(within(palette).queryByText(/^No command matches/)).toBeNull();
    // A destination name finds its command.
    fireEvent.change(input, { target: { value: "library" } });
    expect(
      within(palette)
        .getAllByRole("option")
        .map((o) => o.textContent),
    ).toEqual([expect.stringContaining("Go to Library")]);
  });

  it("Help lists the first-Build steps the empty Builds list shows", () => {
    renderAt("/today", scriptedCore().fetchMock);
    fireEvent.click(screen.getByRole("button", { name: "Help" }));
    const help = dialog("Help");
    expect(within(help).getByRole("heading", { name: "Your first Build" })).toBeTruthy();
    const items = within(help)
      .getAllByRole("listitem")
      .map((li) => li.textContent);
    for (const step of FIRST_BUILD_STEPS) expect(items).toContain(step);
  });
});

describe("Create Build request and refusals", () => {
  it("names the problem with a name the core would refuse", () => {
    expect(nameProblem("  ")).toBe("Enter a name.");
    expect(nameProblem("a".repeat(4096))).toBeNull();
    expect(nameProblem("a".repeat(4097))).toBe("Shorten the name to 4,096 characters or fewer.");
    expect(nameProblem("Rack \uD800")).toBe(
      "The name has a character the core cannot store. Retype it.",
    );
  });

  it("trims the name and sends the chosen base Source or null", () => {
    expect(buildRequest("  Rack ", 4)).toEqual({ name: "Rack", base_project_id: 4 });
    expect(buildRequest("Rack", null)).toEqual({ name: "Rack", base_project_id: null });
  });

  it("puts the core's Profile and Project wording in the UI's words", () => {
    expect(refusalText("Profile already exists: Rack")).toBe(
      "A Build with this name already exists. Choose another name.",
    );
    expect(refusalText("Project not found")).toBe(
      "That Source is no longer in the Library. Choose another.",
    );
    expect(refusalText('Source "Kit" is already attached to this build')).toBe(
      "That Source is already on this Build. Choose another.",
    );
    expect(refusalText("Profile name is too long")).toBe("Build name is too long");
    expect(refusalText("Request is invalid")).toBe("Request is invalid");
  });
});
