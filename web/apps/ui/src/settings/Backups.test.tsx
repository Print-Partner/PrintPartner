import type { Health } from "@print-partner/rust-client";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { createMemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App, createQueryClient } from "../App";
import { routes } from "../routes";
import { ARCHIVE_PATH, LOST_RESTORE } from "./BackupsSection";
import { forgetRestore } from "./restoreOutcome";

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
const problem = (status: number, detail: string, code?: string) =>
  new Response(JSON.stringify({ type: "about:blank", title: "Refused", status, detail, ...(code ? { code } : {}) }), {
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

const SHA = "9f".repeat(32);
const BACKUP = {
  name: "print-partner-backup-20261117T231500Z.tar.gz",
  created_at: "2026-11-17T23:15:00Z",
  app_version: "0.42.0",
  size_bytes: 1_234_567,
  sha256: SHA,
  warnings: [],
};
const CONFIRMATION =
  "Replace this installation's database and files. Prints already sent and Spoolman usage already recorded are not reversed.";
const PREVIEW = {
  restore_id: "0123456789abcdef0123456789abcdef",
  created_at: "2026-11-17T23:15:00Z",
  app_version: "0.42.0",
  schema_version: 52,
  confirmation: CONFIRMATION,
};

const CREATE = "/api/v2/installation/backup";
const STAGE = "/api/v2/installation/restore/stage";
const RESTORE = "/api/v2/installation/restore";
type Answer = (init?: RequestInit) => Response | Promise<Response>;

/** A fake core: /health and empty Printers and Integrations; the backup routes as given. */
function core({ create, stage, restore, archive }: {
  create?: Answer;
  stage?: Answer;
  restore?: Answer;
  /** The archive route, given its `sha256` query and the request. */
  archive?: (sha256: string | null, init?: RequestInit) => Response;
} = {}) {
  let down = false;
  const fetchMock = vi.fn((path: string, init?: RequestInit) =>
    Promise.resolve().then((): Response | Promise<Response> => {
      if (down) throw new TypeError("Failed to fetch");
      if (path === "/health") return json(HEALTH);
      if (path === "/api/v2/printers") return json({ printers: [] });
      if (path === "/api/v2/integrations") return json({ integrations: [] });
      if (path === CREATE && init?.method === "POST" && create) return create(init);
      if (path === STAGE && init?.method === "POST" && stage) return stage(init);
      if (path === RESTORE && init?.method === "POST" && restore) return restore(init);
      const url = new URL(path, "http://core.test");
      if (url.pathname === ARCHIVE_PATH && archive) return archive(url.searchParams.get("sha256"), init);
      return problem(404, `unexpected ${path}`);
    }),
  );
  const calls = (path: string) => fetchMock.mock.calls.filter(([called]) => called === path);
  return { fetchMock, calls, goDown: () => (down = true) };
}

function renderSettings(fetchMock: ReturnType<typeof core>["fetchMock"], container?: HTMLElement) {
  vi.stubGlobal("fetch", fetchMock);
  const router = createMemoryRouter(routes, { initialEntries: ["/settings"] });
  const queryClient = createQueryClient();
  queryClient.setDefaultOptions({ queries: { ...queryClient.getDefaultOptions().queries, retry: false } });
  render(<App router={router} queryClient={queryClient} />, container ? { container } : undefined);
  return { queryClient, router };
}

/** Every anchor the page clicked to start a download, with its href and download attribute. */
function captureDownloads() {
  const clicked: { href: string | null; download: string | null }[] = [];
  vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(function (this: HTMLAnchorElement) {
    clicked.push({ href: this.getAttribute("href"), download: this.getAttribute("download") });
  });
  return clicked;
}

/** A 503 whose headers arrived and whose body stream then broke, so its problem cannot be read. */
const unreadable503 = () =>
  new Response(
    new ReadableStream({
      start(controller) {
        controller.error(new TypeError("network error"));
      },
    }),
    { status: 503, headers: { "content-type": "application/problem+json" } },
  );

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

const archive = () => new File([new Uint8Array([0x1f, 0x8b, 8, 0, 1, 2, 3])], "backup.tar.gz", { type: "application/gzip" });

async function chooseAndCheck(region: HTMLElement, file = archive()) {
  fireEvent.change(within(region).getByLabelText("Backup file"), { target: { files: [file] } });
  fireEvent.click(within(region).getByRole("button", { name: "Check backup" }));
  return file;
}

afterEach(() => {
  cleanup();
  forgetRestore();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("Backups: create and download", () => {
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

  it("creates one backup and shows its name, time, version, size and SHA-256 with a Download link", async () => {
    const answer = deferred();
    const fake = core({ create: () => answer.promise });
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
    const link = within(facts).getByRole("link", { name: "Download" });
    expect(link.getAttribute("href")).toBe(`/api/v2/installation/backup/archive?sha256=${SHA}`);
    expect(link.getAttribute("download")).toBe(BACKUP.name);
    expect(within(region).queryByText("Creating backup")).toBeNull();
    expect(fake.calls(CREATE)).toHaveLength(1);
    expect(fake.calls(CREATE)[0]?.[1]?.body).toBeUndefined();
  });

  it("in a second tab, a busy core says a backup is already running and the first tab keeps its backup", async () => {
    let running = false;
    const first = deferred();
    const fake = core({
      create: () => {
        if (running) return problem(503, "A backup is already running");
        running = true;
        return first.promise.finally(() => (running = false));
      },
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

  it("after a second tab replaces the kept archive, the first tab's Download saves nothing and drops its stale facts", async () => {
    const NEWER = { ...BACKUP, size_bytes: 2_345_678, sha256: "a1".repeat(32) };
    let kept: typeof BACKUP | null = null;
    const made = [BACKUP, NEWER];
    const fake = core({
      create: () => {
        kept = made.shift() ?? null;
        return json(kept);
      },
      archive: (sha256) => {
        if (kept === null) return problem(404, "No backup has been created");
        if (sha256 !== null && sha256 !== kept.sha256) {
          return problem(409, "A newer backup replaced this one; create a backup again");
        }
        return new Response(null, { status: 200, headers: { "content-type": "application/gzip" } });
      },
    });
    const saved = captureDownloads();
    const tabOne = document.body.appendChild(document.createElement("div"));
    const tabTwo = document.body.appendChild(document.createElement("div"));
    renderSettings(fake.fetchMock, tabOne);
    renderSettings(fake.fetchMock, tabTwo);
    const one = await backups(tabOne);
    const two = await backups(tabTwo);
    await online(one);
    await online(two);
    fireEvent.click(within(one).getByRole("button", { name: "Create backup" }));
    expect(await within(one).findByText(SHA)).toBeTruthy();
    fireEvent.click(within(two).getByRole("button", { name: "Create backup" }));
    expect(await within(two).findByText(NEWER.sha256)).toBeTruthy();

    fireEvent.click(within(one).getByRole("link", { name: "Download" }));
    expect((await within(one).findByRole("alert")).textContent).toBe(
      "The core no longer keeps the backup shown here; a newer backup may have been made in another window. Create backup again to download one.",
    );
    expect(within(one).queryByText(SHA)).toBeNull();
    expect(within(one).queryByRole("link", { name: "Download" })).toBeNull();
    expect(saved).toEqual([]);

    fireEvent.click(within(two).getByRole("link", { name: "Download" }));
    await waitFor(() =>
      expect(saved).toEqual([{ href: `${ARCHIVE_PATH}?sha256=${NEWER.sha256}`, download: NEWER.name }]),
    );
    const heads = fake.fetchMock.mock.calls.filter(([path]) => path.startsWith(ARCHIVE_PATH));
    expect(heads.map(([path, init]) => [path, init?.method])).toEqual([
      [`${ARCHIVE_PATH}?sha256=${SHA}`, "HEAD"],
      [`${ARCHIVE_PATH}?sha256=${NEWER.sha256}`, "HEAD"],
    ]);
  });

  it("offline: Create backup is off with the offline reason and sends nothing", async () => {
    const fake = core({ create: () => json(BACKUP) });
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
    expect(fake.calls(CREATE)).toHaveLength(0);
  });

  it("a lost create answer shows no backup and says to create again", async () => {
    const fake = core({ create: () => Promise.reject(new TypeError("Failed to fetch")) });
    renderSettings(fake.fetchMock);
    const region = await backups();
    await online(region);
    fireEvent.click(within(region).getByRole("button", { name: "Create backup" }));
    expect((await within(region).findByRole("alert")).textContent).toBe(
      "The core did not confirm the backup. Create backup again to download one.",
    );
    expect(within(region).queryByRole("link", { name: "Download" })).toBeNull();
  });

  it.each([
    ["a 300", () => new Response(null, { status: 300 })],
    ["a 502 proxy page", () => new Response("<html>Bad Gateway</html>", { status: 502, headers: { "content-type": "text/html" } })],
  ])("%s after a backup was shown drops it and says the core did not confirm", async (_, answer) => {
    let made = false;
    const fake = core({ create: () => (made ? answer() : ((made = true), json(BACKUP))) });
    renderSettings(fake.fetchMock);
    const region = await backups();
    await online(region);
    fireEvent.click(within(region).getByRole("button", { name: "Create backup" }));
    expect(await within(region).findByText(SHA)).toBeTruthy();
    fireEvent.click(within(region).getByRole("button", { name: "Create backup" }));
    expect((await within(region).findByRole("alert")).textContent).toBe(
      "The core did not confirm the backup. Create backup again to download one.",
    );
    expect(within(region).queryByText(SHA)).toBeNull();
    expect(within(region).queryByRole("link", { name: "Download" })).toBeNull();
    expect(fake.calls(CREATE)).toHaveLength(2);
  });
});

describe("Backups: check and restore", () => {
  it("sends nothing when a file is chosen; Check backup posts the file and shows its time and versions", async () => {
    const fake = core({ stage: () => json(PREVIEW) });
    renderSettings(fake.fetchMock);
    const region = await backups();
    await online(region);
    fireEvent.change(within(region).getByLabelText("Backup file"), { target: { files: [archive()] } });
    expect(fake.calls(STAGE)).toHaveLength(0);
    const file = await chooseAndCheck(region);
    const facts = await within(region).findByLabelText("Backup checked");
    expect(within(facts).getByText("0.42.0")).toBeTruthy();
    expect(within(facts).getByText("52")).toBeTruthy();
    expect(within(facts).getByText(/2026/)).toBeTruthy();
    expect(fake.calls(STAGE)).toHaveLength(1);
    const [, init] = fake.calls(STAGE)[0] ?? [];
    expect(init?.body).toBe(file);
    expect(new Headers(init?.headers).get("content-type")).toBe("application/gzip");
    expect(fake.calls(RESTORE)).toHaveLength(0);
  });

  it("Restore this backup opens a dialog on Cancel; Cancel sends no restore", async () => {
    const fake = core({ stage: () => json(PREVIEW), restore: () => json({ restored_schema_version: 52, schema_version: 52 }) });
    renderSettings(fake.fetchMock);
    const region = await backups();
    await online(region);
    await chooseAndCheck(region);
    fireEvent.click(await within(region).findByRole("button", { name: "Restore this backup" }));
    const dialog = await screen.findByRole("dialog", { name: "Replace this installation?" });
    expect(within(dialog).getByText(CONFIRMATION)).toBeTruthy();
    expect(within(dialog).getByText(/Builds, Sources and history are replaced/)).toBeTruthy();
    await waitFor(() => expect(document.activeElement).toBe(within(dialog).getByRole("button", { name: "Cancel" })));
    fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(fake.calls(RESTORE)).toHaveLength(0);
  });

  it("Replace this installation sends one restore with the id and the core's text, then offers Reload", async () => {
    const answer = deferred();
    const fake = core({ stage: () => json(PREVIEW), restore: () => answer.promise });
    const reload = vi.fn();
    vi.stubGlobal("location", { ...window.location, reload });
    renderSettings(fake.fetchMock);
    const region = await backups();
    await online(region);
    await chooseAndCheck(region);
    fireEvent.click(await within(region).findByRole("button", { name: "Restore this backup" }));
    const dialog = await screen.findByRole("dialog");
    const replace = within(dialog).getByRole("button", { name: "Replace this installation" });
    fireEvent.click(replace);
    fireEvent.click(replace);
    expect(await within(dialog).findByText("Restoring. Keep this page open.")).toBeTruthy();
    expect(within(dialog).queryByRole("button")).toBeNull();
    fireEvent.keyDown(dialog, { key: "Escape" });
    expect(screen.getByRole("dialog")).toBeTruthy();
    await act(async () => answer.resolve(json({ restored_schema_version: 52, schema_version: 52 })));
    expect(await within(dialog).findByText("Restore complete")).toBeTruthy();
    expect(fake.calls(RESTORE)).toHaveLength(1);
    const [, init] = fake.calls(RESTORE)[0] ?? [];
    expect(JSON.parse(String(init?.body))).toEqual({ restore_id: PREVIEW.restore_id, confirmation: CONFIRMATION });
    fireEvent.click(within(dialog).getByRole("button", { name: "Reload" }));
    expect(reload).toHaveBeenCalledTimes(1);
  });

  it("a truncated file shows the core's refusal and no restore can be opened", async () => {
    const fake = core({ stage: () => problem(422, "Invalid backup archive") });
    renderSettings(fake.fetchMock);
    const region = await backups();
    await online(region);
    await chooseAndCheck(region);
    expect((await within(region).findByRole("alert")).textContent).toBe(
      "The backup was not accepted: Invalid backup archive. This installation is unchanged.",
    );
    expect(within(region).queryByRole("button", { name: "Restore this backup" })).toBeNull();
    expect(fake.calls(RESTORE)).toHaveLength(0);
  });

  it("after a restart drops the check, confirming says to choose the file again", async () => {
    const fake = core({
      stage: () => json(PREVIEW),
      restore: () => problem(404, "No staged restore with that id; upload the backup again", "restore_not_found"),
    });
    renderSettings(fake.fetchMock);
    const region = await backups();
    await online(region);
    await chooseAndCheck(region);
    fireEvent.click(await within(region).findByRole("button", { name: "Restore this backup" }));
    fireEvent.click(within(await screen.findByRole("dialog")).getByRole("button", { name: "Replace this installation" }));
    expect((await within(region).findByRole("alert")).textContent).toMatch(/Choose the file again/);
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(within(region).queryByRole("button", { name: "Restore this backup" })).toBeNull();
    expect((within(region).getByLabelText("Backup file") as HTMLInputElement).files?.length ?? 0).toBe(0);
    expect(within(region).getByRole("button", { name: "Check backup" }).getAttribute("aria-disabled")).toBe("true");
    expect(fake.calls(RESTORE)).toHaveLength(1);
  });

  it("a lost restore answer shows the lost-answer sentence and never sends the restore again", async () => {
    const fake = core({ stage: () => json(PREVIEW), restore: () => Promise.reject(new TypeError("Failed to fetch")) });
    renderSettings(fake.fetchMock);
    const region = await backups();
    await online(region);
    await chooseAndCheck(region);
    fireEvent.click(await within(region).findByRole("button", { name: "Restore this backup" }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.click(within(dialog).getByRole("button", { name: "Replace this installation" }));
    expect((await within(dialog).findByRole("alert")).textContent).toBe(LOST_RESTORE);
    expect(within(dialog).queryByRole("button", { name: "Replace this installation" })).toBeNull();
    expect(within(dialog).getByRole("button", { name: "Reload" })).toBeTruthy();
    // Escape closes the dialog once nothing is running; the section keeps the sentence.
    fireEvent.keyDown(dialog, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(within(region).getByRole("alert").textContent).toBe(LOST_RESTORE);
    expect(within(region).getByRole("button", { name: "Reload" })).toBeTruthy();
    expect(within(region).queryByRole("button", { name: "Restore this backup" })).toBeNull();
    expect(within(region).getByRole("button", { name: "Check backup" }).getAttribute("aria-disabled")).toBe("true");
    expect(within(region).getByRole("button", { name: "Create backup" }).getAttribute("aria-disabled")).toBe("true");
    expect(fake.calls(RESTORE)).toHaveLength(1);
  });

  it("restart_required is not called unchanged; it shows the core's reason and the lost-answer sentence", async () => {
    const fake = core({
      stage: () => json(PREVIEW),
      restore: () => problem(503, "Restore did not finish; restart Print Partner to recover", "restart_required"),
    });
    renderSettings(fake.fetchMock);
    const region = await backups();
    await online(region);
    await chooseAndCheck(region);
    fireEvent.click(await within(region).findByRole("button", { name: "Restore this backup" }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.click(within(dialog).getByRole("button", { name: "Replace this installation" }));
    expect((await within(dialog).findByRole("alert")).textContent).toBe(
      `Restore did not finish; restart Print Partner to recover. ${LOST_RESTORE}`,
    );
    expect(fake.calls(RESTORE)).toHaveLength(1);
  });

  it("a 503 whose body cannot be read is an unknown outcome: no 'unchanged', no retry, Reload only", async () => {
    const fake = core({ stage: () => json(PREVIEW), restore: unreadable503 });
    renderSettings(fake.fetchMock);
    const region = await backups();
    await online(region);
    await chooseAndCheck(region);
    fireEvent.click(await within(region).findByRole("button", { name: "Restore this backup" }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.click(within(dialog).getByRole("button", { name: "Replace this installation" }));
    expect((await within(dialog).findByRole("alert")).textContent).toBe(LOST_RESTORE);
    expect(within(dialog).queryByText(/unchanged/)).toBeNull();
    expect(within(dialog).queryByRole("button", { name: "Close" })).toBeNull();
    expect(within(dialog).getByRole("button", { name: "Reload" })).toBeTruthy();
    fireEvent.keyDown(dialog, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(within(region).queryByRole("button", { name: "Restore this backup" })).toBeNull();
    expect(within(region).getByRole("button", { name: "Check backup" }).getAttribute("aria-disabled")).toBe("true");
    expect(fake.calls(RESTORE)).toHaveLength(1);
  });

  it("leaving Settings and coming back keeps a lost restore's outcome and lock until a reload", async () => {
    const answer = deferred();
    const fake = core({ stage: () => json(PREVIEW), restore: () => answer.promise });
    const { router } = renderSettings(fake.fetchMock);
    let region = await backups();
    await online(region);
    await chooseAndCheck(region);
    fireEvent.click(await within(region).findByRole("button", { name: "Restore this backup" }));
    fireEvent.click(within(await screen.findByRole("dialog")).getByRole("button", { name: "Replace this installation" }));
    expect(await screen.findByText("Restoring. Keep this page open.")).toBeTruthy();

    // Settings closes while the restore runs; its answer is lost after that.
    await act(() => router.navigate("/builds"));
    await waitFor(() => expect(screen.queryByRole("region", { name: "Backups" })).toBeNull());
    await act(async () => answer.resolve(Promise.reject(new TypeError("Failed to fetch")) as never));

    await act(() => router.navigate("/settings"));
    region = await backups();
    expect((await within(region).findByRole("alert")).textContent).toBe(LOST_RESTORE);
    expect(within(region).getByRole("button", { name: "Reload" })).toBeTruthy();
    expect(within(region).getByRole("button", { name: "Check backup" }).getAttribute("aria-disabled")).toBe("true");
    expect((within(region).getByLabelText("Backup file") as HTMLInputElement).disabled).toBe(true);
    expect(within(region).getByRole("button", { name: "Create backup" }).getAttribute("aria-disabled")).toBe("true");

    // Leaving and coming back once more still keeps it.
    await act(() => router.navigate("/builds"));
    await act(() => router.navigate("/settings"));
    region = await backups();
    expect(within(region).getByRole("alert").textContent).toBe(LOST_RESTORE);
    expect(within(region).queryByRole("button", { name: "Restore this backup" })).toBeNull();
    expect(fake.calls(RESTORE)).toHaveLength(1);
  });

  // The core sends this code-less 503 when the writer's reply is lost after the restore was
  // queued (`WriterFailure::Stopped`), so its body being readable proves nothing.
  it.each([
    ["a readable code-less 503", () => problem(503, "Service temporarily unavailable")],
    ["a 503 with another code", () => problem(503, "Service temporarily unavailable", "transaction_unavailable")],
    ["a 504 from a proxy", () => problem(504, "Gateway Timeout")],
  ])("%s after the restore is sent is an unknown outcome: no 'unchanged', no second restore", async (_, answer) => {
    const fake = core({ stage: () => json(PREVIEW), restore: answer });
    renderSettings(fake.fetchMock);
    const region = await backups();
    await online(region);
    await chooseAndCheck(region);
    fireEvent.click(await within(region).findByRole("button", { name: "Restore this backup" }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.click(within(dialog).getByRole("button", { name: "Replace this installation" }));
    expect((await within(dialog).findByRole("alert")).textContent).toMatch(new RegExp(`${LOST_RESTORE}$`));
    expect(within(dialog).queryByText(/unchanged/)).toBeNull();
    expect(within(dialog).queryByRole("button", { name: "Close" })).toBeNull();
    expect(within(dialog).queryByRole("button", { name: "Replace this installation" })).toBeNull();
    expect(within(dialog).getByRole("button", { name: "Reload" })).toBeTruthy();
    fireEvent.keyDown(dialog, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(within(region).queryByRole("button", { name: "Restore this backup" })).toBeNull();
    expect(within(region).getByRole("button", { name: "Reload" })).toBeTruthy();
    expect(within(region).getByRole("button", { name: "Check backup" }).getAttribute("aria-disabled")).toBe("true");
    expect((within(region).getByLabelText("Backup file") as HTMLInputElement).disabled).toBe(true);
    expect(within(region).getByRole("button", { name: "Create backup" }).getAttribute("aria-disabled")).toBe("true");
    expect(fake.calls(RESTORE)).toHaveLength(1);
  });

  // The core never answers the restore route with 3xx; one comes from a layer in front of it
  // and proves nothing about what the core did.
  it.each([
    ["a 300", () => new Response(null, { status: 300 })],
    ["a 302 without Location", () => new Response(null, { status: 302 })],
    ["a 400 that is not the core's problem body", () => json({ detail: "Bad Request" }, 400)],
    [
      "a 409 problem whose status disagrees",
      () =>
        new Response(JSON.stringify({ type: "about:blank", title: "Refused", status: 503, detail: "Refused" }), {
          status: 409,
          headers: { "content-type": "application/problem+json" },
        }),
    ],
  ])("%s is an unknown outcome: locked, Reload only, exactly one restore request", async (_, answer) => {
    const fake = core({ stage: () => json(PREVIEW), restore: answer });
    renderSettings(fake.fetchMock);
    const region = await backups();
    await online(region);
    await chooseAndCheck(region);
    fireEvent.click(await within(region).findByRole("button", { name: "Restore this backup" }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.click(within(dialog).getByRole("button", { name: "Replace this installation" }));
    expect((await within(dialog).findByRole("alert")).textContent).toBe(LOST_RESTORE);
    expect(within(dialog).queryByText(/unchanged/)).toBeNull();
    expect(within(dialog).queryByRole("button", { name: "Close" })).toBeNull();
    expect(within(dialog).queryByRole("button", { name: "Replace this installation" })).toBeNull();
    expect(within(dialog).getAllByRole("button").map((button) => button.textContent)).toEqual(["Reload"]);
    fireEvent.keyDown(dialog, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(within(region).queryByRole("button", { name: "Restore this backup" })).toBeNull();
    expect(within(region).getByRole("button", { name: "Reload" })).toBeTruthy();
    expect(within(region).getByRole("button", { name: "Check backup" }).getAttribute("aria-disabled")).toBe("true");
    expect((within(region).getByLabelText("Backup file") as HTMLInputElement).disabled).toBe(true);
    expect(within(region).getByRole("button", { name: "Create backup" }).getAttribute("aria-disabled")).toBe("true");
    expect(fake.calls(RESTORE)).toHaveLength(1);
  });

  it("sends the restore with redirects refused, so a 307 or 308 cannot send it again", async () => {
    const fake = core({ stage: () => json(PREVIEW), restore: () => json({ restored_schema_version: 52, schema_version: 52 }) });
    renderSettings(fake.fetchMock);
    const region = await backups();
    await online(region);
    await chooseAndCheck(region);
    fireEvent.click(await within(region).findByRole("button", { name: "Restore this backup" }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.click(within(dialog).getByRole("button", { name: "Replace this installation" }));
    expect(await within(dialog).findByText("Restore complete")).toBeTruthy();
    const [, init] = fake.calls(RESTORE)[0] ?? [];
    expect(init?.redirect).toBe("error");
    expect(init?.method).toBe("POST");
  });

  it.each([
    ["Storage work is still active; try again"],
    ["A restore is already running"],
  ])("restore_busy (%s) says the installation is unchanged and allows a deliberate retry", async (detail) => {
    let busy = true;
    const fake = core({
      stage: () => json(PREVIEW),
      restore: () => (busy ? problem(503, detail, "restore_busy") : json({ restored_schema_version: 52, schema_version: 52 })),
    });
    renderSettings(fake.fetchMock);
    const region = await backups();
    await online(region);
    await chooseAndCheck(region);
    fireEvent.click(await within(region).findByRole("button", { name: "Restore this backup" }));
    let dialog = await screen.findByRole("dialog");
    fireEvent.click(within(dialog).getByRole("button", { name: "Replace this installation" }));
    expect((await within(dialog).findByRole("alert")).textContent).toBe(
      `The restore was refused: ${detail}. This installation is unchanged.`,
    );
    fireEvent.click(within(dialog).getByRole("button", { name: "Close" }));
    expect(within(region).getByRole("alert").textContent).toMatch(/This installation is unchanged/);
    busy = false;
    fireEvent.click(within(region).getByRole("button", { name: "Restore this backup" }));
    dialog = await screen.findByRole("dialog");
    fireEvent.click(within(dialog).getByRole("button", { name: "Replace this installation" }));
    expect(await within(dialog).findByText("Restore complete")).toBeTruthy();
    expect(fake.calls(RESTORE)).toHaveLength(2);
  });

  it("a rolled-back restore is unchanged and asks for the file again", async () => {
    const fake = core({
      stage: () => json(PREVIEW),
      restore: () => problem(500, "Restore failed; the previous installation is kept", "restore_rolled_back"),
    });
    renderSettings(fake.fetchMock);
    const region = await backups();
    await online(region);
    await chooseAndCheck(region);
    fireEvent.click(await within(region).findByRole("button", { name: "Restore this backup" }));
    fireEvent.click(within(await screen.findByRole("dialog")).getByRole("button", { name: "Replace this installation" }));
    expect((await within(region).findByRole("alert")).textContent).toBe(
      "The restore was refused: Restore failed; the previous installation is kept. This installation is unchanged. Choose the file again to retry.",
    );
    expect(within(region).queryByRole("button", { name: "Restore this backup" })).toBeNull();
  });

  it("offline: Check backup and Restore this backup are off and send nothing", async () => {
    const fake = core({ stage: () => json(PREVIEW) });
    const { queryClient } = renderSettings(fake.fetchMock);
    const region = await backups();
    await online(region);
    await chooseAndCheck(region);
    await within(region).findByLabelText("Backup checked");
    fake.goDown();
    await queryClient.refetchQueries({ queryKey: ["core", "health"] });
    const restore = within(region).getByRole("button", { name: "Restore this backup" });
    await waitFor(() => expect(restore.getAttribute("aria-disabled")).toBe("true"));
    fireEvent.click(restore);
    expect(screen.queryByRole("dialog")).toBeNull();
    fireEvent.click(within(region).getByRole("button", { name: "Check backup" }));
    expect(fake.calls(STAGE)).toHaveLength(1);
  });
});
