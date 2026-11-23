import type { Health, JobSnapshot, SourceList } from "@print-partner/rust-client";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { createMemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App, createQueryClient } from "../App";
import { redirectServers } from "../core/redirectServers.test-utils";
import { routes as appRoutes } from "../routes";
import { IMPORT_POLL_MS, JOBS_KEY, importWarnings, readJobs } from "./ZipUpload";

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
  };
}

type Step =
  | { kind: "running"; progress: number | null }
  | { kind: "done"; revision: number; warnings?: string[] }
  | { kind: "error"; error: string }
  | { kind: "missing" }
  | { kind: "down" };

function snapshot(id: string, step: Step): JobSnapshot {
  const finished = step.kind === "done" || step.kind === "error";
  return {
    job_id: id,
    kind: "supplied-source-import",
    status: step.kind === "running" ? "running" : step.kind === "done" ? "done" : "error",
    message: step.kind === "done" ? "Complete" : step.kind === "error" ? "Job failed" : "Running",
    progress: step.kind === "running" ? step.progress : step.kind === "done" ? 100 : null,
    result: step.kind === "done" ? { warnings: step.warnings ?? [] } : null,
    error: step.kind === "error" ? step.error : null,
    created_at: "2026-11-16T15:00:00.000Z",
    finished_at: finished ? "2026-11-16T15:01:00.000Z" : null,
  };
}

class Core {
  sources: Source[];
  uploads: string[] = [];
  jobReads: string[] = [];
  sourceReads = 0;
  scripts = new Map<string, Step[]>();
  jobSource = new Map<string, number>();
  busy = false;
  failNextUpload = false;
  held: Promise<Response> | null = null;
  release: ((response: Response) => void) | null = null;

  constructor(sources: Source[]) {
    this.sources = sources;
  }

  holdNext() {
    this.held = new Promise((resolve) => {
      this.release = resolve;
    });
  }

  script(id: string, steps: Step[], sourceId?: number) {
    this.scripts.set(id, steps);
    if (sourceId != null) this.jobSource.set(id, sourceId);
  }

  private upload(sourceId: number, init: RequestInit): Promise<Response> {
    const file = init.body instanceof FormData ? init.body.get("file") : null;
    const filename = file instanceof File ? file.name : "";
    this.uploads.push(`${sourceId}:${filename}`);
    if (this.failNextUpload) {
      this.failNextUpload = false;
      return Promise.reject(new TypeError("Failed to fetch"));
    }
    if (this.busy) {
      return Promise.resolve(
        json(
          {
            type: "urn:print-partner:problem:conflict",
            title: "Conflict",
            status: 409,
            detail: "Another import is running; try again later",
          },
          409,
        ),
      );
    }
    const jobId = `job-${this.uploads.length}`;
    this.jobSource.set(jobId, sourceId);
    this.busy = true;
    const body = json({ job_id: jobId }, 202);
    if (!this.held) return Promise.resolve(body);
    const pending = this.held;
    this.held = null;
    return pending.then(() => body);
  }

  private job(id: string): Promise<Response> {
    this.jobReads.push(id);
    const script = this.scripts.get(id) ?? [];
    const step = script.length > 1 ? script.shift() : script[0];
    if (!step || step.kind === "missing") {
      return Promise.resolve(
        json({ type: "about:blank", title: "Not Found", status: 404, detail: "Job not found" }, 404),
      );
    }
    if (step.kind === "down") return Promise.reject(new TypeError("Failed to fetch"));
    if (step.kind === "done") {
      const row = this.sources.find((item) => item.id === this.jobSource.get(id));
      if (row) {
        row.current_source_revision_id = step.revision;
        row.content_available = true;
      }
      this.busy = false;
    }
    if (step.kind === "error") this.busy = false;
    return Promise.resolve(json(snapshot(id, step)));
  }

  fetch = vi.fn((path: string, init?: RequestInit) => {
    if (path === "/health") return Promise.resolve(json(health));
    if (path === "/api/v1/sources/activity?limit=100") return Promise.resolve(json({ events: [] }));
    if (path === "/api/v1/sources" && (!init?.method || init.method === "GET")) {
      this.sourceReads += 1;
      return Promise.resolve(json({ sources: this.sources }));
    }
    const upload = /^\/api\/v1\/sources\/(\d+)\/upload-zip$/.exec(path);
    if (upload && init?.method === "POST") return this.upload(Number(upload[1]), init);
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

const drain = () => advance(0);

/**
 * The Sources list is painted by React Query's `setTimeout(0)` notify.
 * `advanceTimersByTime(0)` does not run that timer; one millisecond does,
 * and it does not reach the next 2s job read.
 */
async function see(text: string) {
  for (let i = 0; i < 5 && !screen.queryByText(text); i += 1) {
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1);
    });
  }
  expect(screen.getByText(text)).toBeTruthy();
}

function choose(name: string, filename: string) {
  const input = screen.getByLabelText(`Upload ZIP for ${name}`);
  fireEvent.change(input, {
    target: { files: [new File(["PK"], filename, { type: "application/zip" })] },
  });
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

describe("import warnings", () => {
  it("reads only a list of strings", () => {
    expect(importWarnings(null)).toEqual([]);
    expect(importWarnings({ warnings: "nope" })).toEqual([]);
    expect(importWarnings({ warnings: ["This import is over 256 MiB.", 1] })).toEqual([
      "This import is over 256 MiB.",
    ]);
    localStorage.setItem(JOBS_KEY, "not json");
    expect(readJobs()).toEqual({});
  });
});

describe("PR 3 upload a ZIP and follow the job", () => {
  it("goes from Uploading to Importing to a revision, then a second ZIP", async () => {
    const core = new Core([
      source(1, "Rack"),
      source(2, "Frame", { source_kind: "github", source_type: "git", url: "https://github.com/a/b" }),
    ]);
    core.script("job-1", [
      { kind: "running", progress: null },
      { kind: "running", progress: 40 },
      { kind: "done", revision: 10, warnings: ["This import is over 256 MiB."] },
    ]);
    core.script("job-2", [{ kind: "done", revision: 22 }]);
    core.holdNext();
    renderLibrary(core);
    await drain();
    expect(screen.getByRole("table", { name: "Sources" })).toBeTruthy();
    expect(screen.queryByLabelText("Upload ZIP for Frame")).toBeNull();
    expect(core.uploads).toEqual([]);
    expect(core.jobReads).toEqual([]);

    choose("Rack", "kit.zip");
    expect(within(row("Rack")).getByText("Uploading")).toBeTruthy();
    expect((screen.getByLabelText("Upload ZIP for Rack") as HTMLInputElement).disabled).toBe(true);
    expect(core.uploads).toEqual(["1:kit.zip"]);

    await act(async () => {
      core.release?.(json({ job_id: "job-1" }, 202));
    });
    await drain();
    expect(within(row("Rack")).getByText("Importing")).toBeTruthy();
    expect(within(row("Rack")).getByText("No revision yet")).toBeTruthy();
    expect(JSON.parse(localStorage.getItem(JOBS_KEY) ?? "")).toEqual({ "1": "job-1" });
    expect(core.jobReads).toEqual(["job-1"]);

    const reads = core.jobReads.length;
    await advance(IMPORT_POLL_MS - 1);
    expect(core.jobReads).toHaveLength(reads);
    await advance(1);
    expect(within(row("Rack")).getByText("40%")).toBeTruthy();
    expect(core.jobReads).toHaveLength(reads + 1);

    await advance(IMPORT_POLL_MS);
    await see("Revision 10");
    expect(screen.getByRole("list", { name: "Warnings for Rack" }).textContent).toContain(
      "This import is over 256 MiB.",
    );
    expect(within(row("Rack")).queryByText("Importing")).toBeNull();
    expect(localStorage.getItem(JOBS_KEY)).toBe("{}");
    expect(core.uploads).toEqual(["1:kit.zip"]);
    expect((screen.getByLabelText("Upload ZIP for Rack") as HTMLInputElement).disabled).toBe(false);

    choose("Rack", "other.zip");
    await drain();
    await see("Revision 22");
    expect(core.uploads).toEqual(["1:kit.zip", "1:other.zip"]);
    expect(core.jobReads.filter((id) => id === "job-1")).toHaveLength(3);
  });

  it("keeps the earlier revision when the file is not a ZIP", async () => {
    const core = new Core([source(1, "Rack", { current_source_revision_id: 41, content_available: true })]);
    core.script("job-1", [{ kind: "error", error: "The file is not a readable ZIP" }]);
    renderLibrary(core);
    await drain();
    choose("Rack", "bad.zip");
    await drain();
    const rack = row("Rack");
    expect(within(rack).getByText("Import failed")).toBeTruthy();
    expect(within(rack).getByText("The file is not a readable ZIP")).toBeTruthy();
    expect(within(rack).getByText("Revision 41")).toBeTruthy();
    await advance(IMPORT_POLL_MS * 2);
    expect(core.uploads).toEqual(["1:bad.zip"]);
    expect(core.jobReads).toEqual(["job-1"]);
  });

  it("shows a 409 on a second Source and still finishes the first import", async () => {
    const core = new Core([source(1, "Rack"), source(3, "Hinge")]);
    core.script("job-1", [{ kind: "running", progress: null }]);
    renderLibrary(core);
    await drain();
    choose("Rack", "kit.zip");
    await drain();
    expect(within(row("Rack")).getByText("Importing")).toBeTruthy();
    choose("Hinge", "hinge.zip");
    await drain();
    expect(within(row("Hinge")).getByText("Another import is running; try again later")).toBeTruthy();
    expect(within(row("Rack")).getByText("Importing")).toBeTruthy();
    expect(core.uploads).toEqual(["1:kit.zip", "3:hinge.zip"]);
    core.script("job-1", [{ kind: "done", revision: 8 }]);
    await advance(IMPORT_POLL_MS);
    await see("Revision 8");
    expect(within(row("Hinge")).getByText("Another import is running; try again later")).toBeTruthy();
    expect(core.uploads).toHaveLength(2);
  });

  it("follows a reload without uploading again", async () => {
    const core = new Core([source(1, "Rack")]);
    core.script("job-1", [
      { kind: "running", progress: 10 },
      { kind: "running", progress: 10 },
      { kind: "done", revision: 15 },
    ]);
    renderLibrary(core);
    await drain();
    choose("Rack", "kit.zip");
    await drain();
    expect(within(row("Rack")).getByText("Importing")).toBeTruthy();
    expect(core.uploads).toEqual(["1:kit.zip"]);
    cleanup();
    renderLibrary(core);
    await drain();
    expect(within(row("Rack")).getByText("Importing")).toBeTruthy();
    expect(core.uploads).toEqual(["1:kit.zip"]);
    await advance(IMPORT_POLL_MS);
    await see("Revision 15");
    expect(core.uploads).toEqual(["1:kit.zip"]);
  });

  it("keeps Importing while the core is down, then shows the revision, and does not upload", async () => {
    localStorage.setItem(JOBS_KEY, JSON.stringify({ "1": "job-kept" }));
    const core = new Core([source(1, "Rack")]);
    core.script(
      "job-kept",
      [
        { kind: "running", progress: null },
        { kind: "down" },
        { kind: "running", progress: null },
        { kind: "done", revision: 19 },
      ],
      1,
    );
    renderLibrary(core);
    await drain();
    expect(within(row("Rack")).getByText("Importing")).toBeTruthy();
    expect(core.uploads).toEqual([]);
    await advance(IMPORT_POLL_MS);
    expect(within(row("Rack")).getByText("Importing")).toBeTruthy();
    expect(core.jobReads).toEqual(["job-kept", "job-kept"]);
    await advance(IMPORT_POLL_MS);
    expect(within(row("Rack")).getByText("Importing")).toBeTruthy();
    await advance(IMPORT_POLL_MS);
    await see("Revision 19");
    expect(core.uploads).toEqual([]);
    expect(core.jobReads).toEqual(["job-kept", "job-kept", "job-kept", "job-kept"]);
  });

  it("forgets a job the core no longer knows and reads Sources again", async () => {
    localStorage.setItem(JOBS_KEY, JSON.stringify({ "1": "job-gone" }));
    const core = new Core([source(1, "Rack")]);
    core.script("job-gone", [{ kind: "missing" }], 1);
    renderLibrary(core);
    await drain();
    expect(within(row("Rack")).getByText("Never synced")).toBeTruthy();
    expect(within(row("Rack")).queryByText("Importing")).toBeNull();
    expect(localStorage.getItem(JOBS_KEY)).toBe("{}");
    expect(core.sourceReads).toBeGreaterThan(1);
    expect(core.uploads).toEqual([]);
    await advance(IMPORT_POLL_MS * 2);
    expect(core.jobReads).toEqual(["job-gone"]);
  });

  it("does not send the upload again after a lost answer", async () => {
    const core = new Core([source(1, "Rack")]);
    core.failNextUpload = true;
    renderLibrary(core);
    await drain();
    choose("Rack", "kit.zip");
    await drain();
    expect(within(row("Rack")).getByText(/It was not sent again/)).toBeTruthy();
    expect(localStorage.getItem(JOBS_KEY)).toBeNull();
    await advance(IMPORT_POLL_MS * 2);
    expect(core.uploads).toEqual(["1:kit.zip"]);
    expect(core.jobReads).toEqual([]);
  });

  it("does not post a file whose name is not .zip", async () => {
    const core = new Core([source(1, "Rack")]);
    renderLibrary(core);
    await drain();
    choose("Rack", "notes.txt");
    await drain();
    expect(within(row("Rack")).getByText("Choose a .zip file.")).toBeTruthy();
    expect(core.uploads).toEqual([]);
  });

  it("says the job status cannot be read instead of staying Importing on a redirected poll", async () => {
    vi.useRealTimers();
    const servers = await redirectServers(snapshot("job-1", { kind: "done", revision: 10 }));
    try {
      servers.redirectWith(307);
      localStorage.setItem(JOBS_KEY, JSON.stringify({ "1": "job-1" }));
      const core = new Core([source(1, "Rack")]);
      const original = core.fetch;
      core.fetch = vi.fn((p: string, init?: RequestInit) => (p.startsWith("/api/v1/jobs/") ? servers.forward(p, init) : original(p, init)));
      renderLibrary(core);
      const tr = (await screen.findByRole("rowheader", { name: /Rack/ })).closest("tr") as HTMLElement;
      expect(await within(tr).findByText("Job status cannot be read")).toBeTruthy();
      expect(within(tr).getByText(/GET \/api\/v1\/jobs\/job-1 was answered with a redirect/)).toBeTruthy();
      expect(within(tr).queryByText("Importing")).toBeNull();
      expect(within(tr).queryByText("Revision 10")).toBeNull();
      expect(servers.first).toEqual(["GET /api/v1/jobs/job-1"]);
      expect(servers.destination).toEqual([]);
    } finally { await servers.close(); }
  });
});
