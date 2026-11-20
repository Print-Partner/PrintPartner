import type {
  Health,
  ProfileLayers,
  ProfileSummary,
  SourceAttachmentRequest,
  SourceList,
} from "@print-partner/rust-client";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
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

/** How the UI hears about a PUT whose write the core applies only on `commitLate`. */
type LateAnswer = "502" | "500" | "drop";

/** Builds 1000 and 1001, both with no layers, and a Library holding `sources`. */
function scriptedCore({
  sources: names = [],
  baseAnswer,
}: {
  sources?: string[];
  /** Replaces the set-base reply after the base layer is stored, as a lost answer. */
  baseAnswer?: () => Response;
} = {}) {
  const sources = names.map((name, i) => storedSource(i + 1, name));
  const builds = [storedBuild(1000, "Bare"), storedBuild(1001, "Second")];
  const layers = new Map<number, Layer[]>(builds.map((b) => [b.id, []]));
  const basePuts: SourceAttachmentRequest[] = [];
  /** Stores `source` as the base, as the core's PUT does: added, or moved if one exists. */
  const setBase = (buildId: number, source: Source) => {
    layers.set(buildId, [
      {
        id: buildId * 10,
        layer_order: 0,
        layer_type: "base",
        project_id: source.id,
        project_name: source.name,
      },
    ]);
  };
  const route = (path: string, init?: RequestInit): Response => {
    if (path === "/health") return json(health);
    if (path === "/api/v2/tasks") return json({ tasks: [], unavailable: [] });
    if (path === "/api/v1/sources") return json({ sources });
    const files = /^\/api\/v1\/sources\/(\d+)\/files$/.exec(path);
    if (files) return json({ source_id: Number(files[1]), revision_id: null, files: [] });
    const plan = /^\/api\/v2\/plans\/(\d+)(\/.*)?$/.exec(path);
    const build = builds.find((b) => b.id === Number(plan?.[1]));
    if (!plan || !build) return problem(404, "Not scripted");
    const rest = plan[2] ?? "";
    if (rest === "") return json(build);
    if (rest === "/layers") return json({ profile_id: build.id, layers: layers.get(build.id) });
    if (rest === "/layers/base" && init?.method === "PUT") {
      const body = JSON.parse(String(init.body)) as SourceAttachmentRequest;
      basePuts.push(body);
      const source = sources.find((s) => s.id === body.project_id);
      if (!source) return problem(404, "Project not found");
      setBase(build.id, source);
      return baseAnswer ? baseAnswer() : json({ profile_id: build.id, layers: layers.get(build.id) });
    }
    if (rest === "/filament-shortfalls") {
      return json({ kind: "module_off", module: "spoolman", reason: "default_off" });
    }
    if (rest === "/workflow") return problem(503, "Workflow not scripted");
    return problem(404, "Not scripted");
  };
  let layerMode: "held" | "failed" | undefined;
  const heldLayers: (() => void)[] = [];
  let holdPut = false;
  const heldPuts: (() => void)[] = [];
  let late: LateAnswer | undefined;
  const lateWrites: (() => void)[] = [];
  let sourcesFail = false;
  const fetchMock = vi.fn((path: string, init?: RequestInit): Promise<Response> => {
    if (init?.method === "PUT" && late) {
      // The UI hears a failure now; the core writes only when `commitLate` runs.
      const answer = late;
      late = undefined;
      lateWrites.push(() => void route(path, init));
      if (answer === "drop") return Promise.reject(new TypeError("Failed to fetch"));
      return Promise.resolve(problem(Number(answer), "Upstream failed"));
    }
    if (init?.method === "PUT" && holdPut) {
      return new Promise((resolve) => heldPuts.push(() => resolve(route(path, init))));
    }
    if (/^\/api\/v2\/plans\/\d+\/layers$/.test(path) && !init?.method && layerMode) {
      if (layerMode === "failed") return Promise.resolve(problem(503, "Layers unreadable"));
      return new Promise((resolve) => heldLayers.push(() => resolve(route(path, init))));
    }
    if (path === "/api/v1/sources" && sourcesFail) {
      return Promise.resolve(problem(500, "Library unreadable"));
    }
    return Promise.resolve(route(path, init));
  });
  return {
    fetchMock,
    basePuts,
    /** Sets Build 1000's base without this UI, as another window would. */
    setBaseElsewhere: (name: string) => setBase(1000, sources.find((s) => s.name === name)!),
    /** Holds or fails every later read of a Build's layers until `releaseLayers`. */
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
    /** The next PUT fails at the UI as `answer`; the core writes it only on `commitLate`. */
    answerLate: (answer: LateAnswer) => (late = answer),
    commitLate: () => {
      for (const write of lateWrites.splice(0)) write();
    },
    /** The Source list answers 500 until `recoverSources`. */
    failSources: () => (sourcesFail = true),
    recoverSources: () => (sourcesFail = false),
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

const unconfirmedPanel = () => screen.findByRole("region", { name: "Unconfirmed base Source" });

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
    fireEvent.click(within(form).getByRole("button", { name: "Set base Source" }));
    // The Build read retries before it gives up.
    await vi.advanceTimersByTimeAsync(10_000);
    const panel = await unconfirmedPanel();
    expect(within(panel).getByRole("alert").textContent).toMatch(
      /^Could not confirm the base Source was set: /,
    );
    expect(
      await within(panel).findByText(
        "Could not read the Build again. Press Read again to see whether the base Source was set.",
      ),
    ).toBeTruthy();
    fireEvent.click(within(panel).getByRole("button", { name: "Set a base anyway" }));
    await settle();
    expect(screen.queryByRole("form")).toBeNull();
    expect(core.basePuts).toHaveLength(1);

    core.releaseLayers();
    fireEvent.click(within(panel).getByRole("button", { name: "Read again" }));
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

  it("shows no form on return after a send with a lost answer", async () => {
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
    // Leaving the tab cancelled the send's re-read, so the send settled unconfirmed.
    const panel = await unconfirmedPanel();
    expect(within(panel).getByText("Reading the Build again.")).toBeTruthy();
    expect(screen.queryByRole("form")).toBeNull();

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

// The council finding on #1880 at 5763056: the first PUT's answer is a 502 while the write is
// still on its way to the core. The send settles, a read shows zero layers, and a second PUT
// from a reopened form would replace the base the first one sets when it lands.
describe("a send the UI never saw confirmed", () => {
  const puts = (core: ReturnType<typeof scriptedCore>) =>
    core.fetchMock.mock.calls.filter(([, init]) => init?.method === "PUT").length;

  /** Sends Kit A with a late write, waits for the panel and a read that shows no base. */
  async function sendUnconfirmed(answer: LateAnswer) {
    const core = scriptedCore({ sources: ["Kit A", "Kit B"] });
    const router = renderAt("/builds/1000/sources", core.fetchMock);
    const form = await readyForm();
    fireEvent.change(within(form).getByLabelText("Base Source"), { target: { value: "1" } });
    core.answerLate(answer);
    fireEvent.click(within(form).getByRole("button", { name: "Set base Source" }));
    const panel = await unconfirmedPanel();
    await within(panel).findByText("The last read shows no base Source yet.");
    return { core, router };
  }

  it.each(["502", "500", "drop"] as const)(
    "after a %s, leaving and coming back reads no base but opens no form, and keeps the late base",
    async (answer) => {
      const { core } = await sendUnconfirmed(answer);
      fireEvent.click(tab("Overview"));
      await screen.findByRole("link", { name: "Choose its base Source" });
      fireEvent.click(tab("Sources"));
      const panel = await unconfirmedPanel();
      expect(within(panel).getByRole("alert").textContent).toMatch(
        /^Could not confirm the base Source was set: .*The core may still set Kit A/,
      );
      await within(panel).findByText("The last read shows no base Source yet.");
      expect(screen.queryByRole("form")).toBeNull();

      core.commitLate();
      fireEvent.click(within(panel).getByRole("button", { name: "Read again" }));
      const table = await screen.findByRole("table", { name: "Build Sources" });
      expect(within(table).getByRole("rowheader", { name: "Kit A" })).toBeTruthy();
      expect(core.basePuts).toEqual([{ project_id: 1 }]);
      expect(puts(core)).toBe(1);
    },
  );

  it("Set a base anyway states what the earlier send may do, and sends once after a new choice", async () => {
    const { core } = await sendUnconfirmed("502");
    expect(screen.queryByRole("form")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Set a base anyway" }));
    const form = await screen.findByRole("form", { name: "Set base anyway" });
    expect(form.textContent).toContain(
      "The earlier send of Kit A was not confirmed. If that send reaches the core later, whichever of the two arrives last becomes the base and replaces the other. Choose the Source again to set it now.",
    );
    const set = within(form).getByRole("button", { name: "Set base anyway" });
    expect(set.getAttribute("aria-disabled")).toBe("true");
    fireEvent.click(set);
    await settle();
    expect(puts(core)).toBe(1);

    fireEvent.change(within(form).getByLabelText("Base Source"), { target: { value: "2" } });
    fireEvent.click(set);
    const table = await screen.findByRole("table", { name: "Build Sources" });
    expect(within(table).getByRole("rowheader", { name: "Kit B" })).toBeTruthy();
    expect(core.basePuts).toEqual([{ project_id: 2 }]);
    expect(puts(core)).toBe(2);
  });

  it("Cancel goes back to the panel without sending", async () => {
    const { core } = await sendUnconfirmed("drop");
    fireEvent.click(screen.getByRole("button", { name: "Set a base anyway" }));
    const form = await screen.findByRole("form", { name: "Set base anyway" });
    fireEvent.change(within(form).getByLabelText("Base Source"), { target: { value: "2" } });
    fireEvent.click(within(form).getByRole("button", { name: "Cancel" }));
    await unconfirmedPanel();
    expect(screen.queryByRole("form")).toBeNull();
    expect(puts(core)).toBe(1);
  });

  it("does not hold back another Build", async () => {
    const { core, router } = await sendUnconfirmed("502");
    await act(() => router.navigate("/builds/1001/sources"));
    const form = await readyForm();
    fireEvent.change(within(form).getByLabelText("Base Source"), { target: { value: "2" } });
    fireEvent.click(within(form).getByRole("button", { name: "Set base Source" }));
    const table = await screen.findByRole("table", { name: "Build Sources" });
    expect(within(table).getByRole("rowheader", { name: "Kit B" })).toBeTruthy();
    expect(puts(core)).toBe(2);

    await act(() => router.navigate("/builds/1000/sources"));
    await unconfirmedPanel();
    expect(screen.queryByRole("form")).toBeNull();
  });
});

describe("Set base Source with a failed Source list", () => {
  it("offers Try again, sends nothing, and sends one PUT after the list loads", async () => {
    const core = scriptedCore({ sources: ["Kit A", "Kit B"] });
    core.failSources();
    renderAt("/builds/1000/sources", core.fetchMock);
    const form = await screen.findByRole("form", { name: "Set base Source" });
    const retry = await within(form).findByRole("button", { name: "Try again" });
    expect(within(form).getByText(/^Could not load the Library's Sources: /)).toBeTruthy();
    fireEvent.click(within(form).getByRole("button", { name: "Set base Source" }));
    await settle();
    expect(core.basePuts).toEqual([]);

    core.recoverSources();
    fireEvent.click(retry);
    fireEvent.change(await within(form).findByLabelText("Base Source"), { target: { value: "1" } });
    fireEvent.click(within(form).getByRole("button", { name: "Set base Source" }));
    await waitFor(() => expect(core.basePuts).toEqual([{ project_id: 1 }]));
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
