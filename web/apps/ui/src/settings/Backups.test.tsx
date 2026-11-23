import type { Health } from "@print-partner/rust-client";
import { createServer, type Server } from "node:http";
import type { AddressInfo } from "node:net";
import { act, cleanup, fireEvent, render, waitFor, within } from "@testing-library/react";
import { createMemoryRouter } from "react-router";
import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { App, createQueryClient } from "../App";
import { routes } from "../routes";
import { LOST_RESTORE } from "./BackupsSection";
import { forgetRestore } from "./restoreOutcome";

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
 * A fake core: /health and empty Printers and Integrations; the create route, `HEAD` on the
 * shown backup's archive URL, the stage route and the restore route as given.
 */
function core(create?: Answer, more: { head?: Answer; stage?: Answer; restore?: Answer } = {}) {
  let down = false;
  const answers: Record<string, Answer | undefined> = {
    "GET /health": () => json(HEALTH),
    "GET /api/v2/printers": () => json({ printers: [] }),
    "GET /api/v2/integrations": () => json({ integrations: [] }),
    [`POST ${CREATE}`]: create,
    [`HEAD ${ARCHIVE}`]: more.head,
    [`POST ${STAGE}`]: more.stage,
    [`POST ${RESTORE}`]: more.restore,
  };
  const fetchMock = vi.fn((path: string, init?: RequestInit) =>
    Promise.resolve().then((): Response | Promise<Response> => {
      if (down) throw new TypeError("Failed to fetch");
      const answer = answers[`${init?.method ?? "GET"} ${path}`];
      return answer ? answer(init) : problem(404, `unexpected ${path}`);
    }),
  );
  const calls = (path: string) => fetchMock.mock.calls.filter(([called]) => called === path);
  const restores = () => fetchMock.mock.calls.filter(([called]) => new URL(called, "http://core").pathname === RESTORE);
  return { fetchMock, creates: () => calls(CREATE), heads: () => calls(ARCHIVE), stages: () => calls(STAGE), restores, goDown: () => (down = true) };
}

function renderSettings(fetchMock: ReturnType<typeof core>["fetchMock"], container?: HTMLElement) {
  vi.stubGlobal("fetch", fetchMock);
  const router = createMemoryRouter(routes, { initialEntries: ["/settings"] });
  const queryClient = createQueryClient();
  queryClient.setDefaultOptions({ queries: { ...queryClient.getDefaultOptions().queries, retry: false } });
  render(<App router={router} queryClient={queryClient} />, container ? { container } : undefined);
  return { queryClient, router };
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
  forgetRestore();
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

  it("is off while its HEAD is pending, so a second press sends no HEAD and saves nothing", async () => {
    const clicked = captureDownloads();
    const first = deferred();
    let heads = 0;
    const fake = core(() => json(BACKUP), { head: () => (++heads === 1 ? first.promise : headOk()) });
    const region = await created(fake);
    const button = within(region).getByRole("button", { name: "Download" });
    fireEvent.click(button);
    await waitFor(() => expect(button.getAttribute("aria-disabled")).toBe("true"));
    fireEvent.click(button);
    expect(fake.heads()).toHaveLength(1);
    expect(clicked).toEqual([]);
    await act(async () => first.resolve(headOk()));
    await waitFor(() => expect(clicked).toEqual([{ href: ARCHIVE, download: BACKUP.name }]));
    expect(button.getAttribute("aria-disabled")).toBeNull();
    fireEvent.click(button);
    await waitFor(() => expect(clicked).toHaveLength(2));
    expect(fake.heads()).toHaveLength(2);
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

  it("posts the file once, says Checking backup, then shows the backup's time, app version and schema version and Restore this backup", async () => {
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
    expect(init?.redirect).toBe("manual");
    expect(new Headers(init?.headers).get("content-type")).toBe("application/gzip");
    expect(within(region).getByRole("button", { name: "Restore this backup" })).toBeTruthy();
    expect(document.querySelector("dialog, [role=dialog]")).toBeNull();
    expect(fake.restores()).toEqual([]);
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
    expect(fake.restores()).toEqual([]);
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

const page = () => within(document.body);
const RESTORED = { restored_schema_version: 34, schema_version: 34 };
const restoreProblem = (status: number, extra: Record<string, unknown> = {}) =>
  new Response(
    JSON.stringify({ type: "urn:print-partner:problem:conflict", title: "Refused", status, detail: "Refused here", ...extra }),
    { status, headers: { "content-type": "application/problem+json" } },
  );

/** Opens Settings, checks a file and opens the confirmation dialog. */
async function confirming(fake: ReturnType<typeof core>) {
  const rendered = renderSettings(fake.fetchMock);
  const region = await backups();
  await online(region);
  choose(region, fileOf());
  fireEvent.click(checkButton(region));
  fireEvent.click(await within(region).findByRole("button", { name: "Restore this backup" }));
  const dialog = await page().findByRole("dialog", { name: "Replace this installation?" });
  return { ...rendered, region, dialog };
}

/**
 * Asserts the section is locked: no way to send a restore, a check or a create. `hidden` reaches
 * the page behind an open dialog, which the dialog hides from assistive technology.
 */
function expectLocked(region: HTMLElement) {
  const button = (name: string) => page().queryByRole("button", { name, hidden: true });
  expect(button("Replace this installation")).toBeNull();
  expect(button("Restore this backup")).toBeNull();
  expect(button("Close")).toBeNull();
  expect(button("Check backup")?.getAttribute("aria-disabled")).toBe("true");
  expect(button("Create backup")?.getAttribute("aria-disabled")).toBe("true");
  expect((within(region).getByLabelText("Backup file") as HTMLInputElement).disabled).toBe(true);
}

describe("Backups: restore", () => {
  it("asks first: Cancel has focus and sends nothing; Replace sends one POST without following redirects", async () => {
    const answer = deferred();
    const fake = core(undefined, { stage: () => json(PREVIEW), restore: () => answer.promise });
    const { region, dialog } = await confirming(fake);
    expect(
      within(region).getByText(
        "The core keeps a checked backup only until it restarts. If it restarts before you confirm, choose the file again and check it.",
      ),
    ).toBeTruthy();
    expect(dialog.querySelector("[data-confirmation]")?.textContent).toBe(PREVIEW.confirmation);
    await waitFor(() => expect(document.activeElement).toBe(within(dialog).getByRole("button", { name: "Cancel" })));
    fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
    expect(page().queryByRole("dialog")).toBeNull();
    expect(fake.restores()).toEqual([]);

    fireEvent.click(within(region).getByRole("button", { name: "Restore this backup" }));
    const again = await page().findByRole("dialog");
    fireEvent.click(within(again).getByRole("button", { name: "Replace this installation" }));
    fireEvent.click(within(again).queryByRole("button", { name: "Replace this installation" }) ?? again);
    expect(await within(again).findByText("Restoring. Keep this page open.")).toBeTruthy();
    expect(fake.restores()).toHaveLength(1);
    const init = fake.restores()[0]?.[1];
    expect(init?.redirect).toBe("manual");
    expect(JSON.parse(String(init?.body))).toEqual({ restore_id: PREVIEW.restore_id, confirmation: PREVIEW.confirmation });
    await act(async () => answer.resolve(json(RESTORED)));
    expect(await within(again).findByText(/^Restore complete/)).toBeTruthy();
    expect(within(again).getByRole("button", { name: "Reload" })).toBeTruthy();
    expectLocked(region);
    expect(fake.restores()).toHaveLength(1);
  });

  it.each([
    ["503 restore_busy", () => restoreProblem(503, { code: "restore_busy" })],
    ["a code-less 409", () => restoreProblem(409)],
  ])("%s says unchanged, offers Close, and a second confirmation sends a second POST", async (_, refusal) => {
    let sent = 0;
    const fake = core(undefined, { stage: () => json(PREVIEW), restore: () => (++sent === 1 ? refusal() : json(RESTORED)) });
    const { region, dialog } = await confirming(fake);
    fireEvent.click(within(dialog).getByRole("button", { name: "Replace this installation" }));
    expect((await within(dialog).findByRole("alert")).textContent).toBe("The restore was refused: Refused here. This installation is unchanged.");
    fireEvent.click(within(dialog).getByRole("button", { name: "Close" }));
    fireEvent.click(within(region).getByRole("button", { name: "Restore this backup" }));
    fireEvent.click(within(await page().findByRole("dialog")).getByRole("button", { name: "Replace this installation" }));
    expect(await page().findByText(/^Restore complete/)).toBeTruthy();
    expect(fake.restores()).toHaveLength(2);
  });

  it("404 restore_not_found empties the file field and asks to choose the file again", async () => {
    const fake = core(undefined, { stage: () => json(PREVIEW), restore: () => restoreProblem(404, { code: "restore_not_found" }) });
    const { region, dialog } = await confirming(fake);
    fireEvent.click(within(dialog).getByRole("button", { name: "Replace this installation" }));
    expect((await within(region).findByRole("alert")).textContent).toMatch(/^The core no longer holds this check/);
    expect(page().queryByRole("dialog")).toBeNull();
    expect(within(region).queryByRole("button", { name: "Restore this backup" })).toBeNull();
    expect(checkButton(region).getAttribute("aria-disabled")).toBe("true");
    expect((within(region).getByLabelText("Backup file") as HTMLInputElement).disabled).toBe(false);
  });

  it.each([
    ["code null", () => restoreProblem(409, { code: null })],
    ["code 409", () => restoreProblem(409, { code: 409 })],
    ["code as an object", () => restoreProblem(409, { code: { code: "restore_busy" } })],
    ["an empty code", () => restoreProblem(503, { code: "" })],
    ["an unknown code", () => restoreProblem(503, { code: "restore_maybe" })],
    ["restore_busy on a 409", () => restoreProblem(409, { code: "restore_busy" })],
    ["restart_required", () => restoreProblem(503, { code: "restart_required" })],
    ["a code-less 503", () => restoreProblem(503)],
    ["a status member that disagrees", () => restoreProblem(409, { status: 400 })],
    ["a 409 that is not a problem", () => json({ status: 409 }, 409)],
    ["a 502 proxy page", () => new Response("<html>Bad Gateway</html>", { status: 502, headers: { "content-type": "text/html" } })],
    ["a 200 that fails the contract", () => json({ ok: true })],
    ["a lost answer", () => Promise.reject(new TypeError("Failed to fetch"))],
  ])("%s is an unknown outcome: Reload only, locked, one POST", async (_, answer: Answer) => {
    const fake = core(undefined, { stage: () => json(PREVIEW), restore: answer });
    const { region, dialog } = await confirming(fake);
    fireEvent.click(within(dialog).getByRole("button", { name: "Replace this installation" }));
    expect((await within(dialog).findByRole("alert")).textContent).toBe(LOST_RESTORE);
    expect(within(dialog).getByRole("button", { name: "Reload" })).toBeTruthy();
    expectLocked(region);
    fireEvent.keyDown(dialog, { key: "Escape" });
    fireEvent.click(checkButton(region));
    fireEvent.click(within(region).getByRole("button", { name: "Create backup" }));
    expectLocked(region);
    expect(fake.restores()).toHaveLength(1);
    expect(fake.stages()).toHaveLength(1);
    expect(fake.creates()).toHaveLength(0);
  });

  it("leaving Settings and coming back keeps a lost restore locked", async () => {
    const fake = core(undefined, { stage: () => json(PREVIEW), restore: () => restoreProblem(409, { code: null }) });
    const { router, dialog } = await confirming(fake);
    fireEvent.click(within(dialog).getByRole("button", { name: "Replace this installation" }));
    await within(dialog).findByText(LOST_RESTORE);
    await act(() => router.navigate("/farm/log"));
    await waitFor(() => expect(page().queryByRole("region", { name: "Backups" })).toBeNull());
    await act(() => router.navigate("/settings"));
    const region = await backups();
    expect(within(region).getByText(LOST_RESTORE)).toBeTruthy();
    expect(within(region).getByRole("button", { name: "Reload" })).toBeTruthy();
    choose(region, fileOf());
    expectLocked(region);
    expect(fake.restores()).toHaveLength(1);
  });

  it("a restore still running when Settings closes shows its answer when Settings opens again", async () => {
    const answer = deferred();
    const fake = core(undefined, { stage: () => json(PREVIEW), restore: () => answer.promise });
    const { router, dialog } = await confirming(fake);
    fireEvent.click(within(dialog).getByRole("button", { name: "Replace this installation" }));
    await waitFor(() => expect(fake.restores()).toHaveLength(1));
    await act(() => router.navigate("/farm/log"));
    await act(async () => answer.resolve(new Response("", { status: 504 })));
    await act(() => router.navigate("/settings"));
    const region = await backups();
    expect(await within(region).findByText(LOST_RESTORE)).toBeTruthy();
    expectLocked(region);
  });
});

/** A real loopback server for the restore POST, so Node's own fetch decides whether to follow. */
describe("Backups: a redirected restore", () => {
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
      if (request.url === RESTORE) return send(redirect.status, "text/plain", "", { location: redirect.to });
      if (request.url === "/ok") return send(200, "application/json", RESTORED);
      if (request.url === "/problem")
        return send(503, "application/problem+json", { type: "urn:print-partner:problem:overloaded", title: "Service Unavailable", status: 503, detail: "busy", code: "restore_busy" });
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
  ] as const)("a %i to %s sends one POST and is an unknown outcome", async (status, to) => {
    received.length = 0;
    redirect = { status, to };
    const fake = core(undefined, { stage: () => json(PREVIEW), restore: (init) => nodeFetch(`${origin}${RESTORE}`, init) });
    const { region, dialog } = await confirming(fake);
    fireEvent.click(within(dialog).getByRole("button", { name: "Replace this installation" }));
    expect((await within(dialog).findByRole("alert")).textContent).toBe(LOST_RESTORE);
    expectLocked(region);
    expect(received).toEqual([`POST ${RESTORE}`]);
  });
});
