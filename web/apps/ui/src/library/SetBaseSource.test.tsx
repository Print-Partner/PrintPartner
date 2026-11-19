import type {
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
import { routes } from "../routes";
import { refusalText } from "../screen/SourceSelect";

// GRE-1401 slice a: a scripted core on disposable, in-memory data. It keeps Sources, Builds
// and layers, and answers set-base as Rust does: an unknown Source is a 404 with the core's
// wording, and a PUT on a Build that has a base moves the base to the new Source.
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

/** One Build, 1000, with no layers, and a Library holding `sources`. */
function scriptedCore({
  sources: names = [],
  baseAnswer,
}: {
  sources?: string[];
  /** Replaces the set-base reply after the base layer is stored, as a lost answer. */
  baseAnswer?: () => Response;
} = {}) {
  const sources = names.map((name, i) => storedSource(i + 1, name));
  const build = storedBuild(1000, "Bare");
  let layers: Layer[] = [];
  const basePuts: SourceAttachmentRequest[] = [];
  /** Stores `source` as the base, as the core's PUT does: added, or moved if one exists. */
  const setBase = (source: Source) => {
    layers = [
      {
        id: 10_000,
        layer_order: 0,
        layer_type: "base",
        project_id: source.id,
        project_name: source.name,
      },
    ];
  };
  const route = (path: string, init?: RequestInit): Response => {
    if (path === "/health") return json(health);
    if (path === "/api/v2/tasks") return json({ tasks: [], unavailable: [] });
    if (path === "/api/v1/sources") return json({ sources });
    const files = /^\/api\/v1\/sources\/(\d+)\/files$/.exec(path);
    if (files) return json({ source_id: Number(files[1]), revision_id: null, files: [] });
    if (path === "/api/v2/plans/1000") return json(build);
    if (path === "/api/v2/plans/1000/layers") return json({ profile_id: 1000, layers });
    if (path === "/api/v2/plans/1000/layers/base" && init?.method === "PUT") {
      const body = JSON.parse(String(init.body)) as SourceAttachmentRequest;
      basePuts.push(body);
      const source = sources.find((s) => s.id === body.project_id);
      if (!source) return problem(404, "Project not found");
      setBase(source);
      return baseAnswer ? baseAnswer() : json({ profile_id: 1000, layers });
    }
    if (path === "/api/v2/plans/1000/filament-shortfalls") {
      return json({ kind: "module_off", module: "spoolman", reason: "default_off" });
    }
    if (path === "/api/v2/plans/1000/workflow") return problem(503, "Workflow not scripted");
    return problem(404, "Not scripted");
  };
  let layerMode: "held" | "failed" | undefined;
  const heldLayers: (() => void)[] = [];
  let holdPut = false;
  const heldPuts: (() => void)[] = [];
  const fetchMock = vi.fn((path: string, init?: RequestInit): Promise<Response> => {
    if (init?.method === "PUT" && holdPut) {
      return new Promise((resolve) => heldPuts.push(() => resolve(route(path, init))));
    }
    if (path === "/api/v2/plans/1000/layers" && !init?.method && layerMode) {
      if (layerMode === "failed") return Promise.resolve(problem(503, "Layers unreadable"));
      return new Promise((resolve) => heldLayers.push(() => resolve(route(path, init))));
    }
    return Promise.resolve(route(path, init));
  });
  return {
    fetchMock,
    basePuts,
    /** Sets the base without this UI, as another window would. */
    setBaseElsewhere: (name: string) => setBase(sources.find((s) => s.name === name)!),
    /** Holds or fails every later read of the Build's layers until `releaseLayers`. */
    holdLayers: () => (layerMode = "held"),
    failLayers: () => (layerMode = "failed"),
    releaseLayers: () => {
      layerMode = undefined;
      for (const answerHeld of heldLayers.splice(0)) answerHeld();
    },
    /** Holds the next PUT before the core reads it; `releasePut` stores it and answers. */
    holdPut: () => (holdPut = true),
    releasePut: () => {
      holdPut = false;
      for (const answerHeld of heldPuts.splice(0)) answerHeld();
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
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

/** A send happens a microtask or more after the click, so "nothing sent" waits first. */
const settle = () => new Promise((resolve) => setTimeout(resolve, 50));

const tab = (name: string) =>
  within(screen.getByRole("navigation", { name: "Build sections" })).getByRole("link", { name });

/** The form once its mount-time read of the Build has finished. */
async function readyForm() {
  const form = await screen.findByRole("form", { name: "Set base Source" });
  await within(form).findByText("Choose a Source.");
  return form;
}

describe("UI13-03: a Build with no layers names Set base Source", () => {
  it("offers Set base Source on the Sources tab and sends one PUT for the chosen Source", async () => {
    const core = scriptedCore({ sources: ["Kit A", "Kit B"] });
    renderAt("/builds/1000/sources", core.fetchMock);
    const form = await readyForm();
    expect(form.textContent).toContain("This Build has no layers, so it uses no Sources.");
    const set = within(form).getByRole("button", { name: "Set base Source" });
    expect(set.getAttribute("aria-disabled")).toBe("true");
    fireEvent.click(set);
    await settle();
    expect(core.basePuts).toEqual([]);
    fireEvent.change(within(form).getByLabelText("Base Source"), { target: { value: "2" } });
    expect(set.getAttribute("aria-disabled")).toBeNull();
    fireEvent.click(set);
    const table = await screen.findByRole("table", { name: "Build Sources" });
    expect(within(table).getByRole("rowheader", { name: "Kit B" })).toBeTruthy();
    expect(core.basePuts).toEqual([{ project_id: 2 }]);
  });

  it("Overview with no layers links to the Sources tab's form", async () => {
    const core = scriptedCore({ sources: ["Kit A"] });
    renderAt("/builds/1000", core.fetchMock);
    fireEvent.click(await screen.findByRole("link", { name: "Choose its base Source" }));
    await readyForm();
  });

  it("with an empty Library it leads to Library", async () => {
    const core = scriptedCore();
    renderAt("/builds/1000/sources", core.fetchMock);
    const form = await screen.findByRole("form", { name: "Set base Source" });
    fireEvent.click(await within(form).findByRole("link", { name: "Add a Source in Library" }));
    await screen.findByRole("heading", { level: 1, name: "Library" });
  });

  it("stays closed while the Build is read again after a lost answer", async () => {
    const core = scriptedCore({
      sources: ["Kit A", "Kit B"],
      baseAnswer: () => problem(502, "Bad Gateway"),
    });
    renderAt("/builds/1000/sources", core.fetchMock);
    const form = await readyForm();
    const select = within(form).getByLabelText("Base Source");
    fireEvent.change(select, { target: { value: "2" } });
    core.holdLayers();
    const set = within(form).getByRole("button", { name: "Set base Source" });
    fireEvent.click(set);
    await waitFor(() => expect(core.basePuts).toEqual([{ project_id: 2 }]));
    await settle();
    // The PUT committed but its answer was lost; a second one would replace Kit B.
    fireEvent.change(select, { target: { value: "1" } });
    fireEvent.click(set);
    await settle();
    expect(core.basePuts).toHaveLength(1);
    expect(within(form).getByText("Setting the base Source.")).toBeTruthy();
    expect(set.getAttribute("aria-disabled")).toBe("true");

    core.releaseLayers();
    const table = await screen.findByRole("table", { name: "Build Sources" });
    expect(within(table).getByRole("rowheader", { name: "Kit B" })).toBeTruthy();
    expect(core.basePuts).toHaveLength(1);
  });

  it("a lost answer and a failed re-read keep the form closed until Read again", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const core = scriptedCore({
      sources: ["Kit A", "Kit B"],
      baseAnswer: () => problem(502, "Bad Gateway"),
    });
    renderAt("/builds/1000/sources", core.fetchMock);
    const form = await readyForm();
    fireEvent.change(within(form).getByLabelText("Base Source"), { target: { value: "2" } });
    core.failLayers();
    const set = within(form).getByRole("button", { name: "Set base Source" });
    fireEvent.click(set);
    expect((await within(form).findByRole("alert")).textContent).toMatch(
      /^Could not confirm the base Source was set: /,
    );
    // The Build read retries before it gives up.
    await vi.advanceTimersByTimeAsync(10_000);
    expect(
      await within(form).findByText(
        "Could not read the Build again. Press Read again to see whether the base Source was set.",
      ),
    ).toBeTruthy();
    fireEvent.click(set);
    await settle();
    expect(core.basePuts).toHaveLength(1);

    core.releaseLayers();
    fireEvent.click(within(form).getByRole("button", { name: "Read again" }));
    const table = await screen.findByRole("table", { name: "Build Sources" });
    expect(within(table).getByRole("rowheader", { name: "Kit B" })).toBeTruthy();
    expect(core.basePuts).toHaveLength(1);
  });
});

// The council finding on #1840 at 75415f38: leaving the Sources tab and coming back while the
// layers were being read again remounted the form on cached zero layers, and a second PUT
// replaced the committed base.
describe("Set base Source after leaving the Sources tab and coming back", () => {
  it("shows the committed base from the PUT's answer while the re-read is held", async () => {
    const core = scriptedCore({ sources: ["Kit A", "Kit B"] });
    renderAt("/builds/1000/sources", core.fetchMock);
    const form = await readyForm();
    fireEvent.change(within(form).getByLabelText("Base Source"), { target: { value: "2" } });
    core.holdLayers();
    fireEvent.click(within(form).getByRole("button", { name: "Set base Source" }));
    await waitFor(() => expect(core.basePuts).toEqual([{ project_id: 2 }]));

    fireEvent.click(tab("Overview"));
    expect(await screen.findByText("Kit B")).toBeTruthy();
    fireEvent.click(tab("Sources"));
    const table = await screen.findByRole("table", { name: "Build Sources" });
    expect(within(table).getByRole("rowheader", { name: "Kit B" })).toBeTruthy();
    expect(screen.queryByRole("form", { name: "Set base Source" })).toBeNull();

    core.releaseLayers();
    await settle();
    expect(screen.queryByRole("form", { name: "Set base Source" })).toBeNull();
    expect(core.basePuts).toEqual([{ project_id: 2 }]);
  });

  it("keeps a remounted form closed while a send with a lost answer is read again", async () => {
    const core = scriptedCore({
      sources: ["Kit A", "Kit B"],
      baseAnswer: () => problem(502, "Bad Gateway"),
    });
    renderAt("/builds/1000/sources", core.fetchMock);
    const form = await readyForm();
    fireEvent.change(within(form).getByLabelText("Base Source"), { target: { value: "2" } });
    core.holdLayers();
    fireEvent.click(within(form).getByRole("button", { name: "Set base Source" }));
    await waitFor(() => expect(core.basePuts).toHaveLength(1));

    fireEvent.click(tab("Overview"));
    await screen.findByRole("link", { name: "Choose its base Source" });
    fireEvent.click(tab("Sources"));
    // Leaving the tab cancelled the send's re-read; the new form waits for its own.
    const again = await screen.findByRole("form", { name: "Set base Source" });
    fireEvent.change(await within(again).findByLabelText("Base Source"), {
      target: { value: "1" },
    });
    const set = within(again).getByRole("button", { name: "Set base Source" });
    fireEvent.click(set);
    await settle();
    expect(core.basePuts).toHaveLength(1);
    expect(set.getAttribute("aria-disabled")).toBe("true");
    expect(within(again).getByText("Reading the Build again.")).toBeTruthy();

    core.releaseLayers();
    const table = await screen.findByRole("table", { name: "Build Sources" });
    expect(within(table).getByRole("rowheader", { name: "Kit B" })).toBeTruthy();
    expect(core.basePuts).toEqual([{ project_id: 2 }]);
  });

  it("keeps a remounted form closed while the PUT itself is still in flight", async () => {
    const core = scriptedCore({ sources: ["Kit A", "Kit B"] });
    renderAt("/builds/1000/sources", core.fetchMock);
    const form = await readyForm();
    fireEvent.change(within(form).getByLabelText("Base Source"), { target: { value: "2" } });
    core.holdPut();
    fireEvent.click(within(form).getByRole("button", { name: "Set base Source" }));
    await waitFor(() =>
      expect(core.fetchMock.mock.calls.some(([, init]) => init?.method === "PUT")).toBe(true),
    );

    fireEvent.click(tab("Overview"));
    await screen.findByRole("link", { name: "Choose its base Source" });
    fireEvent.click(tab("Sources"));
    const again = await screen.findByRole("form", { name: "Set base Source" });
    // The layers read on return still have no base, but the send is in flight.
    await settle();
    fireEvent.change(await within(again).findByLabelText("Base Source"), {
      target: { value: "1" },
    });
    fireEvent.click(within(again).getByRole("button", { name: "Set base Source" }));
    await settle();
    expect(core.fetchMock.mock.calls.filter(([, init]) => init?.method === "PUT")).toHaveLength(1);
    expect(within(again).getByText("Setting the base Source.")).toBeTruthy();

    core.releasePut();
    const table = await screen.findByRole("table", { name: "Build Sources" });
    expect(within(table).getByRole("rowheader", { name: "Kit B" })).toBeTruthy();
    expect(core.basePuts).toEqual([{ project_id: 2 }]);
  });

  it("reads the Build again on return before it opens, so a base set elsewhere is kept", async () => {
    const core = scriptedCore({ sources: ["Kit A", "Kit B"] });
    renderAt("/builds/1000/sources", core.fetchMock);
    await readyForm();
    fireEvent.click(tab("Overview"));
    await screen.findByRole("link", { name: "Choose its base Source" });

    core.setBaseElsewhere("Kit A");
    core.holdLayers();
    fireEvent.click(tab("Sources"));
    const form = await screen.findByRole("form", { name: "Set base Source" });
    fireEvent.change(await within(form).findByLabelText("Base Source"), {
      target: { value: "2" },
    });
    fireEvent.click(within(form).getByRole("button", { name: "Set base Source" }));
    await settle();
    expect(core.basePuts).toEqual([]);
    expect(within(form).getByText("Reading the Build again.")).toBeTruthy();

    core.releaseLayers();
    const table = await screen.findByRole("table", { name: "Build Sources" });
    expect(within(table).getByRole("rowheader", { name: "Kit A" })).toBeTruthy();
    expect(core.basePuts).toEqual([]);
  });
});

describe("refusalText", () => {
  it("puts the core's Project wording in the UI's words", () => {
    expect(refusalText("Project not found")).toBe(
      "That Source is no longer in the Library. Choose another.",
    );
    expect(refusalText('Source "Kit" is already attached to this build')).toBe(
      "That Source is already on this Build. Choose another.",
    );
    expect(refusalText("Request is invalid")).toBe("Request is invalid");
  });
});
