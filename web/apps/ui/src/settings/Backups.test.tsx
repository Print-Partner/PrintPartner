import type { Health } from "@print-partner/rust-client";
import { act, cleanup, fireEvent, render, waitFor, within } from "@testing-library/react";
import { createMemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App, createQueryClient } from "../App";
import { routes } from "../routes";

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
/** The core's own problem document, as `pp-api` sends it. */
const problem = (status: number, detail: string, extra: Record<string, unknown> = {}) =>
  new Response(
    JSON.stringify({ type: "urn:print-partner:problem:overloaded", title: "Service Unavailable", status, detail, ...extra }),
    { status, headers: { "content-type": "application/problem+json" } },
  );

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

const SHA = "9f".repeat(32);
const BACKUP = {
  name: "print-partner-backup.tar.gz",
  created_at: "2026-11-17T23:15:00Z",
  app_version: "0.42.0",
  size_bytes: 1_234_567,
  sha256: SHA,
  warnings: [],
};

const CREATE = "/api/v2/installation/backup";
type Answer = (init?: RequestInit) => Response | Promise<Response>;

/** A fake core: /health and empty Printers and Integrations; the create route as given. */
function core(create?: Answer) {
  let down = false;
  const fetchMock = vi.fn((path: string, init?: RequestInit) =>
    Promise.resolve().then((): Response | Promise<Response> => {
      if (down) throw new TypeError("Failed to fetch");
      if (path === "/health") return json(HEALTH);
      if (path === "/api/v2/printers") return json({ printers: [] });
      if (path === "/api/v2/integrations") return json({ integrations: [] });
      if (path === CREATE && init?.method === "POST" && create) return create(init);
      return problem(404, `unexpected ${path}`);
    }),
  );
  const creates = () => fetchMock.mock.calls.filter(([called]) => called === CREATE);
  return { fetchMock, creates, goDown: () => (down = true) };
}

function renderSettings(fetchMock: ReturnType<typeof core>["fetchMock"], container?: HTMLElement) {
  vi.stubGlobal("fetch", fetchMock);
  const router = createMemoryRouter(routes, { initialEntries: ["/settings"] });
  const queryClient = createQueryClient();
  queryClient.setDefaultOptions({ queries: { ...queryClient.getDefaultOptions().queries, retry: false } });
  render(<App router={router} queryClient={queryClient} />, container ? { container } : undefined);
  return { queryClient };
}

const backups = async (root: HTMLElement = document.body) =>
  within(root).findByRole("region", { name: "Backups" });

/** Resolves when the shared /health probe says online, so changes are on. */
async function online(region: HTMLElement) {
  await waitFor(() =>
    expect(within(region).getByRole("button", { name: "Create backup" }).getAttribute("aria-disabled")).toBeNull(),
  );
}

function deferred() {
  let resolve!: (response: Response) => void;
  const promise = new Promise<Response>((done) => (resolve = done));
  return { promise, resolve };
}

const LOST = "The core did not confirm the backup. Create backup again.";

/** Creates once with `json(BACKUP)`, then answers each later create with `then`. */
async function shownThen(then: Answer) {
  let made = false;
  const fake = core((init) => (made ? then(init) : ((made = true), json(BACKUP))));
  renderSettings(fake.fetchMock);
  const region = await backups();
  await online(region);
  fireEvent.click(within(region).getByRole("button", { name: "Create backup" }));
  expect(await within(region).findByText(SHA)).toBeTruthy();
  fireEvent.click(within(region).getByRole("button", { name: "Create backup" }));
  return { fake, region, alert: (await within(region).findByRole("alert")).textContent };
}

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("Backups: create", () => {
  it("replaces the placeholder with Create backup and sends nothing when the section opens", async () => {
    const fake = core();
    renderSettings(fake.fetchMock);
    const region = await backups();
    expect(within(region).queryByText("Not available yet.")).toBeNull();
    expect(within(region).getByRole("button", { name: "Create backup" })).toBeTruthy();
    expect(within(region).getByText(/the database and the files of this whole installation/)).toBeTruthy();
    expect(new Set(fake.fetchMock.mock.calls.map(([path]) => path))).toEqual(
      new Set(["/health", "/api/v2/printers", "/api/v2/integrations"]),
    );
  });

  it("creates one backup and shows its name, time, version, size and SHA-256", async () => {
    const answer = deferred();
    const fake = core(() => answer.promise);
    renderSettings(fake.fetchMock);
    const region = await backups();
    await online(region);
    const button = within(region).getByRole("button", { name: "Create backup" });
    fireEvent.click(button);
    expect(await within(region).findByText("Creating backup")).toBeTruthy();
    expect(button.getAttribute("aria-disabled")).toBe("true");
    fireEvent.click(button);
    await act(async () => answer.resolve(json(BACKUP)));
    const facts = await within(region).findByLabelText("Backup made");
    expect(within(facts).getByText(BACKUP.name)).toBeTruthy();
    expect(within(facts).getByText("1.2 MiB (1,234,567 bytes)")).toBeTruthy();
    expect(within(facts).getByText(SHA)).toBeTruthy();
    expect(within(facts).getByText(/app version 0\.42\.0/)).toBeTruthy();
    expect(within(region).queryByText("Creating backup")).toBeNull();
    expect(fake.creates()).toHaveLength(1);
    expect(fake.creates()[0]?.[1]?.body).toBeUndefined();
  });

  it("after a page reload the section shows no backup and sends no create", async () => {
    const fake = core(() => json(BACKUP));
    renderSettings(fake.fetchMock);
    const region = await backups();
    await online(region);
    fireEvent.click(within(region).getByRole("button", { name: "Create backup" }));
    expect(await within(region).findByText(SHA)).toBeTruthy();
    cleanup();
    renderSettings(fake.fetchMock);
    const reloaded = await backups();
    await online(reloaded);
    expect(within(reloaded).queryByLabelText("Backup made")).toBeNull();
    expect(within(reloaded).getByText(/After a reload, create one again/)).toBeTruthy();
    expect(fake.creates()).toHaveLength(1);
  });

  it("says when the backup is large", async () => {
    const fake = core(() => json({ ...BACKUP, warnings: ["large"] }));
    renderSettings(fake.fetchMock);
    const region = await backups();
    await online(region);
    fireEvent.click(within(region).getByRole("button", { name: "Create backup" }));
    expect(await within(region).findByText(/This backup is large/)).toBeTruthy();
  });

  it("in a second tab, a busy core says a backup is already running and the first tab keeps its backup", async () => {
    let running = false;
    const first = deferred();
    const fake = core(() => {
      if (running) return problem(503, "A backup is already running");
      running = true;
      return first.promise.finally(() => (running = false));
    });
    const tabOne = document.body.appendChild(document.createElement("div"));
    const tabTwo = document.body.appendChild(document.createElement("div"));
    renderSettings(fake.fetchMock, tabOne);
    renderSettings(fake.fetchMock, tabTwo);
    const one = await backups(tabOne);
    const two = await backups(tabTwo);
    await online(one);
    await online(two);
    fireEvent.click(within(one).getByRole("button", { name: "Create backup" }));
    await within(one).findByText("Creating backup");
    fireEvent.click(within(two).getByRole("button", { name: "Create backup" }));
    expect((await within(two).findByRole("alert")).textContent).toBe(
      "The backup was not made: A backup is already running.",
    );
    await act(async () => first.resolve(json(BACKUP)));
    expect(await within(one).findByText(SHA)).toBeTruthy();
    expect(within(two).queryByText(SHA)).toBeNull();
  });

  it("the core's own refusal after a backup was shown keeps it", async () => {
    const { region, alert } = await shownThen(() => problem(503, "Not enough free disk space"));
    expect(alert).toBe("The backup was not made: Not enough free disk space.");
    expect(within(region).getByText(SHA)).toBeTruthy();
  });

  it("offline: Create backup is off with the offline reason and sends nothing", async () => {
    const fake = core(() => json(BACKUP));
    const { queryClient } = renderSettings(fake.fetchMock);
    const region = await backups();
    await online(region);
    fake.goDown();
    await queryClient.refetchQueries({ queryKey: ["core", "health"] });
    const button = within(region).getByRole("button", { name: "Create backup" });
    await waitFor(() => expect(button.getAttribute("aria-disabled")).toBe("true"));
    const described = document.getElementById(button.getAttribute("aria-describedby") ?? "");
    expect(described?.textContent).toMatch(/^Offline: Print Partner cannot reach its core/);
    fireEvent.click(button);
    expect(fake.creates()).toHaveLength(0);
  });

  it("a lost create answer shows no backup and says to create again", async () => {
    const fake = core(() => Promise.reject(new TypeError("Failed to fetch")));
    renderSettings(fake.fetchMock);
    const region = await backups();
    await online(region);
    fireEvent.click(within(region).getByRole("button", { name: "Create backup" }));
    expect((await within(region).findByRole("alert")).textContent).toBe(LOST);
    expect(within(region).queryByLabelText("Backup made")).toBeNull();
  });

  it.each([
    ["a 300", () => new Response(null, { status: 300 })],
    ["a 502 proxy page", () => new Response("<html>Bad Gateway</html>", { status: 502, headers: { "content-type": "text/html" } })],
    ["a 200 that fails the contract", () => json({ name: BACKUP.name })],
    ["a problem whose code is null", () => problem(503, "A backup is already running", { code: null })],
    ["a problem whose code is a number", () => problem(503, "A backup is already running", { code: 503 })],
    ["a problem whose code is an object", () => problem(503, "A backup is already running", { code: {} })],
    ["a problem whose status disagrees", () => problem(503, "A backup is already running", { status: 500 })],
  ])("%s after a backup was shown drops it and says the core did not confirm", async (_, answer) => {
    const { fake, region, alert } = await shownThen(answer);
    expect(alert).toBe(LOST);
    expect(within(region).queryByText(SHA)).toBeNull();
    expect(fake.creates()).toHaveLength(2);
  });
});
