import type { Health } from "@print-partner/rust-client";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
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
function core(answer: (path: string, init?: RequestInit) => Response | Promise<Response>) {
  return vi.fn((path: string, init?: RequestInit) => Promise.resolve().then(() => answer(path, init)));
}

function renderSettings(fetchImpl: ReturnType<typeof core>) {
  vi.stubGlobal("fetch", fetchImpl);
  const router = createMemoryRouter(routes, { initialEntries: ["/settings"] });
  const queryClient = createQueryClient();
  queryClient.setDefaultOptions({
    queries: { ...queryClient.getDefaultOptions().queries, retry: false },
  });
  render(<App router={router} queryClient={queryClient} />);
  return { queryClient, router };
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
    expect(within(modules).getAllByRole("button")).toHaveLength(5);
    expect(screen.getByRole("button", { name: "Turn Bambu on" }).getAttribute("aria-disabled")).toBe("true");
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
    const sharing = screen.getByRole("region", { name: "Sharing and devices" });
    expect(within(sharing).getByText("This screen cannot show sharing status or paired devices yet.")).toBeTruthy();
    expect(within(sharing).queryByRole("button")).toBeNull();
    for (const label of ["Integrations", "Backups"]) {
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


describe("Module changes use the Rust route", () => {
  function changingCore(effect?: (apply: () => Health["modules"]) => Response | Promise<Response>) {
    let modules = MODULES.map((m) => ({ ...m }));
    const writes: { path: string; body: unknown }[] = [];
    const fetchMock = core((path, init) => {
      if (init?.method === "PUT") {
        const body = JSON.parse(String(init.body)) as { on: boolean };
        writes.push({ path, body });
        const apply = () => {
          modules = modules.map((m) => m.module === path.split("/").at(-1)
            ? body.on ? { module: m.module, state: "on" as const }
              : { module: m.module, state: "off" as const, reason: "turned_off" as const }
            : m);
          return modules;
        };
        return effect ? effect(apply) : json({ modules: apply() });
      }
      return json(health(modules));
    });
    return { fetchMock, writes };
  }

  it("confirms an off change once, adopts Rust's state and invalidates dependent reads", async () => {
    const { fetchMock, writes } = changingCore();
    const { queryClient } = renderSettings(fetchMock);
    await screen.findByRole("button", { name: "Turn Farm off" });
    queryClient.setQueryData(["core", "builds"], { cached: true });
    fireEvent.click(screen.getByRole("button", { name: "Turn Farm off" }));
    const dialog = screen.getByRole("dialog", { name: "Turn Farm off?" });
    expect(within(dialog).getByText(/does not stop a printer/)).toBeTruthy();
    expect(writes).toHaveLength(0);
    fireEvent.click(within(dialog).getByRole("button", { name: "Confirm Module change" }));
    await screen.findByText("Module settings saved.");
    expect(writes).toEqual([{ path: "/api/v2/modules/farm", body: { on: false } }]);
    expect(moduleRow("Farm").dataset.moduleState).toBe("off");
    expect(queryClient.getQueryData<Health>(["core", "modules"])?.modules?.[0]?.state).toBe("off");
    expect(queryClient.getQueryState(["core", "builds"])?.isInvalidated).toBe(true);
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("opens sign-in with next when a Module change answers 401", async () => {
    // Only writes marked to keep their edits (the Plan's) stay on the page after a 401.
    const { fetchMock } = changingCore(() => json({ detail: "refused" }, 401));
    const { router } = renderSettings(fetchMock);
    fireEvent.click(await screen.findByRole("button", { name: "Turn Orders on" }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm Module change" }));
    await waitFor(() =>
      expect(router.state.location.pathname + router.state.location.search).toBe(
        "/login?next=%2Fsettings",
      ),
    );
  });

  it("turns on an off Module through the same absolute-state contract", async () => {
    const { fetchMock, writes } = changingCore();
    renderSettings(fetchMock);
    fireEvent.click(await screen.findByRole("button", { name: "Turn Orders on" }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm Module change" }));
    await screen.findByText("Module settings saved.");
    expect(writes).toEqual([{ path: "/api/v2/modules/orders", body: { on: true } }]);
    expect(moduleRow("Orders").dataset.moduleState).toBe("on");
  });

  it("cancels without writing and returns keyboard focus to the opener", async () => {
    const { fetchMock, writes } = changingCore();
    renderSettings(fetchMock);
    const opener = await screen.findByRole("button", { name: "Turn Farm off" });
    opener.focus();
    fireEvent.click(opener);
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(document.activeElement).toBe(opener));
    expect(writes).toHaveLength(0);
    expect(screen.queryByRole("dialog")).toBeNull();
    fireEvent.click(opener);
    fireEvent.keyDown(document, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    await waitFor(() => expect(document.activeElement).toBe(opener));
    expect(writes).toHaveLength(0);
  });

  it("does not change displayed state early or send a second pending request", async () => {
    let finish: (() => void) | undefined;
    const { fetchMock, writes } = changingCore((apply) => new Promise<Response>((resolve) => {
      finish = () => resolve(json({ modules: apply() }));
    }));
    renderSettings(fetchMock);
    fireEvent.click(await screen.findByRole("button", { name: "Turn Farm off" }));
    const confirm = screen.getByRole("button", { name: "Confirm Module change" });
    fireEvent.click(confirm);
    fireEvent.click(confirm);
    await screen.findByText("Saving Module state.");
    expect(writes).toHaveLength(1);
    expect(moduleRow("Farm").dataset.moduleState).toBe("on");
    expect(confirm.getAttribute("aria-disabled")).toBe("true");
    fireEvent.click(screen.getByRole("button", { name: "Close" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(screen.getByText("Saving Module state.")).toBeTruthy();
    finish!();
    await screen.findByText("Module settings saved.");
    expect(moduleRow("Farm").dataset.moduleState).toBe("off");
  });

  it("keeps the shared pending lock when Settings is left and remounted", async () => {
    let finish: (() => void) | undefined;
    const { fetchMock, writes } = changingCore((apply) => new Promise<Response>((resolve) => {
      finish = () => resolve(json({ modules: apply() }));
    }));
    const { router } = renderSettings(fetchMock);
    fireEvent.click(await screen.findByRole("button", { name: "Turn Farm off" }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm Module change" }));
    await screen.findByText("Saving Module state.");
    fireEvent.click(screen.getByRole("button", { name: "Close" }));
    await act(() => router.navigate("/type"));
    await act(() => router.navigate("/settings"));
    const orders = await screen.findByRole("button", { name: "Turn Orders on" });
    expect(orders.getAttribute("aria-disabled")).toBe("true");
    expect(screen.getByText("Saving Module state.")).toBeTruthy();
    fireEvent.click(orders);
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(writes).toHaveLength(1);
    finish!();
    await screen.findByRole("button", { name: "Turn Farm on" });
    await waitFor(() => expect(orders.getAttribute("aria-disabled")).toBeNull());
    expect(moduleRow("Farm").dataset.moduleState).toBe("off");
    expect(writes).toHaveLength(1);
  });

  it("keeps canonical state and does not automatically retry a refusal", async () => {
    const { fetchMock, writes } = changingCore(() => json({ detail: "Busy" }, 503));
    renderSettings(fetchMock);
    fireEvent.click(await screen.findByRole("button", { name: "Turn Farm off" }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm Module change" }));
    expect((await screen.findByRole("alert")).textContent).toContain("Busy");
    expect(moduleRow("Farm").dataset.moduleState).toBe("on");
    fireEvent.click(screen.getByRole("button", { name: "Reload Module state" }));
    expect(writes).toHaveLength(1);
  });

  it("reconciles a lost reply through reads without repeating the write", async () => {
    const { fetchMock, writes } = changingCore((apply) => {
      apply();
      throw new TypeError("Failed to fetch");
    });
    renderSettings(fetchMock);
    fireEvent.click(await screen.findByRole("button", { name: "Turn Farm off" }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm Module change" }));
    await screen.findByRole("alert");
    await waitFor(() => expect(moduleRow("Farm").dataset.moduleState).toBe("off"));
    expect(writes).toHaveLength(1);
  });

  it("keeps an unreadable Module unavailable without guessing its state", async () => {
    const { fetchMock, writes } = changingCore();
    renderSettings(fetchMock);
    fireEvent.click(await screen.findByRole("button", { name: "Turn Bambu on" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(writes).toHaveLength(0);
  });

  it("blocks an open confirmation when the core can no longer read the setting", async () => {
    const { fetchMock, writes } = changingCore();
    const { queryClient } = renderSettings(fetchMock);
    fireEvent.click(await screen.findByRole("button", { name: "Turn Farm off" }));
    act(() => {
      queryClient.setQueryData(["core", "modules"], health(MODULES.map((m) =>
        m.module === "farm" ? { module: "farm", state: "off", reason: "setting_unreadable" } : m,
      )));
    });
    const confirm = screen.getByRole("button", { name: "Confirm Module change" });
    await waitFor(() => expect(confirm.getAttribute("aria-disabled")).toBe("true"));
    expect(screen.getByRole("button", { name: "Confirm Module change", description: /cannot read/ })).toBe(confirm);
    fireEvent.click(confirm);
    expect(writes).toHaveLength(0);
  });

  it("blocks confirmation if connectivity is lost while the dialog is open", async () => {
    const { fetchMock, writes } = changingCore();
    const base = fetchMock.getMockImplementation()!;
    let offline = false;
    fetchMock.mockImplementation((path, init) => offline ? Promise.reject(new TypeError("offline")) : base(path, init));
    const { queryClient } = renderSettings(fetchMock);
    fireEvent.click(await screen.findByRole("button", { name: "Turn Farm off" }));
    offline = true;
    await queryClient.refetchQueries({ queryKey: ["core", "health"] });
    const confirm = screen.getByRole("button", { name: "Confirm Module change" });
    await waitFor(() => expect(confirm.getAttribute("aria-disabled")).toBe("true"));
    expect(screen.getByRole("button", { name: "Confirm Module change", description: /Offline:/ })).toBe(confirm);
    fireEvent.click(confirm);
    expect(writes).toHaveLength(0);
  });
});
