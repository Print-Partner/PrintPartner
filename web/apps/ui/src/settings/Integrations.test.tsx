import type { Health } from "@print-partner/rust-client";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { createMemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App, createQueryClient } from "../App";
import { routes } from "../routes";

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
const problem = (status: number, detail: string) =>
  new Response(JSON.stringify({ type: "about:blank", title: "Refused", status, detail }), {
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

/** A fake core: /health and the list as given, the Bambu pin from `pin`; records every call. */
function core({ list = (): Response => json(LIST), pin = (): Response => json(PINNED), reset = (): Response => json({ reset: true }) } = {}) {
  let down = false;
  const answer: Answer = (path, init) => {
    if (down) return Promise.reject(new TypeError("Failed to fetch"));
    if (path === "/health") return json(HEALTH);
    if (path === "/api/v2/integrations") return list();
    if (path === "/api/v1/integrations/b1/tls-pin") return pin();
    if (path === "/api/v1/integrations/b1/tls-pin/reset" && init?.method === "POST") return reset();
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

  it("says when there are no integrations", async () => {
    renderSettings(core({ list: () => json({ integrations: [] }) }).fetchMock);
    expect(await within(await section()).findByText("No integrations yet.")).toBeTruthy();
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

  it("says when the pin cannot be read", async () => {
    renderSettings(core({ pin: () => problem(503, "The core is busy") }).fetchMock);
    expect(await screen.findByText("Certificate state unavailable: The core is busy.")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Reset certificate" })).toBeNull();
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
