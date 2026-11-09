import type { Health } from "@print-partner/rust-client";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import { createMemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App, createQueryClient } from "../App";
import { routes } from "../routes";

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });

const MODULES: NonNullable<Health["modules"]> = [
  { module: "farm", state: "on" },
  { module: "spoolman", state: "off", reason: "default_off" },
  { module: "orders", state: "off", reason: "turned_off" },
  { module: "bambu", state: "off", reason: "setting_unreadable" },
  { module: "assembly_tracking", state: "on" },
];

const health = (modules: Health["modules"] = MODULES): Health => ({
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
  modules,
});

/** A fake core that answers every request with `answer`, and records the paths. */
function core(answer: () => Response | Promise<Response>) {
  return vi.fn((_path: string) => Promise.resolve().then(answer));
}

function renderSettings(fetchImpl: ReturnType<typeof core>) {
  vi.stubGlobal("fetch", fetchImpl);
  const router = createMemoryRouter(routes, { initialEntries: ["/settings"] });
  const queryClient = createQueryClient();
  queryClient.setDefaultOptions({
    queries: { ...queryClient.getDefaultOptions().queries, retry: false },
  });
  render(<App router={router} queryClient={queryClient} />);
  return { queryClient };
}

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

const moduleRow = (name: string) =>
  screen.getByText(name, { selector: "dt" }).closest(".pp-fact") as HTMLElement;

describe("SET-01 Module state and off reasons come from Rust", () => {
  it("lists every Module from GET /health, on or off, with its off reason", async () => {
    const fetchMock = core(() => json(health()));
    renderSettings(fetchMock);
    const modules = await screen.findByRole("region", { name: "Modules" });
    const names = within(modules)
      .getAllByRole("term")
      .map((dt) => dt.textContent);
    expect(names).toEqual(["Farm", "Spoolman", "Orders", "Bambu", "Assembly tracking"]);
    expect(moduleRow("Farm")).toHaveProperty("dataset.moduleState", "on");
    expect(within(moduleRow("Farm")).getByText(/^On/)).toBeTruthy();
    expect(within(moduleRow("Spoolman")).getByText("Off by default. You have not turned it on.")).toBeTruthy();
    expect(within(moduleRow("Orders")).getByText("You turned it off.")).toBeTruthy();
    expect(
      within(moduleRow("Bambu")).getByText(
        "Off because its setting could not be read. The core tries again.",
      ),
    ).toBeTruthy();
    // Read-only until the core has a Module write route: no switch and no button.
    expect(within(modules).queryByRole("button")).toBeNull();
    expect(within(modules).queryByRole("switch")).toBeNull();
    // Only the core's /health is read; nothing else is called.
    expect(new Set(fetchMock.mock.calls.map(([path]) => path))).toEqual(new Set(["/health"]));
  });

  it("names the sections in a list of in-page links, keyboard reachable", async () => {
    renderSettings(core(() => json(health())));
    await screen.findByRole("region", { name: "Modules" });
    const nav = screen.getByRole("navigation", { name: "Settings sections" });
    const links = within(nav).getAllByRole("link");
    expect(links.map((a) => [a.textContent, a.getAttribute("href")])).toEqual([
      ["Modules", "#modules"],
      ["Integrations", "#integrations"],
      ["Sharing and devices", "#sharing"],
      ["Backups", "#backups"],
    ]);
    for (const label of ["Integrations", "Sharing and devices", "Backups"]) {
      const region = screen.getByRole("region", { name: label });
      expect(within(region).getByText("Not available yet.")).toBeTruthy();
    }
  });
});

describe("SET-05 Settings screen states", () => {
  it("shows the loading state while /health has not answered", async () => {
    renderSettings(core(() => new Promise<Response>(() => {})));
    expect(await screen.findByText("Loading Settings")).toBeTruthy();
    expect(document.querySelector("[data-state=loading]")).not.toBeNull();
  });

  it("is empty when the core lists no Modules", async () => {
    const { modules: _omitted, ...withoutModules } = health();
    renderSettings(core(() => json(withoutModules)));
    expect(
      await screen.findByText("This core does not report its Modules, so there is nothing to set yet."),
    ).toBeTruthy();
    expect(document.querySelector("[data-state=empty]")).not.toBeNull();
  });

  // A /health that does not answer at all makes the frame offline (the case below). An answer
  // the generated schema rejects leaves the core reachable, so Settings shows its error.
  it("shows an error, with Retry, when /health answers with a body it cannot read", async () => {
    renderSettings(core(() => json({ ok: "yes" })));
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("Could not load Settings.");
    expect(within(alert).getByRole("button", { name: "Retry" })).toBeTruthy();
  });

  it("keeps the last Module list, with its time, when a later read fails", async () => {
    let fail = false;
    const { queryClient } = renderSettings(core(() => (fail ? json({ ok: "yes" }) : json(health()))));
    await screen.findByRole("region", { name: "Modules" });
    fail = true;
    await queryClient.refetchQueries({ queryKey: ["core", "modules"] });
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("Could not refresh Settings.");
    expect(alert.querySelector("time")).not.toBeNull();
    expect(screen.getByRole("region", { name: "Modules" })).toBeTruthy();
  });

  it("keeps the last Module list, offline, when the core goes away", async () => {
    let down = false;
    const { queryClient } = renderSettings(
      core(() => (down ? Promise.reject(new TypeError("Failed to fetch")) : json(health()))),
    );
    await screen.findByRole("region", { name: "Modules" });
    down = true;
    await queryClient.refetchQueries({ queryKey: ["core", "health"] });
    await waitFor(() => expect(document.querySelector("[data-state=offline]")).not.toBeNull());
    expect(screen.getByText(/Showing what was loaded at/)).toBeTruthy();
    expect(screen.getByRole("region", { name: "Modules" })).toBeTruthy();
    expect(screen.getByRole("navigation", { name: "Settings sections" })).toBeTruthy();
  });

  it("is offline, with no Module list, when the core cannot be reached", async () => {
    renderSettings(core(() => Promise.reject(new TypeError("Failed to fetch"))));
    await waitFor(() => expect(document.querySelector("[data-state=offline]")).not.toBeNull());
    expect(screen.getByText(/Settings has not loaded yet\./)).toBeTruthy();
    expect(screen.queryByRole("region", { name: "Modules" })).toBeNull();
    // No section exists to link to, so no section links either.
    expect(screen.queryByRole("navigation", { name: "Settings sections" })).toBeNull();
  });
});
