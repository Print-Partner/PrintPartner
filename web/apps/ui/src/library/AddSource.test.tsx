import type { CreateSourceRequest, Health, SourceList } from "@print-partner/rust-client";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { createMemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App, createQueryClient } from "../App";
import { OFFLINE_REASON } from "../frame/connection";
import { routes } from "../routes";
import { missing, sourceRequest } from "./AddSourceDialog";

// A scripted core that keeps Sources and answers `POST /api/v1/sources` as Rust does:
// a name already in use is a 400 with the reason, and nothing is added.
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

const created = (id: number, body: CreateSourceRequest): Source => ({
  id,
  name: body.name,
  url: body.url ?? "",
  source_kind: body.source_kind ?? "github",
  source_type: "git",
  role: "primary",
  category: null,
  branch: body.branch ?? "main",
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

/** `answer` replaces the create's reply after the Source is stored, as a lost or failed answer. */
function scriptedCore({ answer }: { answer?: () => Response } = {}) {
  const sources: Source[] = [];
  const posts: CreateSourceRequest[] = [];
  let down = false;
  const fetchMock = vi.fn((path: string, init?: RequestInit) => {
    if (down) return Promise.reject(new TypeError("Failed to fetch"));
    if (path === "/health") return Promise.resolve(json(health));
    if (path === "/api/v1/sources/activity?limit=100") return Promise.resolve(json({ events: [] }));
    if (path === "/api/v1/sources" && init?.method === "POST") {
      const body = JSON.parse(String(init.body)) as CreateSourceRequest;
      posts.push(body);
      if (sources.some((s) => s.name === body.name)) {
        return Promise.resolve(
          json(
            {
              type: "urn:print-partner:problem:validation",
              title: "Bad Request",
              status: 400,
              detail: `Source already exists: ${body.name}`,
            },
            400,
          ),
        );
      }
      const source = created(sources.length + 1, body);
      sources.push(source);
      return Promise.resolve(answer ? answer() : json(source));
    }
    if (path === "/api/v1/sources") return Promise.resolve(json({ sources }));
    return Promise.resolve(json({}, 404));
  });
  return { fetchMock, posts, sources, goDown: () => (down = true) };
}

function renderLibrary(fetchMock: ReturnType<typeof scriptedCore>["fetchMock"]) {
  vi.stubGlobal("fetch", fetchMock);
  const router = createMemoryRouter(routes, { initialEntries: ["/library"] });
  const queryClient = createQueryClient();
  queryClient.setDefaultOptions({
    queries: { ...queryClient.getDefaultOptions().queries, retry: false },
  });
  render(<App router={router} queryClient={queryClient} />);
}

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

const dialog = () => screen.getByRole("dialog", { name: "Add Source" });
const create = () => within(dialog()).getByRole("button", { name: "Create" });
const type = (label: string, value: string) =>
  fireEvent.change(within(dialog()).getByLabelText(label), { target: { value } });

async function openDialog() {
  fireEvent.click(await screen.findByRole("button", { name: "Add Source" }));
  return dialog();
}

describe("GRE-1365 PR 2: Add Source in Library", () => {
  it("adds a ZIP Source from the empty state with one create, and lists it", async () => {
    const core = scriptedCore();
    renderLibrary(core.fetchMock);
    expect(await screen.findByText("No Sources yet. Sources you add appear here.")).toBeTruthy();
    expect(screen.queryByRole("table")).toBeNull();
    await openDialog();
    type("Name", "Rack");
    fireEvent.click(within(dialog()).getByRole("radio", { name: "ZIP archive" }));
    fireEvent.click(create());
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    const table = await screen.findByRole("table", { name: "Sources" });
    const row = within(table).getByRole("rowheader", { name: /Rack/ }).closest("tr")!;
    expect(row.textContent).toContain("No revision yet");
    expect(row.textContent).toContain("Never synced");
    expect(core.posts).toEqual([{ name: "Rack", source_kind: "archive" }]);
  });

  it("keeps the dialog open with the core's reason when the name is in use", async () => {
    const core = scriptedCore();
    renderLibrary(core.fetchMock);
    for (const attempt of [1, 2]) {
      await openDialog();
      type("Name", "Rack");
      fireEvent.click(create());
      if (attempt === 1) await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    }
    expect((await within(dialog()).findByRole("alert")).textContent).toBe(
      "Source already exists: Rack",
    );
    const table = screen.getByRole("table", { name: "Sources", hidden: true });
    expect(within(table).getAllByRole("rowheader", { hidden: true })).toHaveLength(1);
    expect(core.sources).toHaveLength(1);
  });

  it("sends a GitHub Source's URL and branch", async () => {
    const core = scriptedCore();
    renderLibrary(core.fetchMock);
    await openDialog();
    type("Name", "Toolhead");
    fireEvent.click(within(dialog()).getByRole("radio", { name: "GitHub repository" }));
    expect((within(dialog()).getByLabelText("Branch") as HTMLInputElement).value).toBe("main");
    type("Repository URL", " https://github.com/example/toolhead ");
    type("Branch", "dev");
    fireEvent.click(create());
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(core.posts).toEqual([
      {
        name: "Toolhead",
        source_kind: "github",
        url: "https://github.com/example/toolhead",
        branch: "dev",
      },
    ]);
    const table = await screen.findByRole("table", { name: "Sources" });
    expect(within(table).getByRole("rowheader", { name: /Toolhead/ }).textContent).toContain("git");
  });

  it("disables Create without a name, or GitHub without a URL; Cancel sends nothing", async () => {
    const core = scriptedCore();
    renderLibrary(core.fetchMock);
    await openDialog();
    expect(create().getAttribute("aria-disabled")).toBe("true");
    expect(within(dialog()).getByText("Enter a name.")).toBeTruthy();
    fireEvent.click(create());
    type("Name", "Rack");
    fireEvent.click(within(dialog()).getByRole("radio", { name: "GitHub repository" }));
    expect(create().getAttribute("aria-disabled")).toBe("true");
    expect(within(dialog()).getByText("Enter the repository URL.")).toBeTruthy();
    fireEvent.click(create());
    fireEvent.click(within(dialog()).getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(core.posts).toEqual([]);
  });

  it("a 500 after the core stored the Source is not a refusal: it says so, reads again, never resends", async () => {
    const core = scriptedCore({ answer: () => json({ title: "Internal Server Error" }, 500) });
    renderLibrary(core.fetchMock);
    await openDialog();
    type("Name", "Rack");
    fireEvent.click(create());
    const alert = await within(dialog()).findByRole("alert");
    expect(alert.textContent).toMatch(
      /^Could not confirm the Source was added: Rust core answered 500/,
    );
    // The list is read again, and shows the Source the core did store.
    expect(await screen.findByRole("rowheader", { name: /Rack/, hidden: true })).toBeTruthy();
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(core.posts).toHaveLength(1);
  });

  it("offline: Create is disabled and says why", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const core = scriptedCore();
    renderLibrary(core.fetchMock);
    await openDialog();
    type("Name", "Rack");
    core.goDown();
    await vi.advanceTimersByTimeAsync(31_000);
    await waitFor(() => expect(within(dialog()).getByText(OFFLINE_REASON)).toBeTruthy());
    expect(create().getAttribute("aria-disabled")).toBe("true");
    fireEvent.click(create());
    expect(core.posts).toEqual([]);
  });
});

describe("Add Source request", () => {
  it("sends a ZIP Source as its name only, and leaves an empty branch to the core", () => {
    expect(sourceRequest({ name: " Rack ", kind: "archive", url: "x", branch: "dev" })).toEqual({
      name: "Rack",
      source_kind: "archive",
    });
    expect(sourceRequest({ name: "T", kind: "github", url: "u", branch: " " })).toEqual({
      name: "T",
      source_kind: "github",
      url: "u",
    });
    expect(missing({ name: " ", kind: "archive", url: "", branch: "main" })).toBe("Enter a name.");
  });
});
