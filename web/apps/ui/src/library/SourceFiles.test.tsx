import type { Health, SourceActivity, SourceList, StepPartTree } from "@print-partner/rust-client";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
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
    source(1, "Frame kit"),
    source(3, "Fresh import", {
      content_available: false,
      last_synced_at: null,
      last_commit_sha: null,
      current_source_revision_id: null,
      update_status: null,
    }),
  ],
};

const ACTIVITY: SourceActivity = { events: [] };

const FILES = {
  source_id: 1,
  revision_id: 41,
  files: [
    { path: "README.md", kind: "document", size_bytes: 5 },
    { path: "stl/body.stl", kind: "stl", size_bytes: 1536 },
    { path: "far/part.stl", kind: "stl", size_bytes: 2048 },
    { path: "cad/printer.step", kind: "model", size_bytes: 4096 },
  ],
};

const stepTree = (extra: Partial<StepPartTree>): StepPartTree => ({
  source_id: 1,
  path: "cad/printer.step",
  state: "queued",
  sha256: null,
  failure: null,
  parts: [],
  ...extra,
});

const part = (name: string, depth: number, instance: string | null = null) => ({
  name,
  product_id: `${name}-id`,
  instance,
  depth,
});

/** What the step-conversion route answers, in order; the last one repeats. */
let stepAnswers: StepPartTree[] = [];

interface RecordedObserver {
  callback: IntersectionObserverCallback;
}

const observers: RecordedObserver[] = [];

function installViewport() {
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function rect(
    this: HTMLElement,
  ) {
    const path = this.dataset.path;
    const top = path === "far/part.stl" ? 5000 : 8;
    return {
      x: 0,
      y: top,
      top,
      left: 0,
      right: 64,
      bottom: top + 64,
      width: 64,
      height: 64,
      toJSON() {
        return {};
      },
    } as DOMRect;
  });
  vi.stubGlobal(
    "IntersectionObserver",
    class {
      constructor(callback: IntersectionObserverCallback) {
        this.callback = callback;
        observers.push(this);
      }
      callback: IntersectionObserverCallback;
      observe() {}
      unobserve() {}
      disconnect() {}
      takeRecords() {
        return [];
      }
      root = null;
      rootMargin = "";
      thresholds = [];
    },
  );
}

function renderAt(path: string) {
  const fetchMock = vi.fn((request: string) => {
    if (request === "/health") return Promise.resolve(json(health));
    if (request === "/api/v1/sources") return Promise.resolve(json(SOURCES));
    if (request.startsWith("/api/v1/sources/activity")) return Promise.resolve(json(ACTIVITY));
    if (request === "/api/v1/sources/1/files") return Promise.resolve(json(FILES));
    if (request.startsWith("/api/v1/sources/1/step-conversion?")) {
      const answer = stepAnswers.length > 1 ? stepAnswers.shift() : stepAnswers[0];
      return Promise.resolve(json(answer));
    }
    if (request === "/api/v1/sources/3/files") {
      return Promise.resolve(json({ source_id: 3, revision_id: null, files: [] }));
    }
    return Promise.resolve(json({ detail: "missing" }, 404));
  });
  vi.stubGlobal("fetch", fetchMock);
  const router = createMemoryRouter(routes, { initialEntries: [path] });
  const queryClient = createQueryClient();
  queryClient.setDefaultOptions({
    queries: { ...queryClient.getDefaultOptions().queries, retry: false },
  });
  render(<App router={router} queryClient={queryClient} />);
  return { fetchMock, router };
}

afterEach(() => {
  stepAnswers = [];
  cleanup();
  observers.length = 0;
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

const filesRegion = () => screen.getByRole("region", { name: /Files for / });

describe("Library shows a Source's files and thumbnails", () => {
  it("lists files by folder with sizes, and loads only the thumbnails in view", async () => {
    installViewport();
    const { fetchMock } = renderAt("/library/1");
    const region = await screen.findByRole("region", { name: "Files for Frame kit" });
    expect(await within(region).findByRole("heading", { name: "Top level" })).toBeTruthy();
    expect(within(region).getByText("README.md").parentElement?.textContent).toContain("5 B");
    expect(within(region).getByRole("heading", { name: "stl" })).toBeTruthy();
    expect(within(region).getByText("body.stl").parentElement?.textContent).toContain("1.5 KB");
    const shown = document.querySelector('[data-path="stl/body.stl"] img');
    expect(shown?.getAttribute("src")).toBe("/api/v1/sources/1/thumbnail?path=stl%2Fbody.stl");
    expect(document.querySelector('[data-path="far/part.stl"] img')).toBeNull();
    expect(fetchMock.mock.calls.some(([path]) => String(path).includes("/thumbnail"))).toBe(false);
    fireEvent.load(shown!);
    await waitFor(() => expect(filesRegion().getAttribute("aria-busy")).toBeNull());
    const observer = observers[0];
    if (!observer) throw new Error("below-fold thumbnail did not observe");
    observer.callback(
      [{ isIntersecting: true } as IntersectionObserverEntry],
      observer as unknown as IntersectionObserver,
    );
    await waitFor(() =>
      expect(document.querySelector('[data-path="far/part.stl"] img')).toBeTruthy(),
    );
  });

  it("shows a placeholder when a thumbnail fails", async () => {
    installViewport();
    renderAt("/library/1");
    const image = await waitFor(() => {
      const found = document.querySelector('[data-path="stl/body.stl"] img');
      if (!found) throw new Error("thumbnail not requested");
      return found;
    });
    fireEvent.error(image);
    const region = filesRegion();
    expect(within(region).getByRole("img", { name: "No thumbnail" }).textContent).toBe(
      "No preview",
    );
    expect(document.querySelector('[data-path="stl/body.stl"] img')).toBeNull();
  });

  it("stops waiting on thumbnails when the next Source has no files", async () => {
    installViewport();
    renderAt("/library/1");
    await waitFor(() => {
      if (!document.querySelector('[data-path="stl/body.stl"] img')) {
        throw new Error("thumbnail not requested");
      }
    });
    expect(filesRegion().getAttribute("aria-busy")).toBe("true");
    fireEvent.click(screen.getByRole("link", { name: "Fresh import" }));
    expect(await screen.findByText("No files yet.")).toBeTruthy();
    expect(filesRegion().getAttribute("aria-busy")).toBeNull();
  });

  it("says no files yet when the Source has no revision", async () => {
    renderAt("/library/3");
    expect(await screen.findByText("No files yet.")).toBeTruthy();
    expect(screen.queryByRole("heading", { name: "Top level" })).toBeNull();
  });

  it("selects a Source from the list and leaves that view from its name", async () => {
    const { router, fetchMock } = renderAt("/library");
    await screen.findByRole("table", { name: "Sources" });
    expect(fetchMock.mock.calls.some(([path]) => String(path).endsWith("/files"))).toBe(false);
    fireEvent.click(screen.getByRole("link", { name: "Frame kit" }));
    expect(router.state.location.pathname).toBe("/library/1");
    expect(await screen.findByRole("region", { name: "Files for Frame kit" })).toBeTruthy();
    expect(
      screen.getByRole("link", { name: "Frame kit" }).getAttribute("aria-current"),
    ).toBe("true");
    fireEvent.click(screen.getByRole("link", { name: "Frame kit" }));
    expect(router.state.location.pathname).toBe("/library");
    await waitFor(() => expect(screen.queryByRole("region", { name: /Files for / })).toBeNull());
  });

  it("says when the address names no Source", async () => {
    renderAt("/library/9");
    expect(await screen.findByText("That Source is not in the Library.")).toBeTruthy();
  });
});

describe("Library shows a STEP file's part tree from the Rust cache (GRE-1007)", () => {
  const stepCalls = (fetchMock: ReturnType<typeof vi.fn>) =>
    fetchMock.mock.calls.filter(([path]) => String(path).includes("/step-conversion")).length;

  it("shows queued, then converting, then the nested tree, and stops asking", async () => {
    stepAnswers = [
      stepTree({ state: "queued" }),
      stepTree({ state: "converting" }),
      stepTree({
        state: "done",
        sha256: "ab".repeat(32),
        parts: [
          part("Printer", 0),
          part("Frame", 1, "Frame:1"),
          part("Carriage", 2, "Carriage:1"),
          part("Hotend", 3, "Hotend:1"),
          part("Bed", 1, "Bed:1"),
        ],
      }),
    ];
    const { fetchMock } = renderAt("/library/1");
    const region = await screen.findByRole("region", { name: "Files for Frame kit" });
    expect(stepCalls(fetchMock)).toBe(0);
    const toggle = await within(region).findByRole("button", { name: "Part tree of printer.step" });
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    fireEvent.click(toggle);
    expect(toggle.getAttribute("aria-expanded")).toBe("true");
    expect(await within(region).findByText("Queued for conversion.")).toBeTruthy();
    expect(
      fetchMock.mock.calls.some(
        ([path]) => path === "/api/v1/sources/1/step-conversion?path=cad%2Fprinter.step",
      ),
    ).toBe(true);
    expect(await within(region).findByText("Converting…", {}, { timeout: 3000 })).toBeTruthy();
    expect(await within(region).findByText("5 parts", {}, { timeout: 3000 })).toBeTruthy();

    const tree = within(region).getByRole("list", { name: "Parts of cad/printer.step" });
    const item = (scope: HTMLElement, name: string) =>
      within(scope).getAllByRole("listitem").find((li) => li.firstChild?.textContent === name)!;
    const printer = item(tree, "Printer");
    const frame = item(printer, "Frame");
    const carriage = item(frame, "Carriage");
    expect(item(carriage, "Hotend").textContent).toBe("Hotend (Hotend:1)");
    expect(within(frame).queryByText("Bed")).toBeNull();
    expect(item(printer, "Bed").parentElement?.parentElement).toBe(printer);

    const asked = stepCalls(fetchMock);
    expect(asked).toBe(3);
    await new Promise((resolve) => setTimeout(resolve, 1500));
    expect(stepCalls(fetchMock)).toBe(asked);
  });

  it("says why a STEP file could not be converted", async () => {
    stepAnswers = [stepTree({ state: "failed", failure: "no_products" })];
    renderAt("/library/1");
    const region = await screen.findByRole("region", { name: "Files for Frame kit" });
    fireEvent.click(await within(region).findByRole("button", { name: "Part tree of printer.step" }));
    const alert = await within(region).findByRole("alert");
    expect(alert.textContent).toContain("Conversion failed.");
    expect(alert.textContent).toContain("This STEP file has no assembly structure.");
  });
});
