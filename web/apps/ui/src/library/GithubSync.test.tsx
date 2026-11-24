import type { Health, JobSnapshot, SourceList } from "@print-partner/rust-client";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { createMemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App, createQueryClient } from "../App";
import { routes as appRoutes } from "../routes";
import { syncFailure } from "./GithubSync";
import { JOBS_KEY, readJobs } from "./libraryJobs";
import { IMPORT_POLL_MS as SYNC_POLL_MS } from "./ZipUpload";

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

function source(id: number, name: string, extra: Partial<Source> = {}): Source {
  return {
    id,
    name,
    url: "https://github.com/maker/kit",
    source_kind: "github",
    source_type: "git",
    role: "primary",
    category: null,
    branch: "main",
    tag: null,
    local_path: null,
    content_available: true,
    last_synced_at: "2026-11-01T15:00:00.000Z",
    last_commit_sha: "0123456789abcdef0123456789abcdef01234567",
    current_source_revision_id: 41,
    docs_url: null,
    manifest_community_slug: null,
    metadata: null,
    naming_use_defaults: true,
    update_status: "up_to_date",
    update_checked_at: null,
    doc_count: 0,
    ...extra,
  };
}

type Step =
  | { kind: "running"; progress: number | null }
  | { kind: "done"; revision: number; branch?: string; sha?: string }
  | { kind: "error"; error: string }
  | { kind: "failed"; error: string }
  | { kind: "missing" }
  | { kind: "down" };

const SHA = "abcdef1234567890abcdef1234567890ab";

function snapshot(id: string, step: Step): JobSnapshot {
  const finished = step.kind === "done" || step.kind === "error";
  const result =
    step.kind === "failed"
      ? {
          synced: 0,
          failed: 1,
          results: [],
          failures: [{ project_id: 4, name: "Kit", error: step.error }],
        }
      : step.kind === "done"
        ? { synced: 1, failed: 0, results: [], failures: [] }
        : null;
  return {
    job_id: id,
    kind: "sync",
    status: step.kind === "running" ? "running" : step.kind === "error" ? "error" : "done",
    message: finished ? "Complete" : "Running",
    progress: step.kind === "running" ? step.progress : finished ? 100 : null,
    result,
    error: step.kind === "error" ? step.error : null,
    created_at: "2026-11-16T18:00:00.000Z",
    finished_at: finished || step.kind === "failed" ? "2026-11-16T18:04:00.000Z" : null,
  };
}

class Core {
  sources: Source[];
  syncs: number[][] = [];
  branchUrls: string[] = [];
  jobReads: string[] = [];
  scripts = new Map<string, Step[]>();

  constructor(sources: Source[]) {
    this.sources = sources;
  }

  script(id: string, steps: Step[]) {
    this.scripts.set(id, steps);
  }

  private job(id: string): Promise<Response> {
    this.jobReads.push(id);
    const script = this.scripts.get(id) ?? [];
    const step = script.length > 1 ? script.shift() : script[0];
    if (!step || step.kind === "missing") {
      return Promise.resolve(
        json(
          { type: "about:blank", title: "Not Found", status: 404, detail: "Job not found" },
          404,
        ),
      );
    }
    if (step.kind === "down") return Promise.reject(new TypeError("Failed to fetch"));
    if (step.kind === "done") {
      const row = this.sources.find((item) => String(item.id) === Object.keys(readJobs())[0]);
      const named = this.sources.find(
        (item) => item.id === Number(Object.entries(readJobs()).find(([, job]) => job === id)?.[0]),
      );
      const target = named ?? row;
      if (target) {
        target.current_source_revision_id = step.revision;
        target.branch = step.branch ?? target.branch;
        target.last_commit_sha = step.sha ?? SHA;
        target.last_synced_at = "2026-11-16T18:04:00.000Z";
        target.update_status = "up_to_date";
      }
    }
    return Promise.resolve(json(snapshot(id, step)));
  }

  fetch = vi.fn((path: string, init?: RequestInit) => {
    if (path === "/health") return Promise.resolve(json(health));
    if (path === "/api/v1/sources/activity?limit=100") return Promise.resolve(json({ events: [] }));
    if (path.startsWith("/api/v1/sources/github-branches")) {
      const url = new URL(path, "http://127.0.0.1").searchParams.get("url") ?? "";
      this.branchUrls.push(url);
      if (url.includes("missing")) {
        return Promise.resolve(json({ detail: "GitHub repository not found" }, 404));
      }
      if (url.includes("down")) {
        return Promise.resolve(json({ detail: "GitHub could not answer" }, 502));
      }
      return Promise.resolve(
        json({
          owner: "maker",
          repo: "kit",
          default_branch: "develop",
          url_branch: null,
          branches: ["develop", "main"],
        }),
      );
    }
    if (path === "/api/v1/sources" && init?.method === "POST") {
      const body = JSON.parse(String(init.body)) as { name: string; url?: string; branch?: string };
      const created = source(this.sources.length + 1, body.name, {
        url: body.url ?? "",
        branch: body.branch ?? "main",
        content_available: false,
        last_synced_at: null,
        last_commit_sha: null,
        current_source_revision_id: null,
        update_status: null,
      });
      this.sources.push(created);
      return Promise.resolve(json(created));
    }
    if (path === "/api/v1/sources") return Promise.resolve(json({ sources: this.sources }));
    if (path === "/api/v1/jobs/sync" && init?.method === "POST") {
      const body = JSON.parse(String(init.body)) as { project_ids: number[] };
      this.syncs.push(body.project_ids);
      return Promise.resolve(json({ job_id: `job-${this.syncs.length}` }, 202));
    }
    if (path.startsWith("/api/v1/jobs/")) {
      return this.job(decodeURIComponent(path.slice("/api/v1/jobs/".length)));
    }
    return Promise.resolve(json({ detail: "not found" }, 404));
  });
}

function renderLibrary(core: Core) {
  vi.stubGlobal("fetch", core.fetch);
  const router = createMemoryRouter(appRoutes, { initialEntries: ["/library"] });
  const queryClient = createQueryClient();
  queryClient.setDefaultOptions({
    queries: { ...queryClient.getDefaultOptions().queries, retry: false },
  });
  render(<App router={router} queryClient={queryClient} />);
}

async function advance(ms: number) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

async function see(text: string) {
  for (let i = 0; i < 8 && !screen.queryByText(text); i += 1) {
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1);
    });
  }
  expect(screen.getByText(text)).toBeTruthy();
}

async function seeButton(name: string) {
  for (let i = 0; i < 8 && !screen.queryByRole("button", { name }); i += 1) {
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1);
    });
  }
  return screen.getByRole("button", { name }) as HTMLButtonElement;
}

const row = (name: string) =>
  screen.getByRole("rowheader", { name: new RegExp(name) }).closest("tr") as HTMLElement;

beforeEach(() => {
  localStorage.clear();
  vi.useFakeTimers();
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  localStorage.clear();
});

describe("sync failure text", () => {
  it("reads the failure that names this Source", () => {
    expect(syncFailure(4, null)).toBeNull();
    expect(syncFailure(4, { failures: [{ project_id: 9, error: "other" }] })).toBeNull();
    expect(
      syncFailure(4, { failures: [{ project_id: 4, name: "Kit", error: "Repository not found" }] }),
    ).toBe("Repository not found");
    localStorage.setItem(JOBS_KEY, "{");
    expect(readJobs()).toEqual({});
  });
});

describe("GRE-1728 GitHub branch list and sync", () => {
  it("loads branches on blur, selects the default, and keeps the text field when the read fails", async () => {
    const core = new Core([]);
    renderLibrary(core);
    await see("Add Source");
    fireEvent.click(screen.getByRole("button", { name: "Add Source" }));
    const dialog = screen.getByRole("dialog", { name: "Add Source" });
    fireEvent.click(within(dialog).getByRole("radio", { name: "GitHub repository" }));
    const branch = () => within(dialog).getByLabelText("Branch");
    expect((branch() as HTMLInputElement).value).toBe("main");
    fireEvent.blur(branch());
    expect(core.branchUrls).toEqual([]);

    const url = within(dialog).getByLabelText("Repository URL");
    fireEvent.change(url, { target: { value: " https://github.com/maker/kit " } });
    fireEvent.blur(url);
    await see("develop");
    const list = within(dialog).getByRole("combobox", { name: "Branch" }) as HTMLSelectElement;
    expect(list.value).toBe("develop");
    expect([...list.options].map((option) => option.value)).toEqual(["develop", "main"]);
    expect(core.branchUrls).toEqual(["https://github.com/maker/kit"]);

    fireEvent.change(url, { target: { value: "https://github.com/maker/missing" } });
    fireEvent.blur(url);
    await see("GitHub repository not found");
    expect(within(dialog).getByRole("textbox", { name: "Branch" })).toBeTruthy();
    expect(within(dialog).queryByRole("combobox", { name: "Branch" })).toBeNull();

    fireEvent.change(url, { target: { value: "https://github.com/maker/down" } });
    fireEvent.blur(url);
    await see("GitHub could not answer");
    expect(within(dialog).getByRole("textbox", { name: "Branch" })).toBeTruthy();
  });

  it("starts one sync after create, shows 40%, then the commit and Last synced", async () => {
    const core = new Core([]);
    core.script("job-1", [
      { kind: "running", progress: 40 },
      { kind: "done", revision: 7, branch: "develop", sha: SHA },
    ]);
    renderLibrary(core);
    await see("Add Source");
    fireEvent.click(screen.getByRole("button", { name: "Add Source" }));
    const dialog = screen.getByRole("dialog", { name: "Add Source" });
    fireEvent.change(within(dialog).getByLabelText("Name"), { target: { value: "Kit" } });
    fireEvent.click(within(dialog).getByRole("radio", { name: "GitHub repository" }));
    const url = within(dialog).getByLabelText("Repository URL");
    fireEvent.change(url, { target: { value: "https://github.com/maker/kit" } });
    fireEvent.blur(url);
    await see("develop");
    fireEvent.click(within(dialog).getByRole("button", { name: "Create" }));
    await see("Syncing");
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(core.syncs).toEqual([[1]]);
    expect(within(row("Kit")).getByText("40%")).toBeTruthy();
    expect(JSON.parse(localStorage.getItem(JOBS_KEY) ?? "")).toEqual({ "1": "job-1" });
    await advance(SYNC_POLL_MS);
    await see("develop at abcdef1");
    expect(row("Kit").textContent).toContain("Last synced");
    expect(row("Kit").textContent).toContain("Revision 7");
    expect(core.syncs).toEqual([[1]]);
    expect(localStorage.getItem(JOBS_KEY)).toBe("{}");
  });

  it("says Sync failed and keeps the earlier revision when the job errors", async () => {
    const core = new Core([source(4, "Kit")]);
    core.script("job-1", [
      { kind: "running", progress: null },
      { kind: "error", error: "The repository is gone" },
    ]);
    renderLibrary(core);
    await seeButton("Sync Kit");
    const button = () => screen.getByRole("button", { name: "Sync Kit" }) as HTMLButtonElement;
    expect(within(row("Kit")).getByText("main at 0123456")).toBeTruthy();
    fireEvent.click(button());
    await see("Syncing");
    expect(button().disabled).toBe(true);
    await advance(0);
    expect(core.jobReads).toEqual(["job-1"]);
    expect(button().disabled).toBe(true);
    await advance(SYNC_POLL_MS);
    await see("Sync failed");
    const kit = row("Kit");
    expect(within(kit).getByText("The repository is gone")).toBeTruthy();
    expect(within(kit).getByText("main at 0123456")).toBeTruthy();
    expect(within(kit).getByText("Revision 41")).toBeTruthy();
    expect(button().disabled).toBe(false);
    expect(core.syncs).toEqual([[4]]);
    await advance(SYNC_POLL_MS * 2);
    expect(core.syncs).toEqual([[4]]);
  });

  it("says Sync failed when a finished job's failures name the Source", async () => {
    const core = new Core([source(4, "Kit")]);
    core.script("job-1", [{ kind: "failed", error: "Repository not found" }]);
    renderLibrary(core);
    fireEvent.click(await seeButton("Sync Kit"));
    await see("Sync failed");
    expect(within(row("Kit")).getByText("Repository not found")).toBeTruthy();
    expect(within(row("Kit")).getByText("Revision 41")).toBeTruthy();
    expect(core.syncs).toEqual([[4]]);
  });

  it("keeps Syncing across a failed read, then the same job, and posts once", async () => {
    const core = new Core([source(4, "Kit")]);
    core.script("job-1", [
      { kind: "down" },
      { kind: "running", progress: null },
      { kind: "done", revision: 12, branch: "main", sha: SHA },
    ]);
    renderLibrary(core);
    fireEvent.click(await seeButton("Sync Kit"));
    await see("Syncing");
    expect(core.syncs).toEqual([[4]]);
    await advance(0);
    expect(within(row("Kit")).getByText("Syncing")).toBeTruthy();
    expect(core.jobReads).toEqual(["job-1"]);
    await advance(SYNC_POLL_MS);
    expect(within(row("Kit")).getByText("Syncing")).toBeTruthy();
    expect(core.jobReads).toEqual(["job-1", "job-1"]);
    await advance(SYNC_POLL_MS);
    await see("Revision 12");
    expect(within(row("Kit")).getByText("main at abcdef1")).toBeTruthy();
    expect(core.syncs).toEqual([[4]]);
    expect(core.jobReads).toEqual(["job-1", "job-1", "job-1"]);
  });

  it("follows a reload of the same job and does not sync again", async () => {
    localStorage.setItem(JOBS_KEY, JSON.stringify({ "4": "job-kept" }));
    const core = new Core([source(4, "Kit")]);
    core.script("job-kept", [
      { kind: "running", progress: 10 },
      { kind: "running", progress: 10 },
      { kind: "done", revision: 19, branch: "main", sha: SHA },
    ]);
    renderLibrary(core);
    await see("Syncing");
    expect(core.syncs).toEqual([]);
    cleanup();
    renderLibrary(core);
    await see("Syncing");
    expect(core.syncs).toEqual([]);
    expect(core.jobReads[0]).toBe("job-kept");
    await advance(SYNC_POLL_MS);
    await see("Revision 19");
    expect(core.syncs).toEqual([]);
  });
});
