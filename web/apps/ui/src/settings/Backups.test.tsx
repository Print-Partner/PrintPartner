import type { Health } from "@print-partner/rust-client";
import { createServer, type Server } from "node:http";
import type { AddressInfo } from "node:net";
import { act, cleanup, fireEvent, render, waitFor, within } from "@testing-library/react";
import { createMemoryRouter } from "react-router";
import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from "vitest";
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
const ARCHIVE = `/api/v2/installation/backup/archive?sha256=${SHA}`;
const STAGE = "/api/v2/installation/restore/stage";
const RESTORE = "/api/v2/installation/restore";
type Answer = (init?: RequestInit) => Response | Promise<Response>;

/**
 * Every request any test in this file made to the restore route. This slice never calls it, so
 * `afterEach` fails the test that made one.
 */
const restores: string[] = [];

/**
 * A fake core: /health and empty Printers and Integrations; the create route, `HEAD` on the
 * shown backup's archive URL and the stage route as given.
 */
function core(create?: Answer, more: { head?: Answer; stage?: Answer } = {}) {
  let down = false;
  const fetchMock = vi.fn((path: string, init?: RequestInit) =>
    Promise.resolve().then((): Response | Promise<Response> => {
      if (new URL(path, "http://core").pathname === RESTORE) restores.push(`${init?.method ?? "GET"} ${path}`);
      if (down) throw new TypeError("Failed to fetch");
      if (path === "/health") return json(HEALTH);
      if (path === "/api/v2/printers") return json({ printers: [] });
      if (path === "/api/v2/integrations") return json({ integrations: [] });
      if (path === CREATE && init?.method === "POST" && create) return create(init);
      if (path === ARCHIVE && init?.method === "HEAD" && more.head) return more.head(init);
      if (path === STAGE && init?.method === "POST" && more.stage) return more.stage(init);
      return problem(404, `unexpected ${path}`);
    }),
  );
  const calls = (path: string) => fetchMock.mock.calls.filter(([called]) => called === path);
  return { fetchMock, creates: () => calls(CREATE), heads: () => calls(ARCHIVE), stages: () => calls(STAGE), goDown: () => (down = true) };
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
  const made = restores.splice(0);
  expect(made, "this slice must never call the restore route").toEqual([]);
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
    const [status] = within(region).getAllByRole("status");
    expect(status?.textContent).toBe("");
    fireEvent.click(button);
    await waitFor(() => expect(status?.textContent).toBe("Creating backup"));
    expect(button.getAttribute("aria-disabled")).toBe("true");
    fireEvent.click(button);
    await act(async () => answer.resolve(json(BACKUP)));
    const facts = await within(region).findByLabelText("Backup made");
    expect(within(facts).getByText(BACKUP.name)).toBeTruthy();
    expect(within(facts).getByText("1.2 MiB (1,234,567 bytes)")).toBeTruthy();
    expect(within(facts).getByText(SHA)).toBeTruthy();
    expect(within(facts).getByText(/app version 0\.42\.0/)).toBeTruthy();
    expect(within(region).getAllByRole("status")[0]).toBe(status);
    expect(status?.textContent).toBe("Backup made");
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

/**
 * A real loopback HTTP server for the create route, so Node's own fetch decides whether to follow
 * a redirect. The fake `fetch` below forwards only the create POST to it and counts nothing
 * itself: every count is a request this server received.
 */
describe("Backups: a redirected create", () => {
  const PROBLEM_503 = {
    type: "urn:print-partner:problem:overloaded",
    title: "Service Unavailable",
    status: 503,
    detail: "A backup is already running",
  };
  const received: string[] = [];
  let redirect: { status: 307 | 308; to: "/ok" | "/problem" } = { status: 307, to: "/ok" };
  let server: Server;
  let origin = "";
  const nodeFetch = globalThis.fetch;

  beforeAll(async () => {
    server = createServer((request, response) => {
      received.push(`${request.method} ${request.url}`);
      request.resume();
      const send = (status: number, type: string, body: unknown, headers: Record<string, string> = {}) => {
        response.writeHead(status, { "content-type": type, ...headers });
        response.end(JSON.stringify(body));
      };
      const creates = received.filter((line) => line === `POST ${CREATE}`).length;
      if (request.url === CREATE && creates === 1) return send(200, "application/json", BACKUP);
      if (request.url === CREATE) return send(redirect.status, "text/plain", "", { location: redirect.to });
      if (request.url === "/ok") return send(200, "application/json", { ...BACKUP, sha256: "a1".repeat(32) });
      if (request.url === "/problem") return send(503, "application/problem+json", PROBLEM_503);
      send(404, "text/plain", "");
    });
    await new Promise<void>((done) => server.listen(0, "127.0.0.1", done));
    origin = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
  });
  afterAll(() => new Promise<void>((done) => server.close(() => done())));

  it.each([
    [307, "/ok"],
    [308, "/ok"],
    [307, "/problem"],
    [308, "/problem"],
  ] as const)("a %i to a destination that answers %s sends one POST and is not confirmed", async (status, to) => {
    received.length = 0;
    redirect = { status, to };
    const fake = core((init) => nodeFetch(`${origin}${CREATE}`, init));
    renderSettings(fake.fetchMock);
    const region = await backups();
    await online(region);
    fireEvent.click(within(region).getByRole("button", { name: "Create backup" }));
    expect(await within(region).findByText(SHA)).toBeTruthy();
    fireEvent.click(within(region).getByRole("button", { name: "Create backup" }));
    expect((await within(region).findByRole("alert")).textContent).toBe(LOST);
    expect(within(region).queryByLabelText("Backup made")).toBeNull();
    expect(received).toEqual([`POST ${CREATE}`, `POST ${CREATE}`]);
  });
});

/** Every anchor the page clicked, with its href and download attribute. */
function captureDownloads() {
  const clicked: { href: string | null; download: string | null }[] = [];
  vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(function (this: HTMLAnchorElement) {
    clicked.push({ href: this.getAttribute("href"), download: this.getAttribute("download") });
  });
  return clicked;
}

/** Renders Settings with `fake`, creates one backup and returns the section. */
async function created(fake: ReturnType<typeof core>) {
  renderSettings(fake.fetchMock);
  const region = await backups();
  await online(region);
  fireEvent.click(within(region).getByRole("button", { name: "Create backup" }));
  expect(await within(region).findByText(SHA)).toBeTruthy();
  return region;
}

const headOk = () => new Response(null, { status: 200, headers: { "content-length": String(BACKUP.size_bytes) } });

describe("Backups: download", () => {
  it("asks HEAD for the shown SHA-256 without following a redirect, then saves that URL under the file name", async () => {
    const clicked = captureDownloads();
    const fake = core(() => json(BACKUP), { head: headOk });
    const region = await created(fake);
    fireEvent.click(within(region).getByRole("button", { name: "Download" }));
    await waitFor(() => expect(clicked).toEqual([{ href: ARCHIVE, download: BACKUP.name }]));
    expect(fake.heads()).toHaveLength(1);
    expect(fake.heads()[0]?.[1]?.redirect).toBe("error");
    expect(within(region).getAllByRole("status")[0]?.textContent).toBe("Download started");
    expect(within(region).getByText(SHA)).toBeTruthy();
  });

  it.each([404, 409])("a %i on HEAD drops the shown backup, says to create again and saves nothing", async (status) => {
    const clicked = captureDownloads();
    const fake = core(() => json(BACKUP), { head: () => new Response(null, { status }) });
    const region = await created(fake);
    fireEvent.click(within(region).getByRole("button", { name: "Download" }));
    expect((await within(region).findByRole("alert")).textContent).toBe(
      "The core no longer keeps this backup; a newer one may have been made in another window. Create backup again.",
    );
    expect(within(region).queryByLabelText("Backup made")).toBeNull();
    expect(clicked).toEqual([]);
  });

  it.each([
    [503, "The core is busy with another backup or download. Try Download again in a moment."],
    [204, "The download did not start: the core answered 204. Try Download again."],
    [206, "The download did not start: the core answered 206. Try Download again."],
    [500, "The download did not start: the core answered 500. Try Download again."],
  ])("a %i on HEAD saves nothing, keeps the backup and says why", async (status, said) => {
    const clicked = captureDownloads();
    const fake = core(() => json(BACKUP), { head: () => new Response(null, { status }) });
    const region = await created(fake);
    fireEvent.click(within(region).getByRole("button", { name: "Download" }));
    expect((await within(region).findByRole("alert")).textContent).toBe(said);
    expect(within(region).getAllByRole("status")[0]?.textContent).toBe("");
    expect(within(region).getByText(SHA)).toBeTruthy();
    expect(clicked).toEqual([]);
  });
});

const PREVIEW = {
  restore_id: "0123456789abcdef0123456789abcdef",
  created_at: "2026-11-17T23:15:00Z",
  app_version: "0.41.3",
  schema_version: 34,
  confirmation: "Replace this installation",
};
const fileOf = (name = "print-partner-backup.tar.gz") => new File([new Uint8Array([31, 139, 8, 0])], name, { type: "application/gzip" });
const invalidArchive = () =>
  new Response(
    JSON.stringify({ type: "urn:print-partner:problem:validation", title: "Validation Failed", status: 422, detail: "Invalid backup archive" }),
    { status: 422, headers: { "content-type": "application/problem+json" } },
  );

/** Renders Settings with `fake` and returns the section once it is online. */
async function opened(fake: ReturnType<typeof core>) {
  renderSettings(fake.fetchMock);
  const region = await backups();
  await online(region);
  return region;
}

function choose(region: HTMLElement, file: File) {
  fireEvent.change(within(region).getByLabelText("Backup file"), { target: { files: [file] } });
}

const checkButton = (region: HTMLElement) => within(region).getByRole("button", { name: "Check backup" });
const NOT_CONFIRMED = "The core did not confirm the check. This installation is unchanged. Check backup again.";

describe("Backups: check a backup file", () => {
  it("sends nothing when a file is chosen, and Check backup is off until then", async () => {
    const fake = core(undefined, { stage: () => json(PREVIEW) });
    const region = await opened(fake);
    expect(within(region).getByRole("heading", { name: "Restore from a backup" })).toBeTruthy();
    expect(checkButton(region).getAttribute("aria-disabled")).toBe("true");
    fireEvent.click(checkButton(region));
    const before = fake.fetchMock.mock.calls.length;
    choose(region, fileOf());
    expect(checkButton(region).getAttribute("aria-disabled")).toBeNull();
    expect(fake.fetchMock.mock.calls.length).toBe(before);
    expect(fake.stages()).toHaveLength(0);
  });

  it("posts the file once, says Checking backup, then shows the backup's time, app version and schema version and no Restore button", async () => {
    const answer = deferred();
    const fake = core(undefined, { stage: () => answer.promise });
    const region = await opened(fake);
    const [, status] = within(region).getAllByRole("status");
    expect(status?.textContent).toBe("");
    const file = fileOf();
    choose(region, file);
    fireEvent.click(checkButton(region));
    await waitFor(() => expect(status?.textContent).toBe("Checking backup"));
    expect(checkButton(region).getAttribute("aria-disabled")).toBe("true");
    fireEvent.click(checkButton(region));
    await act(async () => answer.resolve(json(PREVIEW)));
    const facts = await within(region).findByLabelText("Backup checked");
    expect(within(facts).getByText("0.41.3")).toBeTruthy();
    expect(within(facts).getByText("34")).toBeTruthy();
    expect(within(facts).getByText(/2026/)).toBeTruthy();
    expect(within(region).getAllByRole("status")[1]).toBe(status);
    expect(status?.textContent).toBe("Backup checked");
    expect(fake.stages()).toHaveLength(1);
    const init = fake.stages()[0]?.[1];
    expect(init?.body).toBe(file);
    expect(init?.redirect).toBe("error");
    expect(new Headers(init?.headers).get("content-type")).toBe("application/gzip");
    expect(within(region).queryByRole("button", { name: /restore|replace/i })).toBeNull();
    expect(document.querySelector("dialog, [role=dialog]")).toBeNull();
  });

  it("the core's refusal of a truncated file shows its reason, says this installation is unchanged and drops the earlier check", async () => {
    let first = true;
    const fake = core(undefined, { stage: () => (first ? ((first = false), json(PREVIEW)) : invalidArchive()) });
    const region = await opened(fake);
    choose(region, fileOf());
    fireEvent.click(checkButton(region));
    await within(region).findByLabelText("Backup checked");
    choose(region, fileOf("truncated.tar.gz"));
    expect(within(region).queryByLabelText("Backup checked")).toBeNull();
    fireEvent.click(checkButton(region));
    expect((await within(region).findByRole("alert")).textContent).toBe(
      "The backup was not accepted: Invalid backup archive. This installation is unchanged.",
    );
    expect(within(region).queryByLabelText("Backup checked")).toBeNull();
    expect(document.querySelector("dialog, [role=dialog]")).toBeNull();
    expect(fake.stages()).toHaveLength(2);
  });

  it.each([
    ["a 502 proxy page", () => new Response("<html>Bad Gateway</html>", { status: 502, headers: { "content-type": "text/html" } })],
    ["a problem whose code is null", () => problem(422, "Invalid backup archive", { code: null })],
    ["a problem whose status disagrees", () => problem(422, "Invalid backup archive", { status: 400 })],
    ["a 200 that fails the contract", () => json({ app_version: "0.41.3" })],
    ["a lost answer", () => Promise.reject(new TypeError("Failed to fetch"))],
  ])("%s after a check drops the shown preview and is not read as the core's refusal", async (_, answer: Answer) => {
    let first = true;
    const fake = core(undefined, { stage: (init) => (first ? ((first = false), json(PREVIEW)) : answer(init)) });
    const region = await opened(fake);
    choose(region, fileOf());
    fireEvent.click(checkButton(region));
    await within(region).findByLabelText("Backup checked");
    fireEvent.click(checkButton(region));
    expect((await within(region).findByRole("alert")).textContent).toBe(NOT_CONFIRMED);
    expect(within(region).queryByLabelText("Backup checked")).toBeNull();
  });

  it("offline: Check backup is off with the offline reason and sends nothing", async () => {
    const fake = core(undefined, { stage: () => json(PREVIEW) });
    const { queryClient } = renderSettings(fake.fetchMock);
    const region = await backups();
    await online(region);
    choose(region, fileOf());
    fake.goDown();
    await queryClient.refetchQueries({ queryKey: ["core", "health"] });
    await waitFor(() => expect(checkButton(region).getAttribute("aria-disabled")).toBe("true"));
    const described = document.getElementById(checkButton(region).getAttribute("aria-describedby") ?? "");
    expect(described?.textContent).toMatch(/^Offline: Print Partner cannot reach its core/);
    fireEvent.click(checkButton(region));
    expect(fake.stages()).toHaveLength(0);
  });

  it("create, download, check and a refused check never call the restore route", async () => {
    captureDownloads();
    let first = true;
    const fake = core(() => json(BACKUP), {
      head: headOk,
      stage: () => (first ? ((first = false), json(PREVIEW)) : invalidArchive()),
    });
    const region = await created(fake);
    fireEvent.click(within(region).getByRole("button", { name: "Download" }));
    await waitFor(() => expect(fake.heads()).toHaveLength(1));
    choose(region, fileOf());
    fireEvent.click(checkButton(region));
    await within(region).findByLabelText("Backup checked");
    fireEvent.click(checkButton(region));
    await within(region).findByText(/was not accepted/);
    expect(fake.fetchMock.mock.calls.filter(([path]) => new URL(path, "http://core").pathname === RESTORE)).toEqual([]);
    expect(within(region).queryByRole("button", { name: /restore|replace/i })).toBeNull();
  });
});

/**
 * A real loopback server for HEAD on the archive and the stage POST, so Node's own fetch decides
 * whether to follow a redirect. Every count is a request this server received.
 */
describe("Backups: a redirected download check or backup check", () => {
  const received: string[] = [];
  let redirect: { status: 307 | 308; to: "/ok" | "/problem" } = { status: 307, to: "/ok" };
  let server: Server;
  let origin = "";
  const nodeFetch = globalThis.fetch;

  beforeAll(async () => {
    server = createServer((request, response) => {
      received.push(`${request.method} ${request.url}`);
      request.resume();
      const send = (status: number, type: string, body: unknown, headers: Record<string, string> = {}) => {
        response.writeHead(status, { "content-type": type, ...headers });
        response.end(request.method === "HEAD" ? undefined : JSON.stringify(body));
      };
      if (request.url === ARCHIVE || request.url === STAGE) return send(redirect.status, "text/plain", "", { location: redirect.to });
      if (request.url === "/ok") return send(200, "application/json", PREVIEW, { "content-length": "1" });
      if (request.url === "/problem")
        return send(422, "application/problem+json", { type: "urn:print-partner:problem:validation", title: "Validation Failed", status: 422, detail: "Invalid backup archive" });
      send(404, "text/plain", "");
    });
    await new Promise<void>((done) => server.listen(0, "127.0.0.1", done));
    origin = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
  });
  afterAll(() => new Promise<void>((done) => server.close(() => done())));

  const cases = [
    [307, "/ok"],
    [308, "/ok"],
    [307, "/problem"],
    [308, "/problem"],
  ] as const;

  it.each(cases)("Download: a %i to %s sends one HEAD, saves nothing and keeps the backup", async (status, to) => {
    received.length = 0;
    redirect = { status, to };
    const clicked = captureDownloads();
    const fake = core(() => json(BACKUP), { head: (init) => nodeFetch(`${origin}${ARCHIVE}`, init) });
    const region = await created(fake);
    fireEvent.click(within(region).getByRole("button", { name: "Download" }));
    expect((await within(region).findByRole("alert")).textContent).toBe(
      "The download did not start: the core did not answer. Try Download again.",
    );
    expect(clicked).toEqual([]);
    expect(received).toEqual([`HEAD ${ARCHIVE}`]);
  });

  it.each(cases)("Check backup: a %i to %s sends one POST and is not confirmed", async (status, to) => {
    received.length = 0;
    redirect = { status, to };
    const fake = core(undefined, { stage: (init) => nodeFetch(`${origin}${STAGE}`, { ...init, body: new Uint8Array([31, 139]) }) });
    const region = await opened(fake);
    choose(region, fileOf());
    fireEvent.click(checkButton(region));
    expect((await within(region).findByRole("alert")).textContent).toBe(NOT_CONFIRMED);
    expect(within(region).queryByLabelText("Backup checked")).toBeNull();
    expect(received).toEqual([`POST ${STAGE}`]);
  });
});
