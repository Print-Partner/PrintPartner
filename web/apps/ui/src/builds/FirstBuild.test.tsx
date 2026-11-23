import type {
  CreateBuildRequest,
  CreateSourceRequest,
  ProfileLayers,
  ProfileSummary,
  SourceList,
} from "@print-partner/rust-client";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { createMemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App, createQueryClient } from "../App";
import { routes } from "../routes";
import { createStore, resetCreateBuild } from "./createBuild";
import { health } from "./sendQueueFake.test-utils";
import { FIRST_BUILD_STEPS } from "./planCopy";

// GRE-1401: a scripted core on disposable, in-memory data. It keeps Sources, Builds and layers
// and answers createBuild as Rust does (docs/gre-792-builds-http.md): a name already in use is
// 400, the 65th Build is 409, and a refusal writes nothing.
const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
const problem = (status: number, detail: string, type = "validation") =>
  new Response(
    JSON.stringify({ type: `urn:print-partner:problem:${type}`, title: "Refused", status, detail }),
    { status, headers: { "content-type": "application/problem+json" } },
  );

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
  builds?: string[];
  /** The first create's reply, before any check; `write` stores the Build as the core would. */
  answer?: (write: () => ProfileSummary) => Promise<Response>;
  /** List read `read` (from 1): a reply, or undefined for `snapshot`, the core's at arrival. */
  list?: (read: number, snapshot: Response) => Promise<Response> | Response | undefined;
}

function scriptedCore(seed: Seed = {}) {
  const sources = (seed.sources ?? ["Kit A"]).map((name, i) => storedSource(i + 1, name));
  const builds: ProfileSummary[] = [];
  const layers = new Map<number, Layer[]>();
  const sourcePosts: CreateSourceRequest[] = [];
  const buildPosts: CreateBuildRequest[] = [];
  let listReads = 0;
  let answer = seed.answer;
  const addBuild = (name: string, sourceId: number | null) => {
    const build = storedBuild(1000 + builds.length, name);
    builds.push(build);
    const source = sources.find((s) => s.id === sourceId);
    const base: Layer[] = source
      ? [
          {
            id: build.id * 10,
            layer_order: 0,
            layer_type: "base",
            project_id: source.id,
            project_name: source.name,
          },
        ]
      : [];
    layers.set(build.id, base);
    return build;
  };
  for (const name of seed.builds ?? []) addBuild(name, null);
  const createBuild = (request: CreateBuildRequest): Promise<Response> => {
    buildPosts.push(request);
    const write = () => addBuild(request.name, request.base_project_id ?? null);
    if (answer) {
      const once = answer;
      answer = undefined;
      return once(write);
    }
    if (builds.some((b) => b.name === request.name)) {
      return Promise.resolve(problem(400, `Profile already exists: ${request.name}`));
    }
    if (builds.length >= 64) {
      return Promise.resolve(
        problem(409, "Build limit reached: an installation holds at most 64 Builds", "conflict"),
      );
    }
    const build = write();
    return Promise.resolve(json({ ...build, layers: layers.get(build.id) }));
  };
  const sourceRoute = (path: string, method: string, body: unknown): Response | null => {
    if (path.startsWith("/api/v1/sources/activity")) return json({ events: [] });
    if (path === "/api/v1/sources" && method === "POST") {
      const request = body as CreateSourceRequest;
      sourcePosts.push(request);
      const source = storedSource(sources.length + 1, request.name);
      sources.push(source);
      return json(source);
    }
    if (path === "/api/v1/sources") return json({ sources });
    const files = /^\/api\/v1\/sources\/(\d+)\/files$/.exec(path);
    return files && json({ source_id: Number(files[1]), revision_id: null, files: [] });
  };
  const planRoute = (path: string, method: string, body: unknown): Promise<Response> | Response => {
    if (path === "/api/v2/plans" && method === "POST") {
      return createBuild(body as CreateBuildRequest);
    }
    if (path === "/api/v2/plans") {
      listReads += 1;
      const snapshot = json({ profiles: [...builds] });
      return seed.list?.(listReads, snapshot) ?? snapshot;
    }
    const plan = /^\/api\/v2\/plans\/(\d+)(\/layers|\/workflow|\/filament-shortfalls)?$/.exec(path);
    const build = builds.find((b) => b.id === Number(plan?.[1]));
    if (!plan || !build) return problem(404, "Profile not found", "not-found");
    if (plan[2] === "/layers") return json({ profile_id: build.id, layers: layers.get(build.id) });
    if (plan[2] === "/filament-shortfalls") {
      return json({ kind: "module_off", module: "spoolman", reason: "default_off" });
    }
    if (plan[2] === "/workflow") return problem(503, "Workflow not scripted", "unavailable");
    return json(build);
  };
  const route = (path: string, init?: RequestInit): Promise<Response> | Response => {
    const method = init?.method ?? "GET";
    const body: unknown = init?.body ? JSON.parse(String(init.body)) : undefined;
    if (path === "/health") return json(health("off"));
    if (path === "/api/v2/tasks") return json({ tasks: [], unavailable: [] });
    return sourceRoute(path, method, body) ?? planRoute(path, method, body);
  };
  const fetchMock = vi.fn((path: string, init?: RequestInit) => Promise.resolve(route(path, init)));
  return { fetchMock, builds, sourcePosts, buildPosts, listReads: () => listReads };
}

let lastClient: ReturnType<typeof createQueryClient>;

function renderAt(path: string, core: ReturnType<typeof scriptedCore>) {
  vi.stubGlobal("fetch", core.fetchMock);
  const router = createMemoryRouter(routes, { initialEntries: [path] });
  const queryClient = createQueryClient();
  queryClient.setDefaultOptions({
    queries: { ...queryClient.getDefaultOptions().queries, retry: false },
  });
  render(<App router={router} queryClient={queryClient} />);
  lastClient = queryClient;
  return router;
}

afterEach(() => {
  cleanup();
  resetCreateBuild();
  localStorage.clear();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

/** A Response as `fetch` returns one after following a redirect. */
const redirected = (response: Response) =>
  Object.defineProperty(response, "redirected", { value: true });

const pause = () => new Promise((resolve) => setTimeout(resolve, 50));
const dialog = () => screen.getByRole("dialog", { name: "Create Build" });
const heading = (name: string) => screen.findByRole("heading", { level: 1, name });

async function openWithName(name: string) {
  fireEvent.click(await screen.findByRole("button", { name: "Create Build" }));
  await within(dialog()).findByLabelText("Base Source");
  fireEvent.change(within(dialog()).getByLabelText("Name"), { target: { value: name } });
  return within(dialog()).getByRole("button", { name: "Create" });
}

describe("UI13-01: first run walks to a first Source and Build", () => {
  it("goes from an empty install to a Build on a new Source", async () => {
    const core = scriptedCore({ sources: [] });
    const router = renderAt("/builds", core);

    expect(await screen.findByText(/^No Builds yet\. Press Create Build/)).toBeTruthy();
    const steps = screen.getByRole("list", { name: "Your first Build" });
    expect(
      within(steps)
        .getAllByRole("listitem")
        .map((li) => li.textContent),
    ).toEqual([...FIRST_BUILD_STEPS]);

    fireEvent.click(screen.getByRole("button", { name: "Create Build" }));
    const toLibrary = await within(dialog()).findByRole("link", {
      name: "Add a Source in Library",
    });
    expect(within(dialog()).queryByRole("combobox")).toBeNull();
    fireEvent.click(toLibrary);
    await heading("Library");
    expect(screen.queryByRole("dialog")).toBeNull();

    expect(await screen.findByText(/^No Sources yet\. Press Add Source/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Add Source" }));
    const add = screen.getByRole("dialog", { name: "Add Source" });
    fireEvent.change(within(add).getByLabelText("Name"), { target: { value: "Disposable Kit" } });
    fireEvent.click(within(add).getByRole("button", { name: "Create" }));
    await screen.findByRole("rowheader", { name: /Disposable Kit/ });
    expect(core.sourcePosts).toEqual([{ name: "Disposable Kit", source_kind: "archive" }]);

    fireEvent.click(screen.getByRole("link", { name: "Builds" }));
    await heading("Builds");
    const create = await openWithName(" First Build ");
    const base = within(dialog()).getByLabelText("Base Source") as HTMLSelectElement;
    expect([...base.options].map((o) => o.textContent)).toEqual([
      "No base Source",
      "Disposable Kit",
    ]);
    expect(base.selectedOptions[0]!.textContent).toBe("Disposable Kit");
    fireEvent.click(create);

    await heading("First Build");
    expect(router.state.location.pathname).toBe("/builds/1000");
    expect(core.buildPosts).toEqual([{ name: "First Build", base_project_id: 1 }]);
    const layers = screen.getByRole("list", { name: "Layers" });
    expect(layers.textContent).toContain("Disposable Kit");
    expect(screen.queryByRole("dialog")).toBeNull();

    fireEvent.click(screen.getByRole("link", { name: "Builds" }));
    const table = await screen.findByRole("table", { name: "Builds" });
    expect(within(table).getByRole("link", { name: "First Build" })).toBeTruthy();
  });

  it("creates a Build with no base Source, and its Overview leads to Sources", async () => {
    const core = scriptedCore();
    renderAt("/builds", core);
    const create = await openWithName("Loose");
    fireEvent.change(within(dialog()).getByLabelText("Base Source"), { target: { value: "" } });
    fireEvent.click(create);
    await heading("Loose");
    expect(core.buildPosts).toEqual([{ name: "Loose", base_project_id: null }]);
    fireEvent.click(screen.getByRole("link", { name: "Choose its base Source" }));
    await heading("Sources");
  });
});

describe("Create Build answers", () => {
  it("a name in use shows the refusal, names that Build, and sends once", async () => {
    const core = scriptedCore({ builds: ["Taken"] });
    const router = renderAt("/builds", core);
    const create = await openWithName("Taken");
    fireEvent.click(create);
    fireEvent.click(create);
    expect((await within(dialog()).findByRole("alert")).textContent).toBe(
      "A Build with this name already exists. Open it, or choose another name. Open Taken",
    );
    expect(core.buildPosts).toHaveLength(1);
    expect(router.state.location.pathname).toBe("/builds");
    fireEvent.change(within(dialog()).getByLabelText("Name"), { target: { value: "Taken 2" } });
    expect(within(dialog()).queryByRole("alert")).toBeNull();
  });

  it("the Build limit shows the core's detail and writes nothing", async () => {
    const core = scriptedCore({
      builds: Array.from({ length: 64 }, (_, i) => `B${i}`),
    });
    renderAt("/builds", core);
    fireEvent.click(await openWithName("One more"));
    expect((await within(dialog()).findByRole("alert")).textContent).toBe(
      "Build limit reached: an installation holds at most 64 Builds",
    );
    expect(core.builds).toHaveLength(64);
  });

  it("an empty name and a name over 4,096 characters are not sent", async () => {
    const core = scriptedCore();
    renderAt("/builds", core);
    const create = await openWithName("   ");
    expect(within(dialog()).getByText("Enter a name.")).toBeTruthy();
    fireEvent.change(within(dialog()).getByLabelText("Name"), {
      target: { value: "\u{1f9f5}".repeat(4097) },
    });
    expect(
      within(dialog()).getByText("Shorten the name to 4,096 characters or fewer."),
    ).toBeTruthy();
    expect(create.getAttribute("aria-disabled")).toBe("true");
    fireEvent.click(create);
    await pause();
    expect(core.buildPosts).toEqual([]);
  });

  it("disables Create while the create is in flight", async () => {
    let reply: (response: Response) => void = () => {};
    const core = scriptedCore({
      answer: () => new Promise((resolve) => (reply = resolve)),
    });
    renderAt("/builds", core);
    const create = await openWithName("Slow");
    fireEvent.click(create);
    expect(await within(dialog()).findByText("Creating the Build.")).toBeTruthy();
    expect(create.getAttribute("aria-disabled")).toBe("true");
    fireEvent.click(create);
    fireEvent.submit(create.closest("form")!);
    await pause();
    expect(core.buildPosts).toHaveLength(1);
    reply(problem(400, "Project not found"));
    expect((await within(dialog()).findByRole("alert")).textContent).toBe(
      "That Source is no longer in the Library. Choose another.",
    );
  });
});

describe("Create Build repeat rule: an unconfirmed answer is checked, never resent", () => {
  it("a lost answer after the write finds the Build in the list and offers to open it", async () => {
    const core = scriptedCore({
      answer: (write) => {
        write();
        return Promise.resolve(problem(502, "Bad Gateway", "unavailable"));
      },
    });
    const router = renderAt("/builds", core);
    fireEvent.click(await openWithName("Maybe"));
    const alert = await within(dialog()).findByRole("alert");
    expect(alert.textContent).toBe(
      "The answer to Create was lost, but the Builds list now holds this Build, so it was created. Open Maybe",
    );
    await pause();
    expect(core.buildPosts).toHaveLength(1);
    fireEvent.click(within(alert).getByRole("link", { name: "Open Maybe" }));
    await heading("Maybe");
    expect(router.state.location.pathname).toBe("/builds/1000");
    fireEvent.click(screen.getByRole("link", { name: "Builds" }));
    fireEvent.click(await screen.findByRole("button", { name: "Create Build" }));
    expect(within(dialog()).queryByRole("alert")).toBeNull();
    expect((within(dialog()).getByLabelText("Name") as HTMLInputElement).value).toBe("");
  });

  it("a dropped connection before the write says so, and only a press sends again", async () => {
    const core = scriptedCore({
      answer: () => Promise.reject(new TypeError("Failed to fetch")),
    });
    renderAt("/builds", core);
    const create = await openWithName("Second try");
    fireEvent.click(create);
    expect((await within(dialog()).findByRole("alert")).textContent).toBe(
      "Could not confirm the Build was created, and the Builds list holds no Build with this name. Pressing Create again is safe: the core refuses a second Build with the same name.",
    );
    await pause();
    expect(core.buildPosts).toHaveLength(1);
    fireEvent.click(create);
    await heading("Second try");
    expect(core.buildPosts).toHaveLength(2);
    expect(core.builds.map((b) => b.name)).toEqual(["Second try"]);
  });

  it("a lost answer with an unreadable list tells the user to check Builds", async () => {
    const core = scriptedCore({
      answer: () => Promise.resolve(new Response("upstream timeout", { status: 504 })),
      list: (read) => (read > 1 ? problem(503, "List unreadable", "unavailable") : undefined),
    });
    renderAt("/builds", core);
    fireEvent.click(await openWithName("Unknown"));
    expect((await within(dialog()).findByRole("alert")).textContent).toBe(
      "Could not confirm the Build was created, and the Builds list did not load. Check Builds before you press Create again.",
    );
    await pause();
    expect(core.buildPosts).toHaveLength(1);
  });

  it("closing while the create is in flight keeps it: reopening shows it and cannot send again", async () => {
    let reply: (response: Response) => void = () => {};
    let write: () => ProfileSummary = () => storedBuild(0, "");
    const core = scriptedCore({
      answer: (store) => {
        write = store;
        return new Promise((resolve) => (reply = resolve));
      },
    });
    renderAt("/builds", core);
    fireEvent.click(await openWithName("Held"));
    await within(dialog()).findByText("Creating the Build.");
    fireEvent.click(within(dialog()).getByRole("button", { name: "Close" }));
    expect(screen.queryByRole("dialog")).toBeNull();

    // Reopened: the same create, its name, and no way to send a second.
    fireEvent.click(screen.getByRole("button", { name: "Create Build" }));
    expect(within(dialog()).getByText("Creating the Build.")).toBeTruthy();
    expect((within(dialog()).getByLabelText("Name") as HTMLInputElement).value).toBe("Held");
    const create = within(dialog()).getByRole("button", { name: "Create" });
    expect(create.getAttribute("aria-disabled")).toBe("true");
    fireEvent.click(create);
    await pause();
    expect(core.buildPosts).toHaveLength(1);

    // Closed again, the answer is lost after the write; the check still runs and is shown.
    fireEvent.click(within(dialog()).getByRole("button", { name: "Close" }));
    write();
    reply(problem(502, "Bad Gateway", "unavailable"));
    await pause();
    fireEvent.click(screen.getByRole("button", { name: "Create Build" }));
    expect((await within(dialog()).findByRole("alert")).textContent).toBe(
      "The answer to Create was lost, but the Builds list now holds this Build, so it was created. Open Held",
    );
    expect(core.buildPosts).toHaveLength(1);
  });

  it("a reopened dialog opens the Build when the held create answers 200", async () => {
    let reply: (response: Response) => void = () => {};
    const core = scriptedCore({
      answer: (write) => {
        const build = write();
        return new Promise((resolve) => (reply = () => resolve(json({ ...build, layers: [] }))));
      },
    });
    const router = renderAt("/builds", core);
    fireEvent.click(await openWithName("Later"));
    await within(dialog()).findByText("Creating the Build.");
    fireEvent.click(within(dialog()).getByRole("button", { name: "Close" }));
    fireEvent.click(screen.getByRole("button", { name: "Create Build" }));
    reply(json({}));
    await heading("Later");
    expect(router.state.location.pathname).toBe("/builds/1000");
  });

  it("checks with a list read started after the answer, not one started before the create", async () => {
    let releaseFirst: () => void = () => {};
    const core = scriptedCore({
      // The first list read is held with the empty list it saw, past the create's answer.
      list: (read, snapshot) =>
        read === 1 ? new Promise((resolve) => (releaseFirst = () => resolve(snapshot))) : undefined,
      answer: (write) => {
        write();
        return Promise.resolve(problem(502, "Bad Gateway", "unavailable"));
      },
    });
    renderAt("/builds", core);
    fireEvent.click(await openWithName("Fresh"));
    // No list had loaded before the press, so the dialog cannot say this press created it.
    expect((await within(dialog()).findByRole("alert")).textContent).toBe(
      "Could not confirm the Build was created, and the Builds list holds a Build with this name that may be an earlier one. Open it, or choose another name. Open Fresh",
    );
    expect(core.listReads()).toBe(2);
    releaseFirst();
    await pause();
    expect(within(dialog()).getByRole("alert").textContent).toContain("Open Fresh");
    fireEvent.click(within(dialog()).getByRole("button", { name: "Cancel" }));
    const table = await screen.findByRole("table", { name: "Builds" });
    expect(within(table).getByRole("link", { name: "Fresh" })).toBeTruthy();
    expect(core.buildPosts).toHaveLength(1);
  });

  it.each([
    [
      "an unloaded list before the press",
      (read: number) => (read === 1 ? problem(503, "No", "unavailable") : undefined),
    ],
    ["a loaded list that already held it", () => undefined],
  ])("a Build that may be older is not called created: %s", async (_case, list) => {
    const core = scriptedCore({
      builds: ["Old"],
      list,
      answer: () => Promise.resolve(new Response("upstream timeout", { status: 504 })),
    });
    renderAt("/builds", core);
    fireEvent.click(await openWithName("Old"));
    const alert = await within(dialog()).findByRole("alert");
    expect(alert.textContent).not.toContain("so it was created");
    expect(alert.textContent).toContain("may be an earlier one");
    expect(within(alert).getByRole("link", { name: "Open Old" })).toBeTruthy();
    expect(core.builds).toHaveLength(1);
  });

  it.each([
    ["202 with a valid body", () => json(storedBuild(4242, "Ghost"), 202)],
    ["200 that fails the contract", () => json({ id: "not a Build" })],
    ["a proxy's 400 page", () => new Response("<h1>Bad Request</h1>", { status: 400 })],
    ["a proxy's 403 page", () => new Response("<h1>Forbidden</h1>", { status: 403 })],
    ["a redirected 200", () => redirected(json(storedBuild(4242, "Ghost")))],
    ["a redirected 403 problem", () => redirected(problem(403, "Forbidden", "forbidden"))],
    [
      "a redirected 401, which opens no sign-in,",
      () => redirected(problem(401, "Authentication required", "unauthenticated")),
    ],
  ])("%s is checked against the list, never trusted or resent", async (_case, reply) => {
    const core = scriptedCore({ answer: () => Promise.resolve(reply()) });
    const router = renderAt("/builds", core);
    fireEvent.click(await openWithName("Ghost"));
    expect((await within(dialog()).findByRole("alert")).textContent).toMatch(
      /^Could not confirm the Build was created, and the Builds list holds no Build with this name\./,
    );
    await pause();
    expect(router.state.location.pathname).toBe("/builds");
    expect(core.buildPosts).toHaveLength(1);
    expect(core.listReads()).toBe(2);
  });

  it("a name the core refused as in use stays refused when the list cannot show it", async () => {
    const core = scriptedCore({
      builds: ["Taken"],
      list: (read) => (read > 1 ? problem(503, "List unreadable", "unavailable") : undefined),
    });
    renderAt("/builds", core);
    fireEvent.click(await openWithName("Taken"));
    expect((await within(dialog()).findByRole("alert")).textContent).toBe(
      "A Build with this name already exists. Choose another name.",
    );
  });

  it("never follows a redirect: the POST is sent once with redirect manual, then checked", async () => {
    const core = scriptedCore({
      answer: (write) => {
        write();
        return Promise.resolve(new Response(null, { status: 307, headers: { location: "/x" } }));
      },
    });
    renderAt("/builds", core);
    fireEvent.click(await openWithName("Moved"));
    expect((await within(dialog()).findByRole("alert")).textContent).toBe(
      "The answer to Create was lost, but the Builds list now holds this Build, so it was created. Open Moved",
    );
    const posts = core.fetchMock.mock.calls.filter(([, init]) => init?.method === "POST");
    expect(posts).toHaveLength(1);
    expect(posts[0]![1]?.redirect).toBe("manual");
  });

  it("a 200 that lands while the dialog is closed shows the Build in a list read after it", async () => {
    let releaseFirst: () => void = () => {};
    let reply: () => void = () => {};
    const core = scriptedCore({
      list: (read, snapshot) =>
        read === 1 ? new Promise((resolve) => (releaseFirst = () => resolve(snapshot))) : undefined,
      answer: (write) =>
        new Promise((resolve) => {
          reply = () => {
            const build = write();
            resolve(json({ ...build, layers: [] }));
          };
        }),
    });
    const router = renderAt("/builds", core);
    fireEvent.click(await openWithName("Quiet"));
    await within(dialog()).findByText("Creating the Build.");
    fireEvent.click(within(dialog()).getByRole("button", { name: "Close" }));
    reply();
    await pause();
    releaseFirst();
    const table = await screen.findByRole("table", { name: "Builds" });
    expect(within(table).getByRole("link", { name: "Quiet" })).toBeTruthy();
    expect(core.listReads()).toBe(2);
    expect(router.state.location.pathname).toBe("/builds");
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("a 401 from Create opens sign-in, as every other write's does", async () => {
    const core = scriptedCore({
      answer: () => Promise.resolve(problem(401, "Authentication required", "unauthenticated")),
    });
    const router = renderAt("/builds", core);
    fireEvent.click(await openWithName("Signed out"));
    await waitFor(() => expect(router.state.location.pathname).toBe("/login"));
    expect(router.state.location.search).toBe("?next=%2Fbuilds");
    expect(core.buildPosts).toHaveLength(1);
    // The settled Create holds no observer, so its mutation leaves the cache.
    await waitFor(() => expect(lastClient.getMutationCache().getAll()).toHaveLength(0));
  });

  it("an older Create's list read, answered late, never hides a newer Build", async () => {
    let releaseSecond: () => void = () => {};
    const core = scriptedCore({
      // Read 2 is the first Create's read after its 200; it is held with the list it saw.
      list: (read, snapshot) =>
        read === 2
          ? new Promise((resolve) => (releaseSecond = () => resolve(snapshot)))
          : undefined,
    });
    renderAt("/builds", core);
    fireEvent.click(await openWithName("Build 1"));
    await heading("Build 1");
    await waitFor(() => expect(core.listReads()).toBe(2));

    fireEvent.click(screen.getByRole("link", { name: "Builds" }));
    fireEvent.click(await openWithName("Build 2"));
    await heading("Build 2");
    fireEvent.click(screen.getByRole("link", { name: "Builds" }));
    const table = await screen.findByRole("table", { name: "Builds" });
    await within(table).findByRole("link", { name: "Build 2" });

    releaseSecond();
    await pause();
    expect(within(table).getByRole("link", { name: "Build 1" })).toBeTruthy();
    expect(within(table).getByRole("link", { name: "Build 2" })).toBeTruthy();
  });

  it.each([
    ["a check after a lost answer", 502],
    ["the read after a 200", 200],
  ])("leaving Builds does not cancel %s or put the old list back", async (_case, status) => {
    let release: () => void = () => {};
    const core = scriptedCore({
      list: (read, snapshot) =>
        read === 2 ? new Promise((resolve) => (release = () => resolve(snapshot))) : undefined,
      answer: (write) => {
        const build = write();
        return Promise.resolve(
          status === 200
            ? json({ ...build, layers: [] })
            : problem(502, "Bad Gateway", "unavailable"),
        );
      },
    });
    renderAt("/builds", core);
    fireEvent.click(await openWithName("Kept"));
    if (status === 200) {
      await heading("Kept");
      await waitFor(() => expect(lastClient.getMutationCache().getAll()).toHaveLength(0));
    } else {
      await within(dialog()).findByText("Checking the Builds list.");
      fireEvent.click(within(dialog()).getByRole("button", { name: "Close" }));
      fireEvent.click(screen.getByRole("link", { name: "Library" }));
      await heading("Library");
    }
    release();
    await waitFor(() =>
      expect(
        lastClient.getQueryData<{ profiles: ProfileSummary[] }>(["core", "builds"])?.profiles,
      ).toHaveLength(1),
    );
    if (status === 502) {
      fireEvent.click(screen.getByRole("link", { name: "Builds" }));
      fireEvent.click(await screen.findByRole("button", { name: "Create Build" }));
      expect((await within(dialog()).findByRole("alert")).textContent).toContain(
        "so it was created",
      );
    }
  });

  type Client = ReturnType<typeof createQueryClient>;
  const KEY = ["core", "builds"];
  const failNewer = (client: Client) => {
    const query = client.getQueryCache().find({ queryKey: KEY })!;
    query.setState({ status: "error", dataUpdateCount: query.state.dataUpdateCount + 1 });
  };
  const signIn = (c: Client) => c.clear();
  const signOut = (c: Client) => c.removeQueries({ queryKey: KEY });
  const noList = (c: Client) => c.setQueryData(KEY, { no: 1 });
  it.each([
    ["Builds read it again", () => {}, true, "listed", 200],
    ["sign-in cleared the cache", signIn, true, "listed", 200],
    ["sign-in, then the old read 401s", signIn, true, "listed", 401],
    ["sign-in, then the old read 503s", signIn, true, "listed", 503],
    ["sign-out removed it", signOut, false, "unread", 200],
    ["the cache holds no list", noList, false, "unread", 200],
    ["a newer read failed", failNewer, false, "unread", 200],
    ["this session's read 401s", () => {}, false, "unread", 401],
  ] as const)("a check never answers with a list older than the cache's: %s", async (...row) => {
    const [, change, back, kind, status] = row;
    let release: () => void = () => {};
    let commit: () => ProfileSummary = () => storedBuild(0, "");
    const core = scriptedCore({
      list: (read, snapshot) =>
        read !== 2
          ? undefined
          : new Promise((resolve) => {
              release = () => resolve(status === 200 ? snapshot : problem(status, "Read failed"));
            }),
      answer: (write) => {
        commit = write;
        return Promise.resolve(problem(502, "Bad Gateway", "unavailable"));
      },
    });
    const router = renderAt("/builds", core);
    fireEvent.click(await openWithName("Late"));
    await within(dialog()).findByText("Checking the Builds list.");
    fireEvent.click(within(dialog()).getByRole("button", { name: "Close" }));
    fireEvent.click(screen.getByRole("link", { name: "Library" }));
    await heading("Library");
    commit(); // The create lands after the check's read saw an empty list.
    change(lastClient);
    if (back) {
      fireEvent.click(screen.getByRole("link", { name: "Builds" }));
      const table = await screen.findByRole("table", { name: "Builds" });
      expect(within(table).getByRole("link", { name: "Late" })).toBeTruthy();
    }
    const held = lastClient.getQueryData(KEY);
    release();
    await waitFor(() => expect(createStore.get().kind).toBe(kind));
    // The read's older list is never written over what the cache holds.
    expect(lastClient.getQueryData(KEY)).toBe(held);
    // Only this session's 401 opens sign-in.
    const path = status === 401 && !back ? "/login" : back ? "/builds" : "/library";
    expect(router.state.location.pathname).toBe(path);
  });
});
