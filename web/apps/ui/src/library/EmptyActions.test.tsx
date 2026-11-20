import type { Health, ProfileSummary, SourceList } from "@print-partner/rust-client";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { createMemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App, createQueryClient } from "../App";
import { routes } from "../routes";
import { JOBS_KEY } from "./ZipUpload";

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
  const running = (path: string) =>
    json({
      job_id: decodeURIComponent(path.slice("/api/v1/jobs/".length)),
      kind: "supplied-source-import",
      status: "running",
      message: "Running",
      progress: null,
      result: null,
      error: null,
      created_at: "2026-11-20T10:00:00.000Z",
      finished_at: null,
    });
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
  const read = (path: string): Response => {
    if (path === "/health") return json(health);
    if (path === "/api/v1/sources/activity?limit=100") return json({ events: [] });
    if (path === "/api/v1/sources") {
      return sourcesFail
        ? json({ type: "about:blank", title: "Unavailable", status: 503, detail: "down" }, 503)
        : json({ sources });
    }
    const list = /^\/api\/v1\/sources\/(\d+)\/files$/.exec(path);
    if (list) return fileList(Number(list[1]));
    if (path.startsWith("/api/v1/jobs/")) return running(path);
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
  const fetchMock = vi.fn((path: string, init?: RequestInit) =>
    Promise.resolve(!init?.method || init.method === "GET" ? read(path) : write(path, init)),
  );
  return { fetchMock, sent };
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
        "Its import is running. Its files appear here when the import finishes.",
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
        "Its sync is running. Its files appear here when the sync finishes.",
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
