import type { Health } from "@print-partner/rust-client";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { createMemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App, createQueryClient } from "../App";
import { redirectServers } from "../core/redirectServers.test-utils";
import { routes } from "../routes";

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
const problem = (status: number, detail: string, code?: string) =>
  new Response(JSON.stringify({
    type: status === 409 ? "urn:print-partner:problem:conflict" : "about:blank",
    title: "Refused", status, detail, code,
  }), {
    status,
    headers: { "content-type": "application/problem+json" },
  });

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
  modules: [{ module: "farm", state: "on" }],
};

const SECRET = "canary-secret-7f3a";
const view = (id: string, kind: string, name: string, more: Record<string, unknown> = {}) => ({
  id,
  kind,
  name,
  enabled: true,
  base_url: null,
  username: null,
  has_api_key: false,
  has_password: false,
  revision: kind === "bambu" ? null : "fixture-revision",
  ...more,
});
const LIST = {
  integrations: [
    view("m1", "moonraker", "Voron", { base_url: "http://192.168.1.5:7125/", has_api_key: true, api_key: SECRET }),
    view("p1", "prusalink", "MK4", { base_url: "http://192.168.1.6/", username: "maker", has_password: true, password: SECRET }),
    view("s1", "spoolman", "Spools", { base_url: "http://192.168.1.7:7912/", enabled: false }),
    view("b1", "bambu", "X1C"),
  ],
};
const PINNED = { pinned: true, sha256_prefix: "ab12cd34ef56", pinned_at: "2026-11-07T12:00:00Z" };

type Answer = (path: string, init?: RequestInit) => Response | Promise<Response>;

/**
 * A fake core: /health and the list as given, the Bambu pin from `pin`, no printers unless
 * `other` answers them; records every call.
 */
function core({
  list = (): Response => json(LIST),
  pin = (_init?: RequestInit): Response | Promise<Response> => json(PINNED),
  reset = (_init?: RequestInit): Response | Promise<Response> => json({ reset: true }),
  other = (_path: string, _init?: RequestInit): Response | undefined => undefined,
} = {}) {
  let down = false;
  const answer: Answer = (path, init) => {
    if (down) return Promise.reject(new TypeError("Failed to fetch"));
    const answered = other(path, init);
    if (answered) return answered;
    if (path === "/health") return json(HEALTH);
    if (path === "/api/v2/printers") return json({ printers: [] });
    if (path === "/api/v2/integrations") return list();
    if (path === "/api/v1/integrations/b1/tls-pin") return pin(init);
    if (path === "/api/v1/integrations/b1/tls-pin/reset" && init?.method === "POST") return reset(init);
    return problem(404, `unexpected ${path}`);
  };
  const fetchMock = vi.fn((path: string, init?: RequestInit) => Promise.resolve().then(() => answer(path, init)));
  const resets = () => fetchMock.mock.calls.filter(([path]) => path.endsWith("/tls-pin/reset"));
  return { fetchMock, resets, goDown: () => (down = true) };
}

function renderSettings(fetchMock: ReturnType<typeof core>["fetchMock"]) {
  vi.stubGlobal("fetch", fetchMock);
  const router = createMemoryRouter(routes, { initialEntries: ["/settings"] });
  const queryClient = createQueryClient();
  queryClient.setDefaultOptions({ queries: { ...queryClient.getDefaultOptions().queries, retry: false } });
  render(<App router={router} queryClient={queryClient} />);
  return { queryClient };
}

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

const section = () => screen.findByRole("region", { name: "Integrations" });
const row = (name: string) => screen.getByText(name, { selector: "dt" }).closest(".pp-fact") as HTMLElement;
const SPOOLMAN_PATH = "/api/v2/integrations";
const SPOOLMAN_URL = "http://spoolman.test:7912";
const spoolman = view("s1", "spoolman", "Spools", { base_url: SPOOLMAN_URL, has_api_key: true });
const editSpoolman = async (name = "Spools") => {
  await screen.findByText(name, { selector: "dt" });
  fireEvent.click(within(row(name)).getByRole("button", { name: `Edit ${name}` }));
  return screen.getByRole("dialog", { name: "Edit Spoolman" });
};
const fill = (dialog: HTMLElement, label: string | RegExp, value: string) =>
  fireEvent.change(within(dialog).getByLabelText(label), { target: { value } });
const saveSpoolman = (dialog: HTMLElement) =>
  fireEvent.click(within(dialog).getByRole("button", { name: "Save Spoolman" }));

describe("Spoolman settings", () => {
  it("adds a Spoolman connection and removes it only after confirmation", async () => {
    const writes: { method: string; body: unknown; path: string }[] = [];
    let integrations: (typeof spoolman)[] = [];
    renderSettings(core({
      list: () => json({ integrations }),
      other: (path, init) => {
        if (!init?.method || !path.startsWith(SPOOLMAN_PATH)) return;
        writes.push({ method: init.method, path, body: init.body ? JSON.parse(String(init.body)) : undefined });
        integrations = init.method === "POST" ? [spoolman] : [];
        return json(spoolman);
      },
    }).fetchMock);
    await screen.findByText("No integrations yet.");
    fireEvent.click(screen.getByRole("button", { name: "Add Spoolman" }));
    const add = screen.getByRole("dialog", { name: "Add Spoolman" });
    fill(add, "Name", spoolman.name);
    fill(add, "Base URL", SPOOLMAN_URL);
    fill(add, /API key/, SECRET);
    saveSpoolman(add);
    await screen.findByText("Added Spools.");
    expect(within(row("Spools")).getByText("API key saved")).toBeTruthy();
    expect(document.body.textContent).not.toContain(SECRET);
    const open = () => fireEvent.click(within(row("Spools")).getByRole("button", { name: "Remove Spools" }));
    open();
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(writes).toHaveLength(1);
    open();
    fireEvent.click(within(screen.getByRole("dialog", { name: "Remove Spools?" })).getByRole("button", { name: "Remove Spoolman" }));
    await screen.findByText("Removed Spools.");
    expect(writes).toEqual([
      { method: "POST", path: SPOOLMAN_PATH, body: { kind: "spoolman", name: "Spools", base_url: SPOOLMAN_URL, api_key: SECRET } },
      { method: "DELETE", path: `${SPOOLMAN_PATH}/s1`, body: undefined },
    ]);
    expect(screen.getByText("No integrations yet.")).toBeTruthy();
  });

  it("guards edits, keeps the omitted key, and makes an address change carry a replacement or null", async () => {
    const patches: unknown[] = [];
    let stored = { ...spoolman, base_url: `${SPOOLMAN_URL}/?key=${SECRET}` };
    renderSettings(core({
      list: () => json({ integrations: [stored] }),
      other: (path, init) => {
        if (path !== `${SPOOLMAN_PATH}/s1` || init?.method !== "PATCH") return;
        const patch = JSON.parse(String(init.body));
        const { name, base_url: baseUrl, api_key: apiKey } = patch;
        patches.push(patch);
        stored = {
          ...stored, name: name ?? stored.name, base_url: baseUrl ?? stored.base_url,
          has_api_key: apiKey === undefined ? stored.has_api_key : apiKey !== null, revision: `v${patches.length}`,
        };
        return json(stored);
      },
    }).fetchMock);

    const edit = await editSpoolman();
    expect((within(edit).getByLabelText(/API key \(optional\)/) as HTMLInputElement).value).toBe("");
    expect((within(edit).getByLabelText("Base URL") as HTMLInputElement).value).toBe(`${SPOOLMAN_URL}/`);
    fill(edit, "Name", "Inventory");
    saveSpoolman(edit);
    await screen.findByText("Saved Inventory.");
    expect(patches[0]).toStrictEqual({ expected_revision: "fixture-revision", name: "Inventory" });

    const move = await editSpoolman("Inventory");
    const replacement = "http://new-spoolman.test:7912";
    fill(move, "Base URL", replacement);
    const key = within(move).getByLabelText("API key") as HTMLInputElement;
    expect(key.required).toBe(true);
    saveSpoolman(move);
    expect(patches).toHaveLength(1);
    fill(move, "API key", SECRET);
    saveSpoolman(move);
    await screen.findByText("Saved Inventory.");
    expect(patches[1]).toEqual({ expected_revision: "v1", base_url: replacement, api_key: SECRET });

    const clear = await editSpoolman("Inventory");
    fill(clear, "Base URL", SPOOLMAN_URL);
    fireEvent.click(within(clear).getByLabelText("Clear saved API key"));
    saveSpoolman(clear);
    await screen.findByText("Saved Inventory.");
    expect(patches[2]).toEqual({ expected_revision: "v2", base_url: SPOOLMAN_URL, api_key: null });
    expect(within(row("Inventory")).queryByText("API key saved")).toBeNull();
  });

  it("shows a concurrent edit and requires reopening the form before writing with the new revision", async () => {
    let stored = spoolman;
    const patches: unknown[] = [];
    renderSettings(core({
      list: () => json({ integrations: [stored] }),
      other: (path, init) => {
        if (path !== `${SPOOLMAN_PATH}/s1` || init?.method !== "PATCH") return;
        patches.push(JSON.parse(String(init.body)));
        if (patches.length === 1) return problem(409, "The integration changed", "integration_changed");
        stored = { ...stored, name: "My inventory", revision: "v3" };
        return json(stored);
      },
    }).fetchMock);
    const edit = await editSpoolman();
    fill(edit, "Name", "Stale name");
    fill(edit, /API key \(optional\)/, SECRET);
    stored = { ...spoolman, name: "Latest inventory", revision: "v2" };
    saveSpoolman(edit);
    await screen.findByText(/This integration changed while you were editing it/);
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(patches).toEqual([{ expected_revision: "fixture-revision", name: "Stale name", api_key: SECRET }]);

    const latest = await editSpoolman("Latest inventory");
    expect((within(latest).getByLabelText("Name") as HTMLInputElement).value).toBe("Latest inventory");
    expect((within(latest).getByLabelText(/API key \(optional\)/) as HTMLInputElement).value).toBe("");
    fill(latest, "Name", "My inventory");
    saveSpoolman(latest);
    await screen.findByText("Saved My inventory.");
    expect(patches[1]).toEqual({ expected_revision: "v2", name: "My inventory" });
  });

  it("retries only the list read after an unconfirmed add and a failed refresh", async () => {
    let writes = 0;
    let readsAfterWrite = 0;
    renderSettings(core({
      list: () => {
        if (!writes) return json({ integrations: [] });
        if (++readsAfterWrite === 1) return problem(503, "Unavailable");
        return json({ integrations: [{ ...spoolman, has_api_key: false }] });
      },
      other: (path, init) => {
        if (path !== SPOOLMAN_PATH || init?.method !== "POST") return;
        writes++;
        return json({ unreadable: true });
      },
    }).fetchMock);
    await screen.findByText("No integrations yet.");
    fireEvent.click(screen.getByRole("button", { name: "Add Spoolman" }));
    const add = screen.getByRole("dialog", { name: "Add Spoolman" });
    fill(add, "Name", "Spools");
    fill(add, "Base URL", SPOOLMAN_URL);
    saveSpoolman(add);
    await within(add).findByText(/The current integrations couldn't be loaded/);
    expect(within(add).queryByRole("button", { name: "Save Spoolman" })).toBeNull();
    fireEvent.click(within(add).getByRole("button", { name: "Reload integrations" }));
    await screen.findByText(/Print Partner couldn't confirm the change. The list is up to date/);
    expect(row("Spools")).toBeTruthy();
    expect(writes).toBe(1);
    expect(readsAfterWrite).toBe(2);
  });
});

describe("SET-02 integration settings never display secrets", () => {
  it("lists every integration with its kind, address and which credentials are set", async () => {
    renderSettings(core().fetchMock);
    const region = await section();
    await within(region).findByText("Voron", { selector: "dt" });
    expect(within(region).getAllByRole("term").map((dt) => dt.textContent)).toEqual(["Voron", "MK4", "Spools", "X1C"]);
    expect(row("Voron").textContent).toContain("Moonraker, on");
    expect(within(row("Voron")).getByText("http://192.168.1.5:7125/")).toBeTruthy();
    expect(within(row("Voron")).getByText("API key saved")).toBeTruthy();
    expect(within(row("MK4")).getByText("User maker, Password saved")).toBeTruthy();
    expect(row("Spools").textContent).toContain("Spoolman, off");
    expect(within(row("Spools")).getByText(/Spoolman alone owns filament counts\. Stock it cannot read shows as unknown/)).toBeTruthy();
    // Even a core that answered a secret by mistake never gets it on screen.
    expect(document.body.textContent).not.toContain(SECRET);
  });

  it("never shows sign-in details stored in an address, and nothing of one that is not a URL", async () => {
    const list = [
      view("m1", "moonraker", "Voron", { base_url: "http://u:secret@printer.local" }),
      view("m2", "moonraker", "Micron", { base_url: "http://micron.local:7125/?api_key=query-secret#frag" }),
      view("m3", "moonraker", "Old", { base_url: "not a url secret" }),
    ];
    renderSettings(core({ list: () => json({ integrations: list }) }).fetchMock);
    await screen.findByText("Voron", { selector: "dt" });
    expect(within(row("Voron")).getByText("http://printer.local/")).toBeTruthy();
    expect(within(row("Voron")).getByText("Sign-in details stored in this address are hidden.")).toBeTruthy();
    expect(within(row("Micron")).getByText("http://micron.local:7125/")).toBeTruthy();
    expect(within(row("Old")).getByText("The stored address can't be shown.")).toBeTruthy();
    expect(document.body.textContent).not.toContain("secret");
  });

  it("says when there are no integrations", async () => {
    renderSettings(core({ list: () => json({ integrations: [] }) }).fetchMock);
    expect(await within(await section()).findByText("No integrations yet.")).toBeTruthy();
  });
});

describe("the list follows the Printers section", () => {
  it("reads the integrations again after a printer and its connection are deleted", async () => {
    let deleted = false;
    const voron = {
      id: "printer-1",
      name: "Voron",
      model: "Voron 2.4",
      integration: { id: "m1", kind: "moonraker" },
      state: { execution: "idle", bed_clear: "clear", display: "ready" },
      reason: null,
      last_heard_at: null,
      progress_percent: null,
      loaded_filaments: [],
    };
    const config = { id: "printer-1", name: "Voron", model: "Voron 2.4", bed_width_mm: 350, bed_depth_mm: 350, bed_height_mm: 340, margin_mm: 4, max_filament_slots: 1, integration_id: "m1" };
    const fake = core({
      list: () => json(deleted ? { integrations: LIST.integrations.slice(1) } : LIST),
      other: (path, init) => {
        if (path === "/api/v2/printers") return json({ printers: deleted ? [] : [voron] });
        if (init?.method === "DELETE" && path === "/api/v2/printers/printer-1") return json(config);
        if (init?.method === "DELETE" && path === "/api/v2/integrations/m1") return (deleted = true), json(LIST.integrations[0]);
        return undefined;
      },
    });
    renderSettings(fake.fetchMock);
    const integrations = await section();
    await within(integrations).findByText("Voron", { selector: "dt" });
    const printers = screen.getByRole("region", { name: "Printers" });
    fireEvent.click(await within(printers).findByRole("button", { name: "Delete Voron" }));
    fireEvent.click(within(await screen.findByRole("dialog")).getByRole("button", { name: "Delete printer" }));
    await waitFor(() => expect(within(integrations).queryByText("Voron", { selector: "dt" })).toBeNull());
    expect(within(integrations).getByText("MK4", { selector: "dt" })).toBeTruthy();
  });
});

describe("Bambu certificate pin", () => {
  it("shows the pin, asks first with focus on Cancel, and resets only from the confirm button", async () => {
    let pinned = true;
    const fake = core({ pin: () => json(pinned ? PINNED : { pinned: false }), reset: () => ((pinned = false), json({ reset: true })) });
    renderSettings(fake.fetchMock);
    await section();
    expect(await within(await screen.findByText(/^Certificate pinned · ab12cd34ef56 · /)).findByRole("button", { name: "Reset certificate" })).toBeTruthy();
    // No pin is read for a row that is not Bambu.
    expect(fake.fetchMock.mock.calls.filter(([path]) => path.includes("/tls-pin"))).toHaveLength(1);

    fireEvent.click(within(row("X1C")).getByRole("button", { name: "Reset certificate" }));
    const dialog = await screen.findByRole("dialog", { name: "Reset the certificate for X1C?" });
    expect(within(dialog).getByText(/sure the printer on your network is this same device/)).toBeTruthy();
    await waitFor(() => expect(document.activeElement).toBe(within(dialog).getByRole("button", { name: "Cancel" })));
    fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(fake.resets()).toHaveLength(0);

    fireEvent.click(within(row("X1C")).getByRole("button", { name: "Reset certificate" }));
    fireEvent.click(within(await screen.findByRole("dialog")).getByRole("button", { name: "Reset certificate" }));
    expect(await screen.findByText("Not pinned. Test connection to pin.")).toBeTruthy();
    expect(within(await section()).getByText(/^Certificate reset for X1C\. The next Test connection pins/)).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Reset certificate" })).toBeNull();
    expect(fake.resets()).toHaveLength(1);
  });

  it("keeps a refused reset in the dialog and does not send it again", async () => {
    const fake = core({ reset: () => problem(429, "Rate limit exceeded, retry in 30") });
    renderSettings(fake.fetchMock);
    await screen.findByText(/^Certificate pinned/);
    fireEvent.click(within(row("X1C")).getByRole("button", { name: "Reset certificate" }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.click(within(dialog).getByRole("button", { name: "Reset certificate" }));
    expect((await within(dialog).findByRole("alert")).textContent).toBe(
      "The certificate was not reset: Rate limit exceeded, retry in 30.",
    );
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(fake.resets()).toHaveLength(1);
  });

  it("after a lost reply, sends no second reset and re-reads the pin before Reset works again", async () => {
    let pinned = true;
    let held: ((answer: Response) => void) | null = null;
    const fake = core({
      // The first read answers; the read after the lost reply waits until the test answers it.
      pin: () => (fake && fake.resets().length === 0 ? json(PINNED) : (new Promise<Response>((resolve) => (held = resolve)) as never)),
      reset: () => ((pinned = false), Promise.reject(new TypeError("Failed to fetch")) as never),
    });
    renderSettings(fake.fetchMock);
    await screen.findByText(/^Certificate pinned/);
    fireEvent.click(within(row("X1C")).getByRole("button", { name: "Reset certificate" }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.click(within(dialog).getByRole("button", { name: "Reset certificate" }));
    expect((await within(dialog).findByRole("alert")).textContent).toBe("The core didn't confirm the reset. Checking the current pin.");
    await waitFor(() => expect(held).not.toBeNull());
    const again = within(dialog).getByRole("button", { name: "Reset certificate" });
    expect(again.getAttribute("aria-disabled")).toBe("true");
    fireEvent.click(again);
    expect(fake.resets()).toHaveLength(1);
    held!(json(pinned ? PINNED : { pinned: false }));
    expect(await within(dialog).findByText("The core didn't confirm the reset. The certificate is no longer pinned.")).toBeTruthy();
    expect(within(dialog).queryByRole("button", { name: "Reset certificate" })).toBeNull();
    expect(within(row("X1C")).getByText("Not pinned. Test connection to pin.", { exact: false })).toBeTruthy();
    expect(fake.resets()).toHaveLength(1);
  });

  it("ends correct when the dialog is cancelled mid-reset and the core applies it but the answer is lost", async () => {
    let pinned = true;
    let lose: (() => void) | null = null;
    const fake = core({
      pin: () => json(pinned ? PINNED : { pinned: false }),
      reset: () => new Promise<Response>((_, reject) => (lose = () => reject(new TypeError("Failed to fetch")))) as never,
    });
    renderSettings(fake.fetchMock);
    await screen.findByText(/^Certificate pinned/);
    fireEvent.click(within(row("X1C")).getByRole("button", { name: "Reset certificate" }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.click(within(dialog).getByRole("button", { name: "Reset certificate" }));
    await waitFor(() => expect(lose).not.toBeNull());
    fireEvent.click(within(dialog).getByRole("button", { name: "Close" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    pinned = false;
    lose!();
    expect(await within(row("X1C")).findByText(/^Not pinned\. Test connection to pin\./)).toBeTruthy();
    expect(within(row("X1C")).getByRole("alert").textContent).toBe("The core didn't confirm the reset. The certificate is no longer pinned.");
    expect(within(row("X1C")).queryByRole("button", { name: "Reset certificate" })).toBeNull();
    expect(fake.resets()).toHaveLength(1);
  });

  it("never shows the old pin after a confirmed reset whose re-read fails", async () => {
    let reads = 0;
    const fake = core({ pin: () => (++reads === 1 ? json(PINNED) : problem(503, "The core is busy")) });
    renderSettings(fake.fetchMock);
    await screen.findByText(/^Certificate pinned/);
    fireEvent.click(within(row("X1C")).getByRole("button", { name: "Reset certificate" }));
    fireEvent.click(within(await screen.findByRole("dialog")).getByRole("button", { name: "Reset certificate" }));
    expect(await within(row("X1C")).findByText(/^Certificate state unavailable: The core is busy\./)).toBeTruthy();
    expect(within(row("X1C")).queryByText(/Certificate pinned/)).toBeNull();
    expect(within(row("X1C")).queryByRole("button", { name: "Reset certificate" })).toBeNull();
    expect(fake.resets()).toHaveLength(1);
  });

  it("says when the pin cannot be read", async () => {
    renderSettings(core({ pin: () => problem(503, "The core is busy") }).fetchMock);
    expect(await screen.findByText("Certificate state unavailable: The core is busy.")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Reset certificate" })).toBeNull();
  });

  it("after a redirected reset, refuses a redirected pin read and never says the certificate is no longer pinned", async () => {
    const servers = await redirectServers({ pinned: false });
    try {
      servers.redirectWith(307);
      let pinReads = 0;
      renderSettings(core({
        pin: (init) => (++pinReads === 1 ? json(PINNED) : servers.forward("/api/v1/integrations/b1/tls-pin", init)),
        reset: (init) => servers.forward("/api/v1/integrations/b1/tls-pin/reset", init),
      }).fetchMock);
      await screen.findByText(/^Certificate pinned/);
      fireEvent.click(within(row("X1C")).getByRole("button", { name: "Reset certificate" }));
      const dialog = await screen.findByRole("dialog");
      fireEvent.click(within(dialog).getByRole("button", { name: "Reset certificate" }));
      expect((await within(dialog).findByRole("alert")).textContent).toBe("The core didn't confirm the reset. Checking the current pin.");
      expect(await within(row("X1C")).findByText(/^Certificate state unavailable: /)).toBeTruthy();
      expect(dialog.textContent).not.toContain("no longer pinned");
      expect(servers.first).toEqual(["POST /api/v1/integrations/b1/tls-pin/reset", "GET /api/v1/integrations/b1/tls-pin"]);
      expect(servers.destination).toEqual([]);
    } finally { await servers.close(); }
  });
});

describe("SET-05 Integrations section states", () => {
  it("shows loading until the list answers", async () => {
    renderSettings(core({ list: () => new Promise<Response>(() => {}) as never }).fetchMock);
    expect(await within(await section()).findByText("Loading integrations.")).toBeTruthy();
  });

  it("shows the core's reason with Reload when the list cannot be read, and reads again", async () => {
    let fail = true;
    const fake = core({ list: () => (fail ? problem(500, "Integrations are unreadable") : json(LIST)) });
    renderSettings(fake.fetchMock);
    const alert = await within(await section()).findByRole("alert");
    expect(alert.textContent).toContain("Could not load the integrations: Integrations are unreadable");
    fail = false;
    fireEvent.click(within(alert).getByRole("button", { name: "Reload integrations" }));
    expect(await screen.findByText("Voron", { selector: "dt" })).toBeTruthy();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("keeps the list offline, and Reset certificate does nothing", async () => {
    const fake = core();
    const { queryClient } = renderSettings(fake.fetchMock);
    await screen.findByText(/^Certificate pinned/);
    fake.goDown();
    await queryClient.refetchQueries({ queryKey: ["core", "health"] });
    await waitFor(() => expect(document.querySelector("[data-state=offline]")).not.toBeNull());
    const region = await section();
    expect(within(region).getByText(/^Offline: Print Partner cannot reach its core/)).toBeTruthy();
    const reset = within(row("X1C")).getByRole("button", { name: "Reset certificate" });
    expect(reset.getAttribute("aria-disabled")).toBe("true");
    fireEvent.click(reset);
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(fake.resets()).toHaveLength(0);
  });
});
