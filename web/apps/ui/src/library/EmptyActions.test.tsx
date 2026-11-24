import type { Health, ProfileSummary, SourceList } from "@print-partner/rust-client";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { createMemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App, createQueryClient } from "../App";
import { routes } from "../routes";
import { LibraryFilesNext } from "./FilesNext";
import { JOBS_KEY } from "./libraryJobs";
import { rowControlId } from "./ZipUpload";

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });

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

/** A Source just created: no revision and no files yet. */
const source = (id: number, name: string, extra: Partial<Source> = {}): Source => ({
  id,
  name,
  url: "",
  source_kind: "archive",
  source_type: "local",
  role: "primary",
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
  ...extra,
});

const ZIP = source(1, "Rack");
const GITHUB = source(2, "Kit", {
  source_kind: "github",
  source_type: "git",
  url: "https://github.com/example/kit",
});

const summary: ProfileSummary = {
  id: 5,
  name: "Desk organiser",
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
};

interface Sent {
  path: string;
  body: unknown;
  redirect: RequestRedirect | undefined;
}

/**
 * A disposable core. Every Source has no revision unless `files` names one. Uploads and
 * syncs are accepted with a job that stays running, so the row keeps following it.
 */
function scriptedCore({
  sources,
  base,
  files = {},
  sourcesFail = false,
}: {
  sources: Source[];
  base?: number;
  files?: Record<
    number,
    { revision_id: number; files: { path: string; kind: string; size_bytes: number }[] }
  >;
  sourcesFail?: boolean;
}) {
  const sent: Sent[] = [];
  const held = new Map<string, Promise<Response>>();
  const job = (id: string, status: "running" | "error") =>
    json({
      job_id: id,
      kind: "supplied-source-import",
      status,
      message: status === "error" ? "Job failed" : "Running",
      progress: null,
      result: null,
      error: status === "error" ? "The archive is damaged" : null,
      created_at: "2026-11-20T10:00:00.000Z",
      finished_at: status === "error" ? "2026-11-20T10:01:00.000Z" : null,
    });
  /** Holds every read of job `id` until the returned function answers it with `status`. */
  const hold = (id: string) => {
    let answer: (status: "running" | "error") => void = () => {};
    held.set(id, new Promise((resolve) => (answer = (status) => resolve(job(id, status)))));
    return answer;
  };
  const jobRead = (path: string) => {
    const id = decodeURIComponent(path.slice("/api/v1/jobs/".length));
    return held.get(id) ?? job(id, "running");
  };
  const layers = () => {
    const named = sources.find((item) => item.id === base);
    const layer = { id: 10, layer_order: 0, layer_type: "base", project_id: base ?? null };
    return json({ profile_id: 5, layers: [{ ...layer, project_name: named?.name ?? "Rack" }] });
  };
  const fileList = (id: number) =>
    json({
      source_id: id,
      revision_id: files[id]?.revision_id ?? null,
      files: files[id]?.files ?? [],
    });
  const read = (path: string): Response | Promise<Response> => {
    if (path === "/health") return json(health);
    if (path === "/api/v1/sources/activity?limit=100") return json({ events: [] });
    if (path === "/api/v1/sources") {
      return sourcesFail
        ? json({ type: "about:blank", title: "Unavailable", status: 503, detail: "down" }, 503)
        : json({ sources });
    }
    const list = /^\/api\/v1\/sources\/(\d+)\/files$/.exec(path);
    if (list) return fileList(Number(list[1]));
    if (path.startsWith("/api/v1/jobs/")) return jobRead(path);
    if (path === "/api/v2/plans/5") return json(summary);
    if (path === "/api/v2/plans/5/layers") return layers();
    return json({ detail: "not found" }, 404);
  };
  const write = (path: string, init: RequestInit): Response => {
    sent.push({
      path,
      body: typeof init.body === "string" ? JSON.parse(init.body) : init.body,
      redirect: init.redirect,
    });
    if (/^\/api\/v1\/sources\/\d+\/upload-zip$/.test(path)) {
      return json({ job_id: `upload-${sent.length}` }, 202);
    }
    if (path === "/api/v1/jobs/sync") return json({ job_id: `sync-${sent.length}` }, 202);
    return json({ detail: "not found" }, 404);
  };
  // While held, every write is recorded at once and answered when released.
  let gate: Promise<void> | null = null;
  const holdWrites = () => {
    let open = () => {};
    gate = new Promise((resolve) => (open = resolve));
    return () => open();
  };
  const fetchMock = vi.fn((path: string, init?: RequestInit) => {
    if (!init?.method || init.method === "GET") return Promise.resolve(read(path));
    const answer = write(path, init);
    return gate ? gate.then(() => answer) : Promise.resolve(answer);
  });
  return { fetchMock, sent, hold, holdWrites };
}

function renderAt(path: string, core: ReturnType<typeof scriptedCore>) {
  vi.stubGlobal("fetch", core.fetchMock);
  const router = createMemoryRouter(routes, { initialEntries: [path] });
  const queryClient = createQueryClient();
  queryClient.setDefaultOptions({
    queries: { ...queryClient.getDefaultOptions().queries, retry: false },
  });
  render(<App router={router} queryClient={queryClient} />);
  return router;
}

const filesFor = (name: string) => screen.findByRole("region", { name: `Files for ${name}` });

/** Another tab writing the job map, as the browser reports it to this one. */
function otherTabWrites(jobs: Record<string, string>) {
  const oldValue = localStorage.getItem(JOBS_KEY);
  const newValue = JSON.stringify(jobs);
  localStorage.setItem(JOBS_KEY, newValue);
  act(() => {
    window.dispatchEvent(new StorageEvent("storage", { key: JOBS_KEY, oldValue, newValue }));
  });
}

const zipFile = () => new File(["PK"], "rack.zip", { type: "application/zip" });

beforeEach(() => localStorage.clear());

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  localStorage.clear();
});

describe("GRE-1867: Library and Source-files empty states name the next action", () => {
  it("a Library with no Sources names Add Source, and Add Source opens its dialog", async () => {
    const core = scriptedCore({ sources: [] });
    renderAt("/library", core);
    expect(
      await screen.findByText(
        "No Sources yet. Press Add Source to add a kit from a ZIP archive or a GitHub repository.",
      ),
    ).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Add Source" }));
    expect(await screen.findByRole("dialog", { name: "Add Source" })).toBeTruthy();
    expect(core.sent).toEqual([]);
  });

  it("empty activity in a ZIP-only Library names Upload ZIP, and the row's Upload ZIP posts once", async () => {
    const core = scriptedCore({ sources: [ZIP] });
    renderAt("/library", core);
    const recent = await screen.findByRole("region", { name: "Recent activity" });
    expect(recent.textContent).toContain(
      "No Source activity yet. To add or replace a Source's files, press Upload ZIP on its row.",
    );
    expect(recent.textContent).not.toContain("Sync");
    fireEvent.change(screen.getByLabelText("Upload ZIP for Rack"), {
      target: { files: [zipFile()] },
    });
    await waitFor(() =>
      expect(core.sent.map((item) => item.path)).toEqual(["/api/v1/sources/1/upload-zip"]),
    );
  });

  it.each([
    ["only a GitHub Source", [GITHUB]],
    ["a ZIP Source and a GitHub Source", [ZIP, GITHUB]],
  ])(
    "empty activity in a Library with %s names Sync, and the row's Sync posts once",
    async (_, sources) => {
      const core = scriptedCore({ sources });
      renderAt("/library", core);
      const recent = await screen.findByRole("region", { name: "Recent activity" });
      expect(recent.textContent).toContain(
        "No Source activity yet. To bring a GitHub Source up to date, press Sync on its row.",
      );
      expect(recent.textContent).not.toContain("Upload ZIP");
      fireEvent.click(screen.getByRole("button", { name: "Sync Kit" }));
      await waitFor(() =>
        expect(core.sent).toEqual([
          { path: "/api/v1/jobs/sync", body: { project_ids: [2] }, redirect: undefined },
        ]),
      );
    },
  );

  it("an empty ZIP Source's files press its row's Upload ZIP, and a running import keeps the button away", async () => {
    const core = scriptedCore({ sources: [ZIP, GITHUB] });
    const router = renderAt("/library/1", core);
    const region = await filesFor("Rack");
    expect(await within(region).findByText("No files yet.")).toBeTruthy();
    expect(within(region).queryByRole("button", { name: "Sync" })).toBeNull();
    const input = screen.getByLabelText("Upload ZIP for Rack");
    const opened = vi.fn();
    input.addEventListener("click", opened);
    fireEvent.click(within(region).getByRole("button", { name: "Upload ZIP" }));
    // The row's own file input opened, so the row's upload flow runs, and nothing was sent.
    expect(opened).toHaveBeenCalledTimes(1);
    expect(document.activeElement).toBe(input);
    expect(core.sent).toEqual([]);
    fireEvent.change(input, { target: { files: [zipFile()] } });
    expect(
      await within(region).findByText(
        "Its import is running. If it succeeds, its files appear here.",
      ),
    ).toBeTruthy();
    expect(within(region).queryByRole("button", { name: "Upload ZIP" })).toBeNull();
    expect(core.sent.map((item) => item.path)).toEqual(["/api/v1/sources/1/upload-zip"]);

    // Leaving the Source and coming back, and Back then Forward, still offer no second upload.
    fireEvent.click(screen.getByRole("link", { name: "Rack" }));
    await waitFor(() => expect(router.state.location.pathname).toBe("/library"));
    await act(() => router.navigate("/builds"));
    await act(() => router.navigate(-1));
    await act(() => router.navigate(-1));
    expect(router.state.location.pathname).toBe("/library/1");
    const back = await filesFor("Rack");
    expect(await within(back).findByText(/^Its import is running/)).toBeTruthy();
    expect(within(back).queryByRole("button", { name: "Upload ZIP" })).toBeNull();
    await act(() => router.navigate(1));
    await act(() => router.navigate(-1));
    const again = await filesFor("Rack");
    expect(await within(again).findByText(/^Its import is running/)).toBeTruthy();
    expect(within(again).queryByRole("button", { name: "Upload ZIP" })).toBeNull();
    expect(core.sent).toHaveLength(1);
  });

  it("an empty GitHub Source's files press its row's Sync once, then say the sync is running", async () => {
    const core = scriptedCore({ sources: [ZIP, GITHUB] });
    renderAt("/library/2", core);
    const region = await filesFor("Kit");
    expect(await within(region).findByText("No files yet.")).toBeTruthy();
    expect(
      within(region).getByText(/^Sync this Source to fetch its files from GitHub\./),
    ).toBeTruthy();
    expect(within(region).queryByRole("button", { name: "Upload ZIP" })).toBeNull();
    fireEvent.click(within(region).getByRole("button", { name: "Sync" }));
    expect(
      await within(region).findByText(
        "Its sync is running. If it succeeds, its files appear here.",
      ),
    ).toBeTruthy();
    expect(within(region).queryByRole("button", { name: "Sync" })).toBeNull();
    expect(core.sent).toEqual([
      { path: "/api/v1/jobs/sync", body: { project_ids: [2] }, redirect: undefined },
    ]);
  });

  it("a remembered import from an earlier visit offers no Upload ZIP in the files view", async () => {
    localStorage.setItem(JOBS_KEY, JSON.stringify({ "1": "upload-0" }));
    const core = scriptedCore({ sources: [ZIP] });
    renderAt("/library/1", core);
    const region = await filesFor("Rack");
    expect(await within(region).findByText(/^Its import is running/)).toBeTruthy();
    expect(within(region).queryByRole("button", { name: "Upload ZIP" })).toBeNull();
    expect(core.sent).toEqual([]);
  });

  it("a revision with no files also names the action; a revision with files names none", async () => {
    const core = scriptedCore({
      sources: [ZIP, source(3, "Shelf", { current_source_revision_id: 8 })],
      files: {
        1: { revision_id: 7, files: [] },
        3: { revision_id: 8, files: [{ path: "a.txt", kind: "document", size_bytes: 1 }] },
      },
    });
    const router = renderAt("/library/1", core);
    const region = await filesFor("Rack");
    expect(await within(region).findByText("This revision has no files.")).toBeTruthy();
    expect(within(region).getByRole("button", { name: "Upload ZIP" })).toBeTruthy();
    await act(() => router.navigate("/library/3"));
    const shelf = await filesFor("Shelf");
    expect(await within(shelf).findByText("a.txt")).toBeTruthy();
    expect(within(shelf).queryByRole("button", { name: "Upload ZIP" })).toBeNull();
    expect(within(shelf).queryByRole("link")).toBeNull();
  });

  it.each([
    ["a ZIP Source", ZIP, "Upload ZIP in Library", "Upload ZIP"],
    ["a GitHub Source", GITHUB, "Sync in Library", "Sync"],
  ])(
    "a Build's empty base files from %s link to its Library row, and Back then Forward keep it",
    async (_, base, link, action) => {
      const core = scriptedCore({ sources: [ZIP, GITHUB], base: base.id });
      const router = renderAt("/builds/5/sources", core);
      const region = await filesFor(base.name);
      expect(await within(region).findByText("No files yet.")).toBeTruthy();
      fireEvent.click(await within(region).findByRole("link", { name: link }));
      await waitFor(() => expect(router.state.location.pathname).toBe(`/library/${base.id}`));
      const library = await filesFor(base.name);
      expect(await within(library).findByRole("button", { name: action })).toBeTruthy();
      await act(() => router.navigate(-1));
      expect(router.state.location.pathname).toBe("/builds/5/sources");
      expect(
        await within(await filesFor(base.name)).findByRole("link", { name: link }),
      ).toBeTruthy();
      await act(() => router.navigate(1));
      expect(router.state.location.pathname).toBe(`/library/${base.id}`);
      expect(
        await within(await filesFor(base.name)).findByRole("button", { name: action }),
      ).toBeTruthy();
      expect(core.sent).toEqual([]);
    },
  );

  it("a Build's empty base files link to Library when the Library's Sources fail", async () => {
    const core = scriptedCore({ sources: [ZIP], base: 1, sourcesFail: true });
    const router = renderAt("/builds/5/sources", core);
    const region = await filesFor("Rack");
    expect(
      await within(region).findByText(
        /^Open this Source in Library to upload a ZIP archive or sync it\./,
      ),
    ).toBeTruthy();
    fireEvent.click(within(region).getByRole("link", { name: "Open in Library" }));
    await waitFor(() => expect(router.state.location.pathname).toBe("/library/1"));
  });
});

const CASES = [
  ["import", { source: ZIP, other: GITHUB, job: "upload-7", action: "Upload ZIP" }],
  ["sync", { source: GITHUB, other: ZIP, job: "sync-7", action: "Sync" }],
] as const;

/** Opens a Source whose remembered job is `job`, with that job's reads held. */
async function openRunning(source: Source, job: string, jobs: Record<string, string> = {}) {
  localStorage.setItem(JOBS_KEY, JSON.stringify({ ...jobs, [source.id]: job }));
  const core = scriptedCore({ sources: [ZIP, GITHUB] });
  const answer = core.hold(job);
  renderAt(`/library/${source.id}`, core);
  const region = await filesFor(source.name);
  expect(await within(region).findByText(/^Its (import|sync) is running/)).toBeTruthy();
  return { core, region, answer };
}

describe("GRE-1867 fix round 1: the files panel follows a remembered job that ends", () => {
  it.each(CASES)(
    "after another tab ends the %s first, the panel offers its action again and the late answer changes nothing",
    async (_, { source, job, action }) => {
      const { core, region, answer } = await openRunning(source, job);
      otherTabWrites({});
      expect(await within(region).findByRole("button", { name: action })).toBeTruthy();
      expect(within(region).queryByText(/is running/)).toBeNull();
      await act(async () => answer("error"));
      expect(within(region).getByRole("button", { name: action })).toBeTruthy();
      expect(core.sent).toEqual([]);
    },
  );

  it.each(CASES)(
    "when the %s entry is gone without a storage event, its own failed answer brings the action back",
    async (_, { source, job, action }) => {
      const { core, region, answer } = await openRunning(source, job);
      localStorage.setItem(JOBS_KEY, "{}");
      await act(async () => answer("error"));
      expect(await within(region).findByRole("button", { name: action })).toBeTruthy();
      expect(within(region).queryByText(/is running/)).toBeNull();
      expect(core.sent).toEqual([]);
    },
  );

  it.each(CASES)(
    "a newer %s from another tab keeps the panel running when the older one fails late",
    async (_, { source, job, action }) => {
      const { core, region, answer } = await openRunning(source, job);
      otherTabWrites({ [source.id]: `${job}-newer` });
      await act(async () => answer("error"));
      expect(within(region).getByText(/is running/)).toBeTruthy();
      expect(within(region).queryByRole("button", { name: action })).toBeNull();
      expect(JSON.parse(localStorage.getItem(JOBS_KEY) ?? "")).toEqual({
        [source.id]: `${job}-newer`,
      });
      await waitFor(() =>
        expect(core.fetchMock.mock.calls.map(([path]) => path)).toContain(
          `/api/v1/jobs/${job}-newer`,
        ),
      );
    },
  );

  it.each(CASES)(
    "a newer %s already stored, with no storage event yet, keeps the panel running when the older one fails late",
    async (_, { source, job, action }) => {
      const { core, region, answer } = await openRunning(source, job);
      const answerNewer = core.hold(`${job}-newer`);
      localStorage.setItem(JOBS_KEY, JSON.stringify({ [source.id]: `${job}-newer` }));
      await act(async () => answer("error"));
      expect(within(region).getByText(/is running/)).toBeTruthy();
      expect(within(region).queryByRole("button", { name: action })).toBeNull();
      await waitFor(() =>
        expect(core.fetchMock.mock.calls.map(([path]) => path)).toContain(
          `/api/v1/jobs/${job}-newer`,
        ),
      );
      await act(async () => answerNewer("error"));
      expect(await within(region).findByRole("button", { name: action })).toBeTruthy();
      expect(core.sent).toEqual([]);
    },
  );

  it.each(CASES)(
    "a storage event for another Source leaves the panel for the %s as it was",
    async (_, { source, other, job, action }) => {
      const { region } = await openRunning(source, job, { [other.id]: "other-1" });
      otherTabWrites({ [source.id]: job });
      expect(within(region).getByText(/is running/)).toBeTruthy();
      expect(within(region).queryByRole("button", { name: action })).toBeNull();
    },
  );
});

/** Starts a job from the empty files panel of the Source on screen, through its row. */
async function startFromPanel(source: Source) {
  const region = await filesFor(source.name);
  const action = source.source_kind === "github" ? "Sync" : "Upload ZIP";
  fireEvent.click(await within(region).findByRole("button", { name: action }));
  if (source.source_kind !== "github") {
    fireEvent.change(screen.getByLabelText(`Upload ZIP for ${source.name}`), {
      target: { files: [zipFile()] },
    });
  }
  expect(await within(region).findByText(/is running/)).toBeTruthy();
}

const throwing = (method: "getItem" | "setItem" | "removeItem") =>
  vi.spyOn(Storage.prototype, method).mockImplementation(() => {
    throw new DOMException("The store refused", "QuotaExceededError");
  });

const FAILURES = CASES.flatMap(([what, kind]) => [
  [what, "a refused read", kind, ["getItem"]] as const,
  [what, "a refused write", kind, ["setItem", "removeItem"]] as const,
]);

describe("GRE-1867 fix round 3: storage that throws never loses a job this tab follows", () => {
  it.each(CASES)(
    "with writes refused, starting a job for another Source keeps the %s running",
    async (_, { source, other }) => {
      throwing("setItem");
      const core = scriptedCore({ sources: [ZIP, GITHUB] });
      const router = renderAt(`/library/${source.id}`, core);
      await startFromPanel(source);
      await act(() => router.navigate(`/library/${other.id}`));
      await startFromPanel(other);
      await act(() => router.navigate(`/library/${source.id}`));
      const region = await filesFor(source.name);
      expect(await within(region).findByText(/is running/)).toBeTruthy();
      expect(within(region).queryByRole("button")).toBeNull();
      expect(core.sent).toHaveLength(2);
    },
  );

  it.each(FAILURES)(
    "when the %s ends with %s, the other Source's job keeps running",
    async (_, _failure, { source, other, job, action }, methods) => {
      const { region, answer } = await openRunning(source, job, { [other.id]: "other-1" });
      for (const method of methods) throwing(method);
      await act(async () => answer("error"));
      expect(await within(region).findByRole("button", { name: action })).toBeTruthy();
      fireEvent.click(screen.getByRole("link", { name: other.name }));
      const elsewhere = await filesFor(other.name);
      expect(await within(elsewhere).findByText(/is running/)).toBeTruthy();
      expect(within(elsewhere).queryByRole("button")).toBeNull();
    },
  );
});

/** The row's own Upload ZIP input or Sync button for `source`. */
const rowControl = (source: Source) =>
  (source.source_kind === "github"
    ? screen.getByRole("button", { name: `Sync ${source.name}` })
    : screen.getByLabelText(`Upload ZIP for ${source.name}`)) as HTMLInputElement;

/** A press on the row control that ignores `disabled`, as a script or assistive tool might. */
function forcePress(source: Source) {
  const control = rowControl(source);
  if (source.source_kind === "github")
    control.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  else fireEvent.change(control, { target: { files: [zipFile()] } });
}

const ENDS = CASES.flatMap(([what, kind]) => [
  [what, "its failed answer", kind] as const,
  [what, "another tab ending it", kind] as const,
]);

describe("GRE-1875: Library job recovery", () => {
  const startedJob = (source: Source, count: number) =>
    `${source.source_kind === "github" ? "sync" : "upload"}-${count}`;
  const jobReads = (core: ReturnType<typeof scriptedCore>, job: string) =>
    core.fetchMock.mock.calls.filter(([url]) => url === `/api/v1/jobs/${job}`).length;

  it.each(CASES)("retries an unrecorded %s when another Source starts", async (_, { source, other }) => {
    const outage = throwing("setItem");
    const core = scriptedCore({ sources: [ZIP, GITHUB] });
    const router = renderAt(`/library/${source.id}`, core);
    await startFromPanel(source);
    expect(localStorage.getItem(JOBS_KEY)).toBeNull();

    outage.mockRestore();
    await act(() => router.navigate(`/library/${other.id}`));
    await startFromPanel(other);
    expect(JSON.parse(localStorage.getItem(JOBS_KEY) ?? "")).toEqual({
      [source.id]: startedJob(source, 1),
      [other.id]: startedJob(other, 2),
    });
    expect(rowControl(source).disabled).toBe(true);
    expect(core.sent).toHaveLength(2);
  });

  it.each(CASES)("learns another tab's %s after a refused write", async (_, { source, other, job }) => {
    const outage = throwing("setItem");
    const core = scriptedCore({ sources: [ZIP, GITHUB] });
    const router = renderAt(`/library/${other.id}`, core);
    await startFromPanel(other);
    expect(localStorage.getItem(JOBS_KEY)).toBeNull();

    outage.mockRestore();
    await act(() => router.navigate(`/library/${source.id}`));
    const region = await filesFor(source.name);
    otherTabWrites({ [source.id]: job });
    expect(await within(region).findByText(/is running/)).toBeTruthy();
    expect(rowControl(source).disabled).toBe(true);
    expect(rowControl(other).disabled).toBe(true);
    expect(jobReads(core, job)).toBe(1);
    expect(JSON.parse(localStorage.getItem(JOBS_KEY) ?? "")).toEqual({
      [source.id]: job,
      [other.id]: startedJob(other, 1),
    });
    expect(core.sent).toHaveLength(1);
  });

  it.each(CASES)("never follows an ended %s again after a refused read", async (_, { source, other, job, action }) => {
    const stale = { [source.id]: job, [other.id]: "other-job" };
    localStorage.setItem(JOBS_KEY, JSON.stringify(stale));
    const core = scriptedCore({ sources: [ZIP, GITHUB] });
    const answer = core.hold(job);
    const otherAnswer = core.hold("other-job");
    const router = renderAt(`/library/${source.id}`, core);
    const region = await filesFor(source.name);
    await waitFor(() => expect(jobReads(core, job)).toBe(1));
    await waitFor(() => expect(jobReads(core, "other-job")).toBe(1));

    const outage = throwing("getItem");
    await act(async () => answer("error"));
    expect(await within(region).findByRole("button", { name: action })).toBeTruthy();
    expect(rowControl(other).disabled).toBe(true);
    outage.mockRestore();
    expect(JSON.parse(localStorage.getItem(JOBS_KEY) ?? "")).toEqual(stale);

    await act(async () => otherAnswer("error"));
    expect(localStorage.getItem(JOBS_KEY)).toBe("{}");
    expect(within(region).queryByText(/is running/)).toBeNull();
    expect(jobReads(core, job)).toBe(1);
    otherTabWrites({ [source.id]: job });
    await act(() => router.navigate("/type"));
    await act(() => router.navigate(`/library/${source.id}`));
    const reopened = await filesFor(source.name);
    expect(within(reopened).getByRole("button", { name: action })).toBeTruthy();
    expect(within(reopened).queryByText(/is running/)).toBeNull();
    expect(jobReads(core, job)).toBe(1);
    expect(core.sent).toEqual([]);
  });

  it.each(CASES)("guards an unanswered %s across Library re-entry", async (_, { source, action }) => {
    const core = scriptedCore({ sources: [ZIP, GITHUB] });
    const release = core.holdWrites();
    const path = `/library/${source.id}`;
    const router = renderAt(path, core);
    await within(await filesFor(source.name)).findByRole("button", { name: action });
    forcePress(source);
    await waitFor(() => expect(core.sent).toHaveLength(1));

    await act(() => router.navigate("/type"));
    expect(screen.queryByRole("heading", { name: "Library" })).toBeNull();
    await act(() => router.navigate(path));
    const region = await filesFor(source.name);
    expect(rowControl(source).disabled).toBe(true);
    fireEvent.click(within(region).getByRole("button", { name: action }));
    forcePress(source);
    expect(core.sent).toHaveLength(1);
    expect(jobReads(core, startedJob(source, 1))).toBe(0);

    await act(async () => release());
    expect(await within(region).findByText(/is running/)).toBeTruthy();
    expect(jobReads(core, startedJob(source, 1))).toBe(1);
    expect(core.sent).toHaveLength(1);
  });

  it.each(CASES)("ignores a late %s answer after leaving the signed-in frame", async (_, { source, action }) => {
    const core = scriptedCore({ sources: [ZIP, GITHUB] });
    const release = core.holdWrites();
    const path = `/library/${source.id}`;
    const router = renderAt(path, core);
    await within(await filesFor(source.name)).findByRole("button", { name: action });
    forcePress(source);
    await waitFor(() => expect(core.sent).toHaveLength(1));

    await act(() => router.navigate("/login"));
    await act(() => router.navigate(path));
    await filesFor(source.name);
    expect(rowControl(source).disabled).toBe(false);
    await act(async () => release());
    expect(localStorage.getItem(JOBS_KEY)).toBeNull();
    expect(jobReads(core, startedJob(source, 1))).toBe(0);
    expect(core.sent).toHaveLength(1);
  });
});

describe("GRE-1867 fix round 4: a Source with a job in this tab's map is busy on its row too", () => {
  it.each(ENDS)(
    "%s: a job learned from another tab disables the row at once, sends nothing on a forced press, and ends with %s",
    async (_, end, { source, job }) => {
      const core = scriptedCore({ sources: [ZIP, GITHUB] });
      renderAt(`/library/${source.id}`, core);
      const region = await filesFor(source.name);
      await within(region).findByRole("button", { name: /^(Upload ZIP|Sync)$/ });
      const answer = core.hold(job);
      otherTabWrites({ [source.id]: job });
      expect(await within(region).findByText(/is running/)).toBeTruthy();
      expect(rowControl(source).disabled).toBe(true);
      forcePress(source);
      await act(async () => {});
      expect(core.sent).toEqual([]);
      if (end === "its failed answer") await act(async () => answer("error"));
      else otherTabWrites({});
      await waitFor(() => expect(rowControl(source).disabled).toBe(false));
      expect(core.sent).toEqual([]);
    },
  );

  it.each(CASES)(
    "%s: a job found in storage at first render keeps the row disabled until it ends",
    async (_, { source, job }) => {
      const { core, answer } = await openRunning(source, job);
      expect(rowControl(source).disabled).toBe(true);
      forcePress(source);
      await act(async () => {});
      expect(core.sent).toEqual([]);
      await act(async () => answer("error"));
      await waitFor(() => expect(rowControl(source).disabled).toBe(false));
    },
  );

  it.each(CASES)(
    "%s: while this tab's own POST is in flight, the row and the panel send no second POST",
    async (_, { source }) => {
      const core = scriptedCore({ sources: [ZIP, GITHUB] });
      const release = core.holdWrites();
      renderAt(`/library/${source.id}`, core);
      const region = await filesFor(source.name);
      const action = source.source_kind === "github" ? "Sync" : "Upload ZIP";
      fireEvent.click(await within(region).findByRole("button", { name: action }));
      if (source.source_kind !== "github") forcePress(source);
      await waitFor(() => expect(core.sent).toHaveLength(1));
      forcePress(source);
      fireEvent.click(within(region).getByRole("button", { name: action }));
      expect(within(region).getByRole("status").textContent).toBe(
        "This Source's row is busy. Its status is in the Sources table.",
      );
      await act(async () => release());
      expect(await within(region).findByText(/is running/)).toBeTruthy();
      expect(core.sent).toHaveLength(1);
    },
  );
});

describe("GRE-1867 fix round 1: the files-panel button is never a silent no-op", () => {
  it.each([
    ["archive", "Upload ZIP"],
    ["github", "Sync"],
  ])("a %s Source's %s says so when its row control is not on the page", (kind, action) => {
    render(<LibraryFilesNext sourceId={1} kind={kind} running={false} />);
    fireEvent.click(screen.getByRole("button", { name: action }));
    expect(screen.getByRole("status").textContent).toBe(
      "This Source's row is not on this page. Reload Library to find it.",
    );
  });

  it("says the row is busy when its control is disabled, and presses nothing", () => {
    render(
      <>
        <LibraryFilesNext sourceId={1} kind="archive" running={false} />
        <input id={rowControlId(1, "upload")} type="file" disabled aria-label="row input" />
      </>,
    );
    const input = screen.getByLabelText("row input");
    const opened = vi.fn();
    input.addEventListener("click", opened);
    fireEvent.click(screen.getByRole("button", { name: "Upload ZIP" }));
    expect(opened).not.toHaveBeenCalled();
    expect(screen.getByRole("status").textContent).toBe(
      "This Source's row is busy. Its status is in the Sources table.",
    );
  });
});
