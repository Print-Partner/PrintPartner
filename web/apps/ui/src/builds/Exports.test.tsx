import type { Health, ProfileSummary } from "@print-partner/rust-client";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { createMemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App, createQueryClient } from "../App";
import { routes } from "../routes";

// GRE-1534: the Production tab's downloads against a scripted core. The STL pack route is
// GRE-1525 (#1780); its refusals are staged here, not on a runtime.

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });

const HEALTH: Health = {
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
  modules: [{ module: "farm", state: "off", reason: "default_off" }],
};

const ACCEPTED: ProfileSummary["freshness"] = {
  status: "current",
  accepted_input_set_id: 1,
  accepted_at: "2026-11-12T00:05:00Z",
};
const NOT_ACCEPTED: ProfileSummary["freshness"] = {
  status: "untracked",
  accepted_input_set_id: null,
  accepted_at: null,
  reasons: [{ kind: "no_accepted_inputs" }],
};

const build = (freshness: ProfileSummary["freshness"]): ProfileSummary => ({
  id: 2,
  name: "Bracket kit",
  order_number: null,
  special_request: null,
  part_count: 4,
  accepted_progress: { kind: "ready", total_units: 8, remaining_units: 3 },
  build_stale: false,
  freshness,
  archived_at: null,
  last_used_at: null,
  customer: null,
  due_date: null,
});

/** A file body that records whether anything read it. Nothing is pulled until a read. */
function trackedBody() {
  const pulled = { count: 0 };
  const body = new ReadableStream<Uint8Array>(
    {
      pull(controller) {
        pulled.count += 1;
        controller.enqueue(new Uint8Array([80, 75]));
        controller.close();
      },
    },
    { highWaterMark: 0 },
  );
  return { body, pulled };
}

type ExportAnswer = (path: string, init?: RequestInit) => Response | Promise<Response>;

function core(exportAnswer: ExportAnswer, freshness = ACCEPTED) {
  return vi.fn(async (path: string, init?: RequestInit) => {
    if (path === "/health") return json(HEALTH);
    if (path === "/api/v2/plans/2") return json(build(freshness));
    if (path === "/api/v2/plans/2/layers") return json({ profile_id: 2, layers: [] });
    if (path.startsWith("/api/v2/plans/2/exports/")) return exportAnswer(path, init);
    return json(
      { type: "urn:print-partner:problem:not-found", title: "Not Found", status: 404 },
      404,
    );
  });
}

/** Every anchor the page clicked, with its href and download attribute. */
function captureDownloads() {
  const clicked: { href: string | null; download: string | null }[] = [];
  vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(function (
    this: HTMLAnchorElement,
  ) {
    clicked.push({ href: this.getAttribute("href"), download: this.getAttribute("download") });
  });
  return clicked;
}

function renderAt(fetchMock: ReturnType<typeof core>) {
  vi.stubGlobal("fetch", fetchMock);
  const router = createMemoryRouter(routes, { initialEntries: ["/builds/2/production"] });
  const queryClient = createQueryClient();
  queryClient.setDefaultOptions({
    queries: { ...queryClient.getDefaultOptions().queries, retryDelay: 0 },
  });
  render(<App router={router} queryClient={queryClient} />);
  return router;
}

const downloads = async () => within(await screen.findByRole("region", { name: "Downloads" }));

const exportRequests = (fetchMock: ReturnType<typeof core>) =>
  fetchMock.mock.calls.filter(([path]) => path.includes("/exports/"));

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("Production tab downloads", () => {
  it.each([
    ["Download checklist", "/api/v2/plans/2/exports/checklist"],
    ["Download STL pack", "/api/v2/plans/2/exports/stl-pack"],
  ])(
    "%s asks the core first, never reads the body, then lets the browser download",
    async (label, path) => {
      const clicked = captureDownloads();
      const tracked = trackedBody();
      let signal: AbortSignal | undefined;
      const fetchMock = core((_, init) => {
        signal = init?.signal ?? undefined;
        return new Response(tracked.body, { status: 200 });
      });
      renderAt(fetchMock);
      fireEvent.click((await downloads()).getByRole("button", { name: label }));
      await waitFor(() => expect(clicked).toHaveLength(1));
      expect(clicked[0]).toEqual({ href: path, download: "" });
      expect(exportRequests(fetchMock).map(([p]) => p)).toEqual([path]);
      expect(signal?.aborted).toBe(true);
      expect(tracked.pulled.count).toBe(0);
    },
  );

  it("Remaining units only asks for the remaining scope, for the check and the download", async () => {
    const clicked = captureDownloads();
    const fetchMock = core(() => new Response(trackedBody().body, { status: 200 }));
    renderAt(fetchMock);
    const actions = await downloads();
    fireEvent.click(actions.getByRole("checkbox", { name: "Remaining units only" }));
    fireEvent.click(actions.getByRole("button", { name: "Download STL pack" }));
    await waitFor(() => expect(clicked).toHaveLength(1));
    expect(clicked[0]!.href).toBe("/api/v2/plans/2/exports/stl-pack?scope=remaining");
    expect(exportRequests(fetchMock)[0]![0]).toBe(
      "/api/v2/plans/2/exports/stl-pack?scope=remaining",
    );
  });

  it("a missing Source file names the Source, the file and the reason; nothing downloads", async () => {
    const clicked = captureDownloads();
    const fetchMock = core(() =>
      json(
        {
          type: "urn:print-partner:problem:conflict",
          title: "Conflict",
          status: 409,
          code: "source_files_unavailable",
          detail: "Source files are missing or changed since the Plan was accepted",
          files: [
            { source: "Base kit", relative_path: "plates/bracket.stl", reason: "missing" },
            { source: "Base kit", relative_path: "plates/gear.stl", reason: "digest_mismatch" },
            { source: "Addon", relative_path: "odd.stl", reason: "something_new" },
          ],
        },
        409,
      ),
    );
    renderAt(fetchMock);
    const actions = await downloads();
    fireEvent.click(actions.getByRole("button", { name: "Download STL pack" }));
    const alert = await actions.findByRole("alert");
    expect(alert.textContent).toContain(
      "Source files are missing or changed since the Plan was accepted",
    );
    expect(
      within(alert)
        .getAllByRole("listitem")
        .map((li) => li.textContent),
    ).toEqual([
      "Base kit: plates/bracket.stl is missing",
      "Base kit: plates/gear.stl changed since the Plan was accepted",
      "Addon: odd.stl is unavailable",
    ]);
    expect(clicked).toEqual([]);
    // After a refusal the action can be used again.
    expect(
      actions.getByRole("button", { name: "Download STL pack" }).getAttribute("aria-disabled"),
    ).toBeNull();
  });

  it("nothing left to pack shows the core's reason and starts no download", async () => {
    const clicked = captureDownloads();
    const fetchMock = core(() =>
      json(
        {
          type: "urn:print-partner:problem:conflict",
          title: "Conflict",
          status: 409,
          code: "nothing_to_pack",
          detail: "No Required unit is left to pack",
        },
        409,
      ),
    );
    renderAt(fetchMock);
    const actions = await downloads();
    fireEvent.click(actions.getByRole("checkbox", { name: "Remaining units only" }));
    fireEvent.click(actions.getByRole("button", { name: "Download STL pack" }));
    expect((await actions.findByRole("alert")).textContent).toBe(
      "No Required unit is left to pack",
    );
    expect(clicked).toEqual([]);
  });

  it("with the session ended, either action opens sign-in and nothing downloads", async () => {
    const clicked = captureDownloads();
    const router = renderAt(core(() => json({ detail: "refused" }, 401)));
    fireEvent.click((await downloads()).getByRole("button", { name: "Download checklist" }));
    await waitFor(() => expect(router.state.location.pathname).toBe("/login"));
    expect(router.state.location.search).toContain("next=%2Fbuilds%2F2%2Fproduction");
    expect(clicked).toEqual([]);
  });

  it("is busy until the core answers, and one press at a time", async () => {
    const clicked = captureDownloads();
    let answer: (response: Response) => void = () => {};
    const fetchMock = core(() => new Promise<Response>((resolve) => (answer = resolve)));
    renderAt(fetchMock);
    const actions = await downloads();
    fireEvent.click(actions.getByRole("button", { name: "Download checklist" }));
    const busy = await actions.findByRole("button", { name: "Preparing download" });
    expect(busy.getAttribute("aria-disabled")).toBe("true");
    fireEvent.click(busy);
    fireEvent.click(actions.getByRole("button", { name: "Download STL pack" }));
    expect(exportRequests(fetchMock)).toHaveLength(1);
    answer(new Response(trackedBody().body, { status: 200 }));
    await waitFor(() => expect(clicked).toHaveLength(1));
  });

  it("a Build with no accepted Plan shows one line and no actions", async () => {
    renderAt(core(() => json({}), NOT_ACCEPTED));
    expect(
      await screen.findByText(/No Plan has been accepted, so there is nothing to download/),
    ).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Download STL pack" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Download checklist" })).toBeNull();
  });

  it("shows the help line under the STL pack action", async () => {
    renderAt(core(() => json({})));
    const pack = (await downloads()).getByRole("button", { name: "Download STL pack" });
    const help = document.getElementById(pack.getAttribute("aria-describedby")!);
    expect(help?.textContent?.replace(/\s+/g, " ").trim()).toBe(
      "If the download fails part-way, a Source file changed while it was being packed. Download again.",
    );
  });
});
