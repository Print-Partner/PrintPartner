import type {
  Health,
  ProfileLayers,
  ProfileSummary,
  SourceActivity,
  SourceList,
} from "@print-partner/rust-client";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import { createMemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App, createQueryClient } from "../App";
import { routes } from "../routes";

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

const source = (id: number, name: string, extra: Partial<Source> = {}): Source => ({
  id,
  name,
  url: `https://example.com/${id}.git`,
  source_kind: "git",
  source_type: "kit",
  role: "primary",
  category: null,
  branch: "main",
  tag: null,
  local_path: null,
  content_available: true,
  last_synced_at: "2026-11-05T10:00:00.000Z",
  last_commit_sha: "0123456789abcdef0123456789abcdef01234567",
  current_source_revision_id: 40 + id,
  docs_url: null,
  manifest_community_slug: null,
  metadata: null,
  naming_use_defaults: true,
  update_status: "up_to_date",
  update_checked_at: null,
  doc_count: 0,
  ...extra,
});

const SOURCES: SourceList = {
  sources: [
    source(1, "Frame kit", { tag: "v2.1", category: "Frames" }),
    source(2, "Hinge pack", { update_status: "updates_available" }),
    source(3, "Fresh import", {
      content_available: false,
      last_synced_at: null,
      last_commit_sha: null,
      current_source_revision_id: null,
      update_status: null,
    }),
  ],
};

const ACTIVITY: SourceActivity = {
  events: [
    {
      id: 9,
      at: "2026-11-06T09:00:00.000Z",
      kind: "source.sync_failed",
      source_id: 2,
      source_name: "Hinge pack",
      detail: "network unreachable",
    },
    {
      id: 8,
      at: "2026-11-05T09:00:00.000Z",
      kind: "source.update_available",
      source_id: 2,
      source_name: "Hinge pack",
      detail: null,
    },
    {
      id: 7,
      at: "2026-11-04T09:00:00.000Z",
      kind: "source.updated",
      source_id: 1,
      source_name: "Frame kit",
      detail: null,
    },
  ],
};

type Handler = (path: string) => Response | Promise<Response>;

function core(handler: Handler) {
  return vi.fn((path: string) =>
    Promise.resolve(path === "/health" ? json(health) : handler(path)),
  );
}

function renderAt(path: string, fetchImpl: ReturnType<typeof core>) {
  vi.stubGlobal("fetch", fetchImpl);
  const router = createMemoryRouter(routes, { initialEntries: [path] });
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
});

const row = (table: HTMLElement, name: string) =>
  within(table)
    .getByRole("rowheader", { name: new RegExp(name) })
    .closest("tr") as HTMLElement;

describe("UI14-01 Library lists Sources with revision, sync state and activity", () => {
  it("shows each Source's revision, sync state and newest activity from Rust", async () => {
    const fetchMock = core((path) =>
      path === "/api/v1/sources"
        ? json(SOURCES)
        : path === "/api/v1/sources/activity"
          ? json(ACTIVITY)
          : json({}, 404),
    );
    renderAt("/library", fetchMock);
    const table = await screen.findByRole("table", { name: "Sources" });
    const frame = row(table, "Frame kit");
    expect(within(frame).getByText("v2.1 at 0123456")).toBeTruthy();
    expect(within(frame).getByText("Revision 41")).toBeTruthy();
    expect(within(frame).getByText("Up to date")).toBeTruthy();
    expect(within(frame).getByText("kit, Frames")).toBeTruthy();
    await waitFor(() => expect(within(frame).getByText("Updated")).toBeTruthy());
    const hinge = row(table, "Hinge pack");
    expect(within(hinge).getByText("main at 0123456")).toBeTruthy();
    expect(within(hinge).getByText("Update available")).toBeTruthy();
    // The newest event wins; the older "Update available" event stays in Recent activity.
    expect(within(hinge).getByText("Sync failed: network unreachable")).toBeTruthy();
    const fresh = row(table, "Fresh import");
    expect(within(fresh).getByText("No revision yet")).toBeTruthy();
    expect(within(fresh).getAllByText("Never synced")).toHaveLength(1);
    expect(within(fresh).getByText("No files on disk")).toBeTruthy();
    expect(within(fresh).getByText("None recorded")).toBeTruthy();
    const recent = screen.getByRole("region", { name: "Recent activity" });
    expect(within(recent).getAllByRole("listitem")).toHaveLength(3);
    // Read only: no buttons in the list.
    expect(within(table).queryByRole("button")).toBeNull();
  });

  it("keeps the Sources when activity fails, and says so", async () => {
    const fetchMock = core((path) =>
      path === "/api/v1/sources" ? json(SOURCES) : json({ detail: "boom" }, 503),
    );
    renderAt("/library", fetchMock);
    const table = await screen.findByRole("table", { name: "Sources" });
    expect(await screen.findByText("Could not load Source activity.")).toBeTruthy();
    expect(within(row(table, "Frame kit")).getByText("Not loaded")).toBeTruthy();
  });

  it("says when the Library has no Sources", async () => {
    renderAt(
      "/library",
      core(() => json({ sources: [], events: [] })),
    );
    expect(await screen.findByText("No Sources yet. Sources you add appear here.")).toBeTruthy();
  });
});

const summary = (freshness: ProfileSummary["freshness"]): ProfileSummary => ({
  id: 5,
  name: "Desk organiser",
  order_number: null,
  special_request: null,
  part_count: 12,
  accepted_progress: { kind: "empty" },
  build_stale: false,
  freshness,
  archived_at: null,
  last_used_at: null,
  customer: null,
  due_date: null,
});

const LAYERS: ProfileLayers = {
  profile_id: 5,
  layers: [
    { id: 11, layer_order: 1, layer_type: "addon", project_id: 2, project_name: "Hinge pack" },
    { id: 10, layer_order: 0, layer_type: "base", project_id: 1, project_name: "Frame kit" },
    { id: 12, layer_order: 2, layer_type: "custom", project_id: null, project_name: null },
  ],
};

function buildCore(
  freshness: ProfileSummary["freshness"],
  sources: () => Response = () => json(SOURCES),
) {
  return core((path) => {
    if (path === "/api/v2/plans/5") return json(summary(freshness));
    if (path === "/api/v2/plans/5/layers") return json(LAYERS);
    if (path === "/api/v1/sources") return sources();
    return json({}, 404);
  });
}

describe("UI14-04 The Sources tab shows which Source revisions the Build uses", () => {
  it("lists each layer's Source with its current and accepted revision", async () => {
    renderAt(
      "/builds/5/sources",
      buildCore({
        status: "stale",
        accepted_at: "2026-11-01T10:00:00.000Z",
        accepted_input_set_id: 3,
        reasons: [
          {
            kind: "source_revision_changed",
            source_id: 2,
            source_name: "Hinge pack",
            accepted_revision_id: 30,
            current_revision_id: 42,
          },
        ],
        untracked_sources: [],
      }),
    );
    const table = await screen.findByRole("table", { name: "Build Sources" });
    const rows = within(table).getAllByRole("row").slice(1);
    expect(rows.map((r) => within(r).getAllByRole("cell")[0]!.textContent)).toEqual([
      "base",
      "addon",
      "custom",
    ]);
    await waitFor(() => expect(within(rows[0]!).getByText("v2.1 at 0123456")).toBeTruthy());
    expect(within(rows[0]!).getByText("The current revision")).toBeTruthy();
    expect(within(rows[1]!).getByText("Revision 30")).toBeTruthy();
    expect(within(rows[1]!).getByText("The Source is now at revision 42")).toBeTruthy();
    expect(within(rows[2]!).getByText("No Source on this layer")).toBeTruthy();
    expect(screen.getByRole("link", { name: "Sources" }).getAttribute("aria-current")).toBe("page");
  });

  it("says no Plan is accepted yet, and keeps the layers when Sources fail", async () => {
    renderAt(
      "/builds/5/sources",
      buildCore(
        {
          status: "untracked",
          accepted_at: null,
          accepted_input_set_id: null,
          reasons: [{ kind: "no_accepted_inputs" }],
        },
        () => json({ detail: "boom" }, 503),
      ),
    );
    const table = await screen.findByRole("table", { name: "Build Sources" });
    expect(await screen.findByText("Could not load the Library's Sources.")).toBeTruthy();
    expect(within(table).getAllByText(/^Could not load:/)).toHaveLength(2);
  });

  it("shows No Plan accepted yet beside each Source", async () => {
    renderAt(
      "/builds/5/sources",
      buildCore({
        status: "untracked",
        accepted_at: null,
        accepted_input_set_id: null,
        reasons: [{ kind: "no_accepted_inputs" }],
      }),
    );
    const table = await screen.findByRole("table", { name: "Build Sources" });
    await waitFor(() => expect(within(table).getAllByText("No Plan accepted yet")).toHaveLength(2));
  });
});
