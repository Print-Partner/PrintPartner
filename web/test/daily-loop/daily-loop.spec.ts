import { Buffer } from "node:buffer";
import { spawnSync } from "node:child_process";
import { readFile } from "node:fs/promises";
import process from "node:process";
import { crc32, inflateRawSync } from "node:zlib";
import { expect, test, type Locator, type Page } from "@playwright/test";
import { STEPS } from "./steps.mjs";

// The twelve steps of docs/product/daily-loop.md, in order, on one installation of the
// Rust-only image. Each test does what the owner does, at the screen or command he uses.

const CONTAINER = process.env.PP_DAILY_LOOP_CONTAINER ?? "";
const OWNER = { email: "owner@example.test", password: "daily-loop-owner-1" };

type Body = (fixtures: { page: Page }) => Promise<void>;

/**
 * One step. `ticket` is its expected-fail marker: the ticket that will make the step
 * pass, or null once it does. A marked step that passes fails the run, and so does an
 * unmarked step that fails; score.ts counts only unmarked passes.
 */
function step(number: number, ticket: `GRE-${number}` | null, body: Body) {
  const title = `${number}. ${STEPS[number - 1]}`;
  const walk: Body = async ({ page }) => {
    // Proof for score.ts that the fixtures were set up and the body started. A marked
    // step that fails before this, for one when the browser context fails, scores `fail`.
    test.info().annotations.push({ type: "entered" });
    await body({ page });
    // Proof for score.ts that the body ran to its end. Playwright can report a test it
    // never ran as passed, for one, after the browser failed to start.
    test.info().annotations.push({ type: "walked" });
  };
  if (ticket) test.fail(title, { annotation: { type: "ticket", description: ticket } }, walk);
  else test(title, walk);
}

/**
 * Ends a marked step at the point its ticket has to write the rest. The step keeps
 * failing until that ticket finishes the test, so a half-built screen never scores.
 */
function unwritten(rest: string): never {
  throw new Error(`This step's test is not written past here. Still to check: ${rest}.`);
}

const docker = (...args: string[]) =>
  spawnSync("docker", args, { encoding: "utf8", timeout: 30_000 });
const inContainer = (...command: string[]) => docker("exec", CONTAINER, ...command);

/**
 * Marks a failure of the harness itself: the server, the sign-in link or the owner's
 * sign-in. score.ts scores it `fail` even on a marked step, so a broken harness never
 * reads as twelve expected failures.
 */
const harness = <T>(what: string, action: () => Promise<T>) =>
  action().catch((error: unknown) => {
    throw new Error(`Harness: ${what}. ${String(error).split("\n")[0]}`);
  });

/** Waits for pp-server, which starts again after a sign-out stops it. */
const serverReady = () =>
  harness("pp-server is not ready", async () => {
    const readiness = () =>
      inContainer("pp-server", "healthcheck", "readiness", "--status-file", "/tmp/pp-server.status.json")
        .status;
    await expect.poll(readiness, { timeout: 180_000 }).toBe(0);
  });

/** Opens a new `pp-server bootstrap-url` link in this browser. */
async function openSignInLink(page: Page) {
  await serverReady();
  await harness("the sign-in link did not open", async () => {
    const link = inContainer("pp-server", "bootstrap-url", "--data", "/data");
    if (link.status !== 0) throw new Error(`bootstrap-url: ${link.stderr.trim()}`);
    // The link names the port inside the container. Docker publishes it on a host port
    // that can change when the container restarts (docs/docker.md, Sign in).
    const published = /-> (\S+)/.exec(docker("port", CONTAINER).stdout)?.[1];
    if (!published) throw new Error("docker port shows no published address");
    const url = new URL(link.stdout.trim());
    url.host = published;
    // The link is a secret, so a failed navigation must not print it.
    await page.goto(url.href).catch(() => {
      throw new Error("the browser could not load it");
    });
  });
}

async function fillSignIn(page: Page) {
  await page.getByLabel("Email").fill(OWNER.email);
  await page.getByLabel("Password").fill(OWNER.password);
  await page.getByRole("button", { name: "Sign in" }).click();
}

const screen = (page: Page, name: string) => page.getByRole("heading", { name, level: 1 });

/** Every step after the first starts here: a new browser, signed in as the owner, on Today. */
async function signIn(page: Page) {
  await openSignInLink(page);
  await harness("the owner could not sign in", async () => {
    await fillSignIn(page);
    await expect(screen(page, "Today")).toBeVisible();
  });
}

async function go(page: Page, destination: string) {
  const rail = page.getByRole("navigation", { name: "Main" });
  await rail.getByRole("link", { name: destination }).click();
  await expect(screen(page, destination)).toBeVisible();
}

const BUILD = "Daily loop kit";
// A stand-in for the kit's repository. The ticket that syncs it names a real one.
const KIT_URL = "https://github.com/example/daily-loop-kit";

/**
 * The assistant: an MCP client connected as docs/docker.md says, with an API key made
 * while signed in and neither cookie nor Origin. The key is a secret; nothing here prints it.
 */
async function connectAssistant(page: Page) {
  const origin = new URL(page.url()).origin;
  const made = await page.request.post(`${origin}/settings/api-keys`, {
    data: {},
    headers: { Origin: origin },
  });
  expect(made.status(), "POST /settings/api-keys").toBe(201);
  const { key } = (await made.json()) as { key: string };
  let session: string | null = null;
  let id = 0;
  const send = async (method: string, params?: object) => {
    const notification = method.startsWith("notifications/");
    const response = await fetch(`${origin}/mcp`, {
      method: "POST",
      headers: {
        Authorization: `Bearer ${key}`,
        "Content-Type": "application/json",
        Accept: "application/json, text/event-stream",
        ...(session && { "Mcp-Session-Id": session, "Mcp-Protocol-Version": "2025-06-18" }),
      },
      body: JSON.stringify({ jsonrpc: "2.0", method, params, ...(!notification && { id: ++id }) }),
    });
    session ??= response.headers.get("mcp-session-id");
    if (notification) return undefined;
    // The server may keep an event stream open after its answer: read to the first one.
    const reader = response.body!.getReader();
    const decoder = new TextDecoder();
    for (let text = ""; ; ) {
      const { value, done } = await reader.read();
      text += decoder.decode(value, { stream: !done });
      const answer = /^(?:data: *)?(\{.*\})\s*$/m.exec(text)?.[1];
      if (answer && (done || text.endsWith("\n"))) {
        await reader.cancel();
        const message = JSON.parse(answer);
        if (message.error) {
          throw new Error(`MCP ${method} answered error ${message.error.code}: ${message.error.message}`);
        }
        return message.result;
      }
      if (done) throw new Error(`MCP ${method} answered ${response.status} with no result`);
    }
  };
  await send("initialize", {
    protocolVersion: "2025-06-18",
    capabilities: {},
    clientInfo: { name: "daily-loop", version: "0" },
  });
  await send("notifications/initialized");
  return {
    /** Calls a tool and returns the JSON it answers with; a tool error throws its text. */
    call: async (name: string, args: object) => {
      const { content, isError } = await send("tools/call", { name, arguments: args });
      if (isError) throw new Error(`${name}: ${content[0].text}`);
      return JSON.parse(content[0].text);
    },
  };
}

/** Signs in and opens the first Build in the Builds list, on the named tab. */
async function openBuild(page: Page, tab = "Overview") {
  await signIn(page);
  await go(page, "Builds");
  await page.getByRole("table", { name: "Builds" }).getByRole("link").first().click();
  await page.getByRole("link", { name: tab, exact: true }).click();
}

step(1, null, async ({ page }) => {
  await openSignInLink(page);
  await page.getByLabel("Email").fill(OWNER.email);
  await page.getByLabel("Password", { exact: true }).fill(OWNER.password);
  await page.getByLabel("Password again").fill(OWNER.password);
  await page.getByRole("button", { name: "Create account" }).click();
  await expect(screen(page, "Today")).toBeVisible();

  await page.getByRole("button", { name: "Sign out" }).click();
  await expect(screen(page, "Sign in")).toBeVisible();
  // Signed out, he asks for Builds; the sign-in screen takes him there. The browser
  // takes a new sign-in link. Each try starts from a browser with no cookies and runs
  // again if a restart of pp-server cuts it off.
  await expect(async () => {
    await page.context().clearCookies();
    await openSignInLink(page);
    await page.goto(new URL("/builds", page.url()).href);
    await fillSignIn(page);
    await expect(screen(page, "Builds")).toBeVisible();
  }).toPass({ timeout: 120_000 });
});

// GRE-1515 writes this step: the import is to write the synthetic installation (22 Builds
// with accepted Plans, 10 Sources, 2 printers) into a new data folder, and the server is to
// start on it. Steps 6 and 10 use its Builds.
step(2, "GRE-1515", async ({ page }) => {
  await serverReady();
  const imported = inContainer(
    "pp-server",
    "import",
    ...["--from", "/tmp/node-installation", "--data", "/tmp/imported"],
  );
  expect(imported.status, imported.stderr).toBe(0);
  await signIn(page);
  await go(page, "Builds");
  unwritten(
    "the synthetic installation is imported and the server restarted on it: its Builds, accepted Plans, Plates, Checkoff history, printers and Sources open, Plan, Plate and Required-unit IDs match, and the original is unchanged",
  );
});

// The Moonraker simulator run.mjs starts beside the container (web/test/moonraker-sim).
// The name is not one the synthetic installation's printers use.
const PRINTER = "Daily loop simulator";

/** What the simulator's control port reports: its state and request counts, never the key. */
async function simulatorLog() {
  const simulator = process.env.PP_DAILY_LOOP_SIMULATOR ?? "";
  const control = docker("port", simulator, "7200").stdout.trim().split("\n")[0];
  expect(control, `docker port ${simulator} 7200`).toMatch(/^127\.0\.0\.1:\d+$/);
  const answer = await fetch(`http://${control}/_sim/log`);
  expect(answer.status, "GET /_sim/log").toBe(200);
  return (await answer.json()) as {
    state: string;
    counts: { requests: number; unauthorized: number };
  };
}

step(3, null, async ({ page }) => {
  await signIn(page);
  await go(page, "Settings");
  // Farm is off on a new installation; the owner turns it on to see his printers there.
  await page.getByRole("button", { name: "Turn Farm on" }).click();
  await page.getByRole("button", { name: "Confirm Module change" }).click();
  await expect(page.getByRole("button", { name: "Turn Farm off" })).toBeVisible();
  await page.getByRole("button", { name: "Add a printer" }).click();
  const dialog = page.getByRole("dialog", { name: "Add a printer" });
  await dialog.getByLabel("Kind").selectOption("moonraker");
  await dialog.getByLabel("Address", { exact: true }).fill(process.env.PP_DAILY_LOOP_PRINTER_URL ?? "");
  // The key file is a secret; it goes only into the password field.
  const key = (await readFile(process.env.PP_DAILY_LOOP_PRINTER_KEY_FILE ?? "", "utf8")).trim();
  await dialog.getByLabel("API key (optional)").fill(key);
  // The probe sends no key, so the simulator answers 401 as a Moonraker that needs one.
  await dialog.getByRole("button", { name: "Check address" }).click();
  await expect(dialog.getByRole("status")).toHaveText("The printer answered.");
  await dialog.getByLabel("Name", { exact: true }).fill(PRINTER);
  await dialog.getByLabel("Model", { exact: true }).fill("Moonraker simulator");
  await dialog.getByLabel("Bed width (mm)").fill("220");
  await dialog.getByLabel("Bed depth (mm)").fill("220");
  await dialog.getByLabel("Bed height (mm)").fill("250");
  await dialog.getByRole("button", { name: "Save printer" }).click();
  await expect(dialog).toBeHidden();
  await expect(page.getByRole("status").filter({ hasText: `Added ${PRINTER}.` })).toBeVisible();
  await expect(page.getByRole("button", { name: `Edit ${PRINTER}` })).toBeVisible();

  // Restart the container. Every request after this one must carry the saved key.
  const before = await simulatorLog();
  const restartedAt = Date.now();
  const restarted = docker("restart", CONTAINER);
  expect(restarted.status, restarted.stderr).toBe(0);
  // The published port can change on restart, so sign in afresh on the new address.
  await page.context().clearCookies();
  await signIn(page);
  await go(page, "Farm");

  // The restarted core polls the simulator with the key: new requests, no new 401.
  await expect
    .poll(async () => (await simulatorLog()).counts.requests, { timeout: 60_000 })
    .toBeGreaterThan(before.counts.requests);
  const after = await simulatorLog();
  expect(after.counts.unauthorized, "requests the simulator refused after the restart").toBe(
    before.counts.unauthorized,
  );
  // The simulator is in standby and no one has confirmed its bed clear, so Farm shows it
  // Idle with its bed to clear, heard after the restart, from the live stream rather than a
  // last-known state.
  expect(after.state, "the simulator's state").toBe("standby");
  const row = page
    .getByRole("table", { name: "Printers" })
    .getByRole("row")
    .filter({ has: page.getByRole("rowheader", { name: PRINTER }) });
  await expect(row).toHaveCount(1);
  const cells = row.getByRole("cell");
  await expect(cells.nth(0)).toHaveText("Bed to clear", { timeout: 60_000 });
  await expect(cells.nth(1)).toHaveText("Idle");
  const heard = row.locator("time");
  await expect(heard).toBeVisible();
  const heardAt = Date.parse((await heard.getAttribute("datetime")) ?? "");
  expect(heardAt, "last heard after the restart").toBeGreaterThanOrEqual(restartedAt - 1000);
  await expect(page.getByText("Live printer updates are down")).toHaveCount(0);
  await expect(page.getByText("Connecting to live printer updates.")).toHaveCount(0);
});

/**
 * A ZIP of `entries`, stored without compression and dated 1980-01-01, so each run uploads
 * the same bytes. The kit comes from a ZIP rather than GitHub, so the step needs no network.
 */
function zipOf(entries: Record<string, string>): Buffer {
  const locals: Buffer[] = [];
  const centrals: Buffer[] = [];
  let offset = 0;
  for (const [path, text] of Object.entries(entries)) {
    const name = Buffer.from(path);
    const data = Buffer.from(text);
    // Version 2.0, no flags, stored, time 00:00, date 1980-01-01, CRC-32, sizes, name length.
    const fields = Buffer.alloc(26);
    fields.writeUInt16LE(20, 0);
    fields.writeUInt16LE(0x21, 8);
    fields.writeUInt32LE(crc32(data), 10);
    fields.writeUInt32LE(data.length, 14);
    fields.writeUInt32LE(data.length, 18);
    fields.writeUInt16LE(name.length, 22);
    const local = Buffer.concat([u32(0x04034b50), fields, name, data]);
    // The central record: made-by version, the same fields, no comment or attributes, then
    // the local header's offset.
    const tail = Buffer.alloc(14);
    tail.writeUInt32LE(offset, 10);
    centrals.push(Buffer.concat([u32(0x02014b50), u16(20), fields, tail, name]));
    locals.push(local);
    offset += local.length;
  }
  const central = Buffer.concat(centrals);
  const end = Buffer.alloc(18);
  end.writeUInt16LE(centrals.length, 4);
  end.writeUInt16LE(centrals.length, 6);
  end.writeUInt32LE(central.length, 8);
  end.writeUInt32LE(offset, 12);
  return Buffer.concat([...locals, central, u32(0x06054b50), end]);
}

function u16(value: number) {
  const bytes = Buffer.alloc(2);
  bytes.writeUInt16LE(value);
  return bytes;
}

function u32(value: number) {
  const bytes = Buffer.alloc(4);
  bytes.writeUInt32LE(value);
  return bytes;
}

/** An ASCII STL of a 10 mm tetrahedron: enough geometry to render a thumbnail. */
function tetrahedron(name: string) {
  const facets = [
    ["0 0 -1", "0 0 0", "0 10 0", "10 0 0"],
    ["0 -1 0", "0 0 0", "10 0 0", "0 0 10"],
    ["-1 0 0", "0 0 0", "0 0 10", "0 10 0"],
    ["0.577 0.577 0.577", "10 0 0", "0 10 0", "0 0 10"],
  ].map(
    ([normal, ...corners]) =>
      ` facet normal ${normal}\n  outer loop\n${corners.map((corner) => `   vertex ${corner}\n`).join("")}  endloop\n endfacet\n`,
  );
  return `solid ${name}\n${facets.join("")}endsolid ${name}\n`;
}

const KIT_SOURCE = "Daily loop kit files";
const KIT_V1 = {
  "body/frame.stl": tetrahedron("frame"),
  "arms/left.stl": tetrahedron("left"),
  "README.txt": "Daily-loop test kit.\n",
};
// The second upload adds a part, so its files tell the two revisions apart.
const KIT_V2 = { ...KIT_V1, "arms/right.stl": tetrahedron("right") };

/**
 * Starts recording each text the Sync cell of the kit's row shows to a screen reader while
 * it is busy. Uploading and importing can end between two polls of an assertion, so
 * `seenProgress` reads the record instead of the screen.
 */
async function recordProgress(page: Page) {
  await page.getByRole("table", { name: "Sources" }).evaluate((table, source) => {
    const seen: string[] = [];
    Object.assign(window, { ppSeenProgress: seen });
    const record = () => {
      for (const status of table.querySelectorAll('tbody [role="status"]')) {
        const row = status.closest("tr")?.querySelector("th a")?.textContent;
        if (row === source) seen.push(status.textContent ?? "");
      }
    };
    new MutationObserver(record).observe(table, {
      subtree: true,
      childList: true,
      characterData: true,
      attributes: true,
    });
  }, KIT_SOURCE);
}

const seenProgress = (page: Page) =>
  page.evaluate(() => (window as unknown as { ppSeenProgress: string[] }).ppSeenProgress);

const kitRow = (page: Page) =>
  page
    .getByRole("table", { name: "Sources" })
    .getByRole("row")
    .filter({ has: page.getByRole("link", { name: KIT_SOURCE, exact: true }) });

/** Uploads `files` as a ZIP on the kit's Library row and returns the job the core accepted. */
async function uploadKit(page: Page, files: Record<string, string>) {
  const accepted = page.waitForResponse(
    (response) =>
      response.request().method() === "POST" && /\/sources\/\d+\/upload-zip$/.test(response.url()),
  );
  await kitRow(page)
    .getByLabel(`Upload ZIP for ${KIT_SOURCE}`)
    .setInputFiles({ name: "daily-loop-kit.zip", mimeType: "application/zip", buffer: zipOf(files) });
  const answer = await accepted;
  expect(answer.status(), "POST upload-zip").toBe(202);
}

/** Waits for the kit's row to finish its import and returns its revision text. */
async function importedRevision(page: Page) {
  const row = kitRow(page);
  await expect(row.getByLabel(`Upload ZIP for ${KIT_SOURCE}`)).toBeEnabled({ timeout: 60_000 });
  await expect(row).not.toContainText(/Import failed|No revision yet|No files on disk/);
  const revision = row.getByText(/^Revision \d+$/);
  await expect(revision).toBeVisible();
  return revision.textContent();
}

/** Opens the kit's files in Library and checks each file is listed and each STL has a thumbnail. */
async function expectKitFiles(page: Page, files: Record<string, string>) {
  await kitRow(page).getByRole("link", { name: KIT_SOURCE, exact: true }).click();
  const panel = page.getByRole("region", { name: `Files for ${KIT_SOURCE}` });
  for (const path of Object.keys(files)) {
    const row = panel.getByRole("listitem").filter({ hasText: path.split("/").pop() });
    await expect(row).toHaveCount(1);
  }
  const stls = Object.keys(files).filter((path) => path.endsWith(".stl")).length;
  // The panel is busy until every thumbnail in view has loaded or failed.
  await expect(panel).not.toHaveAttribute("aria-busy", "true", { timeout: 30_000 });
  await expect(panel.getByRole("img", { name: "No thumbnail" })).toHaveCount(0);
  const thumbnails = panel.locator(".pp-stl-thumb img");
  await expect(thumbnails).toHaveCount(stls);
  for (const thumbnail of await thumbnails.all()) {
    const width = await thumbnail.evaluate((image: HTMLImageElement) => image.naturalWidth);
    expect(width, "thumbnail width").toBeGreaterThan(0);
  }
}

step(4, null, async ({ page }) => {
  await signIn(page);
  await go(page, "Library");
  await page.getByRole("button", { name: "Add Source" }).click();
  const dialog = page.getByRole("dialog", { name: "Add Source" });
  await dialog.getByLabel("Name").fill(KIT_SOURCE);
  await dialog.getByRole("radio", { name: "ZIP archive" }).check();
  await dialog.getByRole("button", { name: "Create" }).click();
  await expect(dialog).toBeHidden();
  await expect(kitRow(page)).toContainText("No revision yet");

  // The first upload: the row shows its progress, then the files and thumbnails show.
  await recordProgress(page);
  await uploadKit(page, KIT_V1);
  const first = await importedRevision(page);
  expect(await seenProgress(page)).toContainEqual(expect.stringMatching(/^(Uploading|Importing)/));
  await expectKitFiles(page, KIT_V1);

  // The second upload, then a restart of the container as soon as the core has accepted it.
  // The import may finish before the stop or be settled on the next start; either way the
  // new revision must land, and the row must not stay busy or show a failed import.
  await uploadKit(page, KIT_V2);
  const restarted = docker("restart", CONTAINER);
  expect(restarted.status, restarted.stderr).toBe(0);
  // The published port can change on restart, so sign in afresh on the new address.
  await page.context().clearCookies();
  await signIn(page);
  await go(page, "Library");
  const second = await importedRevision(page);
  expect(second, "the revision after the restart").not.toBe(first);
  await expectKitFiles(page, KIT_V2);
});

// The one printable file of the Build Step 5 creates. Step 7 finds these bytes in the pack.
const STL = "bracket.stl";
const BRACKET =
  "solid bracket\nfacet normal 0 0 1\nouter loop\nvertex 0 0 0\nvertex 1 0 0\nvertex 0 1 0\nendloop\nendfacet\nendsolid bracket\n";

// The owner creates Builds through the assistant, so this step goes over MCP.
step(5, null, async ({ page }) => {
  await signIn(page);
  const origin = new URL(page.url()).origin;
  // The kit's printable files, as a ZIP in a new Library Source. Step 4 tests the Library's
  // upload screen; this step uploads through the same routes so it does not depend on it.
  const KIT = "Daily loop kit parts";
  const made = await page.request.post(`${origin}/api/v1/sources`, {
    data: { name: KIT, source_kind: "archive" },
    headers: { Origin: origin },
  });
  expect(made.ok(), `POST /api/v1/sources: ${made.status()} ${await made.text()}`).toBe(true);
  const kit = (await made.json()) as { id: number };
  const upload = await page.request.post(`${origin}/api/v1/sources/${kit.id}/upload-zip`, {
    multipart: {
      file: {
        name: "kit.zip",
        mimeType: "application/zip",
        buffer: storedZip(STL, BRACKET),
      },
    },
    headers: { Origin: origin },
  });
  expect(upload.status(), `upload-zip: ${await upload.text()}`).toBe(202);
  const { job_id } = (await upload.json()) as { job_id: string };
  const job = async () =>
    (await (await page.request.get(`${origin}/api/v1/jobs/${job_id}`)).json()) as {
      status: string;
    };
  const running = /^(pending|running)$/;
  await expect.poll(async () => (await job()).status, { timeout: 60_000 }).not.toMatch(running);
  const unpacked = await job();
  expect(unpacked.status, JSON.stringify(unpacked)).toBe("done");

  // The assistant refuses to create a Build while the Orders Module is off, which it is
  // on a new installation, and tells the owner to turn it on in Settings.
  await go(page, "Settings");
  await page.getByRole("button", { name: "Turn Orders on" }).click();
  await page.getByRole("button", { name: "Confirm Module change" }).click();
  await expect(page.getByRole("button", { name: "Turn Orders off" })).toBeVisible();
  const assistant = await connectAssistant(page);
  /** Proposes a write and confirms it, as the owner does when the assistant asks. */
  const apply = async (tool: string, args: object) => {
    const proposed = await assistant.call(tool, args);
    expect(proposed.status, JSON.stringify(proposed)).toBe("proposed");
    const applied = await assistant.call("confirm_apply", { action_id: proposed.action.id });
    expect(applied.ok, JSON.stringify(applied)).toBe(true);
    return applied.result;
  };
  const request = "One kit for the daily-loop test.";
  const { plan_id } = await apply("propose_create_build", { name: BUILD, request });
  await go(page, "Builds");
  const builds = page.getByRole("table", { name: "Builds" });
  await expect(builds.getByRole("link", { name: BUILD })).toBeVisible();

  // Its Sources: the uploaded kit and the kit's repository. The repository is a stand-in
  // that never syncs, so it stays reference evidence and the uploaded kit is the base.
  const inputs = [{ source_id: kit.id }, { url: KIT_URL }];
  const imported = await apply("propose_import_build_inputs", { plan_id, inputs });
  const [base, repository] = imported.evidence as { id: string }[];
  const roles = [
    { evidence_id: base.id, role: "structural_base" },
    { evidence_id: repository.id, role: "evidence" },
  ];
  await apply("propose_set_build_source_roles", { plan_id, roles });
  const rebuilt = await apply("propose_rebuild_plan", { plan_id });
  expect(rebuilt.recompute, JSON.stringify(rebuilt)).toBe("created");

  // The Build's Sources tab shows the kit as its base layer, and its Plan tab the draft.
  await builds.getByRole("link", { name: BUILD }).click();
  await page.getByRole("link", { name: "Sources", exact: true }).click();
  const layers = page.getByRole("table", { name: "Build Sources" });
  await expect(layers.getByRole("row").filter({ hasText: KIT })).toContainText("base");
  await page.getByRole("link", { name: "Plan", exact: true }).click();
  await expect(page.getByRole("checkbox", { name: `Include ${STL}` })).toBeChecked();
  await expect(page.getByRole("button", { name: "Review and accept" })).toBeVisible();
});

/** A ZIP that stores one file uncompressed: enough for the Source import to unpack. */
function storedZip(name: string, text: string) {
  const data = Buffer.from(text);
  const path = Buffer.from(name);
  let crc = ~0;
  for (const byte of data) {
    crc ^= byte;
    for (let bit = 0; bit < 8; bit++) crc = (crc >>> 1) ^ (0xedb88320 & -(crc & 1));
  }
  crc = ~crc >>> 0;
  // Version 2.0, no flags, stored, 1980-01-01 00:00.
  const common = (header: Buffer, at: number) => {
    header.writeUInt16LE(20, at);
    header.writeUInt16LE(0, at + 2);
    header.writeUInt16LE(0, at + 4);
    header.writeUInt16LE(0, at + 6);
    header.writeUInt16LE(0x21, at + 8);
    header.writeUInt32LE(crc, at + 10);
    header.writeUInt32LE(data.length, at + 14);
    header.writeUInt32LE(data.length, at + 18);
    header.writeUInt16LE(path.length, at + 22);
  };
  const local = Buffer.alloc(30);
  local.writeUInt32LE(0x04034b50, 0);
  common(local, 4);
  const central = Buffer.alloc(46);
  central.writeUInt32LE(0x02014b50, 0);
  central.writeUInt16LE(20, 4);
  common(central, 6);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(1, 8);
  end.writeUInt16LE(1, 10);
  end.writeUInt32LE(central.length + path.length, 12);
  end.writeUInt32LE(local.length + path.length + data.length, 16);
  return Buffer.concat([local, path, data, central, path, end]);
}

step(6, "GRE-1515", async ({ page }) => {
  await openBuild(page, "Plan");
  const quantity = page.getByRole("textbox", { name: /^Quantity of / }).first();
  await quantity.fill("2");
  await quantity.press("Enter");
  await page.getByRole("button", { name: "Review and accept" }).click();
  const gate = page.getByRole("dialog", { name: "Accept this draft" });
  await gate.getByRole("button", { name: "Accept" }).click();
  await expect(page.getByText("Accepted. The Plan is now version")).toBeVisible();
  unwritten("Required units keep their IDs across a second accepted revision");
});

/** Clicks `button` and returns the file the browser saved, read back from disk. */
async function downloaded(page: Page, button: Locator) {
  const [download] = await Promise.all([
    page.waitForEvent("download", { timeout: 30_000 }),
    button.click(),
  ]);
  expect(await download.failure(), "download failure").toBeNull();
  const body = await readFile(await download.path());
  expect(body.length, `${download.suggestedFilename()} size`).toBeGreaterThan(0);
  return { name: download.suggestedFilename(), body };
}

/**
 * The entries of a ZIP, read from its central directory: name and bytes, each checked
 * against its CRC-32. The pack stores its entries; deflate is read too, in case it changes.
 */
function unzip(zip: Buffer): { name: string; data: Buffer }[] {
  const end = zip.lastIndexOf(u32(0x06054b50));
  expect(zip.readUInt32LE(0), "ZIP local header signature").toBe(0x04034b50);
  expect(end, "ZIP end of central directory").toBeGreaterThan(0);
  const count = zip.readUInt16LE(end + 10);
  const entries = [];
  for (let at = zip.readUInt32LE(end + 16), index = 0; index < count; index++) {
    expect(zip.readUInt32LE(at), "ZIP central header signature").toBe(0x02014b50);
    const [method, crc, packed, size] = [
      zip.readUInt16LE(at + 10),
      zip.readUInt32LE(at + 16),
      zip.readUInt32LE(at + 20),
      zip.readUInt32LE(at + 24),
    ];
    const nameLength = zip.readUInt16LE(at + 28);
    const name = zip.toString("utf8", at + 46, at + 46 + nameLength);
    const local = zip.readUInt32LE(at + 42);
    const start = local + 30 + zip.readUInt16LE(local + 26) + zip.readUInt16LE(local + 28);
    const raw = zip.subarray(start, start + packed);
    expect([0, 8], `${name} compression method`).toContain(method);
    const data = method === 8 ? inflateRawSync(raw) : raw;
    expect(data.length, `${name} size`).toBe(size);
    expect(crc32(data), `${name} CRC-32`).toBe(crc);
    entries.push({ name, data });
    at += 46 + nameLength + zip.readUInt16LE(at + 30) + zip.readUInt16LE(at + 32);
  }
  return entries;
}

step(7, null, async ({ page }) => {
  await openBuild(page, "Production");
  await expect(screen(page, "Production")).toBeVisible();
  // What the Build says its accepted Plan is: Step 6 accepted bracket.stl at quantity 2.
  const id = /\/builds\/(\d+)\//.exec(page.url())?.[1];
  const workflow = await page.request.get(new URL(`/api/v2/plans/${id}/workflow`, page.url()).href);
  expect(workflow.ok(), `GET workflow: ${workflow.status()}`).toBe(true);
  const { accepted_plan: accepted } = (await workflow.json()) as {
    accepted_plan: { kind: string; plan_version: number; total_units: number };
  };
  expect(accepted.kind, JSON.stringify(accepted)).toBe("ready");
  expect(accepted.total_units, "accepted units").toBe(2);
  const version = accepted.plan_version;
  const slug = BUILD.toLowerCase().replaceAll(" ", "-");
  const downloads = page.getByRole("region", { name: "Downloads" });

  // The STL pack: one copy of bracket.stl per Required unit, byte for byte the Source file.
  const pack = await downloaded(page, downloads.getByRole("button", { name: "Download STL pack" }));
  expect(pack.name).toBe(`${slug}-v${version}-stl.zip`);
  const entries = unzip(pack.body);
  const stem = STL.replace(/\.stl$/, "");
  const names = entries.map(({ name }) => name.split("/").pop()).sort();
  expect(names).toEqual(
    Array.from(
      { length: accepted.total_units },
      (_, unit) => `${stem}_${String(unit + 1).padStart(2, "0")}.stl`,
    ),
  );
  for (const { name, data } of entries) {
    expect(name, "entry path").toMatch(/^[^/]+\/_root\/[^/]+\.stl$/);
    expect(data.toString("utf8"), `${name} bytes`).toBe(BRACKET);
  }

  // The checklist, opened as the owner opens the saved file: the Build, the accepted
  // version, and bracket.stl at quantity 2 with none printed.
  const checklist = await downloaded(
    page,
    downloads.getByRole("button", { name: "Download checklist" }),
  );
  expect(checklist.name).toBe(`${slug}-v${version}-checklist.html`);
  const sheet = await page.context().newPage();
  try {
    await sheet.setContent(checklist.body.toString("utf8"));
    await expect(sheet).toHaveTitle(`${BUILD} checklist, Plan version ${version}`);
    await expect(sheet.getByRole("heading", { level: 1 })).toHaveText(BUILD);
    // Each fact is a term and the definition after it; an Order number line is optional.
    const fact = (term: string) => sheet.locator(`dt:text-is("${term}") + dd`);
    await expect(fact("Plan version")).toHaveText(String(version));
    await expect(fact("Printed")).toHaveText(`0 of ${accepted.total_units} units`);
    const rows = sheet
      .getByRole("table")
      .getByRole("row")
      .filter({ has: sheet.getByRole("cell") });
    await expect(rows).toHaveCount(1);
    await expect(rows.getByRole("cell")).toHaveText([STL, /.*/, /.*/, "2", "0 / 2", /.*/]);
  } finally {
    await sheet.close();
  }
});

/** What the simulator's control port reports that Steps 8 and 9 read. */
type SimulatorLog = {
  state: string;
  progress: number;
  filename: string | null;
  counts: { requests: number; unauthorized: number; uploads: number; starts: number };
  files: { name: string; size: number; sha256: string; uploads: number }[];
  fault: string | null;
};

/**
 * One request to the simulator's control port: `log` reads it; `state` and `fault` change
 * it with `body`. Each answers with the log after the request.
 */
async function simulator(path: "log" | "state" | "fault", body?: object) {
  const name = process.env.PP_DAILY_LOOP_SIMULATOR ?? "";
  const control = docker("port", name, "7200").stdout.trim().split("\n")[0];
  expect(control, `docker port ${name} 7200`).toMatch(/^127\.0\.0\.1:\d+$/);
  const answer = await fetch(
    `http://${control}/_sim/${path}`,
    body && {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    },
  );
  expect(answer.status, `/_sim/${path}: ${body ? JSON.stringify(body) : ""}`).toBe(200);
  return (await answer.json()) as SimulatorLog;
}

// The sliced file the owner sends in Step 8, and prints in Step 9. Its first send's answer
// is lost; that send has its own name so the simulator counts each file's uploads apart.
const GCODE = "; Daily loop bracket, sliced\nG28\nG1 Z0.2 F3000\nG1 X10 Y10 E1 F1500\nM84\n";
const SENT = "bracket.gcode";
const LOST = "bracket-lost-answer.gcode";

/** A send's row on the Production tab, by its file name. */
const sendRow = (page: Page, file: string) =>
  page
    .getByRole("table", { name: "Sends" })
    .getByRole("row")
    .filter({ has: page.getByRole("cell", { name: file, exact: true }) });

/** Sends `file` to the simulator from the Production tab's sheet: upload, then start. */
async function uploadAndStart(page: Page, file: string) {
  await page.getByRole("button", { name: "Send a file" }).click();
  const sheet = page.getByRole("dialog", { name: "Send a file" });
  await sheet
    .getByLabel("Sliced file")
    .setInputFiles({ name: file, mimeType: "text/x-gcode", buffer: Buffer.from(GCODE) });
  await sheet.getByLabel("Printer", { exact: true }).selectOption({ label: PRINTER });
  await sheet.getByRole("radio", { name: "Upload and start print" }).check();
  const send = sheet.getByRole("button", { name: "Send", exact: true });
  await expect(send).toBeEnabled();
  await send.click();
  const confirm = page.getByRole("dialog", { name: "Start this print?" });
  await expect(confirm).toContainText(`uploads ${file} to ${PRINTER} and starts the print`);
  await confirm.getByRole("button", { name: "Start print" }).click();
  await expect(confirm).toBeHidden();
}

step(8, null, async ({ page }) => {
  await openBuild(page, "Production");
  await expect(screen(page, "Production")).toBeVisible();
  const before = await simulator("log");
  expect(before.state, "the simulator's state").toBe("standby");
  expect(before.fault, "the simulator's armed fault").toBeNull();

  // The first send's answer is lost: the simulator stores the upload, then closes the
  // connection without answering, so the core cannot know whether the printer has it.
  await simulator("fault", { fault: "lose-upload" });
  await uploadAndStart(page, LOST);
  const lost = sendRow(page, LOST);
  await expect(lost.getByRole("cell").nth(4)).toHaveText("Outcome unknown", { timeout: 60_000 });
  // The row asks the owner what happened and offers no Send now.
  await expect(
    lost.getByRole("group", { name: `Actions for ${LOST}` }).getByRole("button"),
  ).toHaveText(["It printed", "It did not print", "Remove"]);
  const unknown = await simulator("log");
  expect(unknown.fault, "the armed fault, after it fired").toBeNull();
  expect(unknown.counts.uploads, "uploads").toBe(before.counts.uploads + 1);
  expect(unknown.counts.starts, "print starts").toBe(before.counts.starts);
  expect(unknown.state, "the simulator's state").toBe("standby");
  // The core refuses Send now itself while the outcome is unknown, so nothing resends it.
  const origin = new URL(page.url()).origin;
  const queue = await page.request.get(`${origin}/api/v2/printer-send-queue`);
  expect(queue.ok(), `GET printer-send-queue: ${queue.status()}`).toBe(true);
  const { items } = (await queue.json()) as { items: { id: string; filename: string }[] };
  const item = items.find(({ filename }) => filename === LOST);
  expect(item, `${LOST} in the send queue`).toBeDefined();
  const again = await page.request.post(
    `${origin}/api/v2/printer-send-queue/${item?.id}/dispatch`,
    { headers: { Origin: origin } },
  );
  expect(again.status(), `Send now on an unknown outcome: ${await again.text()}`).toBe(409);
  expect((await simulator("log")).counts, "counts after the refused Send now").toEqual(
    unknown.counts,
  );

  // The owner sees the simulator idle, not printing, and says so.
  await lost.getByRole("button", { name: "It did not print" }).click();
  await expect(lost.getByRole("cell").nth(4)).toContainText("Failed");
  await expect(
    lost.getByRole("group", { name: `Actions for ${LOST}` }).getByRole("button"),
  ).toHaveText(["Send now", "Remove"]);

  // Then he sends the file again. The printer receives it once and starts it once. This
  // dispatch also wakes the send worker, which must leave the reconciled send alone.
  await uploadAndStart(page, SENT);
  await expect(sendRow(page, SENT).getByRole("cell").nth(4)).toHaveText("Sent", {
    timeout: 60_000,
  });
  await expect(lost.getByRole("cell").nth(4)).toContainText("Failed");
  const sent = await simulator("log");
  expect(sent.counts.uploads, "uploads").toBe(unknown.counts.uploads + 1);
  expect(sent.counts.starts, "print starts").toBe(unknown.counts.starts + 1);
  expect(sent.state, "the simulator's state").toBe("printing");
  expect(sent.filename, "the file printing").toBe(SENT);
  const sha256 = Buffer.from(
    await crypto.subtle.digest("SHA-256", Buffer.from(GCODE)),
  ).toString("hex");
  const files = Object.fromEntries(sent.files.map(({ name, ...file }) => [name, file]));
  expect(files[SENT], `${SENT} on the simulator`).toEqual({
    size: Buffer.byteLength(GCODE),
    sha256,
    uploads: 1,
  });
  expect(files[LOST]?.uploads, `uploads of ${LOST}`).toBe(1);
});

/** The pulse line's count of printers in the state `label`; 0 when it lists none there. */
async function pulseCount(page: Page, label: string) {
  const line = page.getByRole("region", { name: "Printer pulse" });
  await expect(line.getByRole("list")).toBeVisible();
  const names = await line
    .getByRole("listitem")
    .evaluateAll((items) => items.map((item) => item.getAttribute("aria-label") ?? ""));
  const count = names.map((name) => /^(\d+) (.+)$/.exec(name)).find((match) => match?.[2] === label);
  return count ? Number(count[1]) : 0;
}

step(9, null, async ({ page }) => {
  // Step 8 started bracket.gcode on the simulator. The owner keeps Today open in this tab
  // and Farm in a second one while the print runs to its end.
  expect((await simulator("log")).state, "the simulator's state").toBe("printing");
  await signIn(page);
  const farm = await page.context().newPage();
  try {
    await farm.goto(page.url());
    await go(farm, "Farm");
    const row = farm
      .getByRole("table", { name: "Printers" })
      .getByRole("row")
      .filter({ has: farm.getByRole("rowheader", { name: PRINTER }) });
    const [status, execution] = [row.getByRole("cell").nth(0), row.getByRole("cell").nth(1)];
    // A mark on each tab that a page reload would wipe.
    for (const tab of [page, farm]) {
      await tab.evaluate(() => Object.assign(window, { ppNotReloaded: true }));
    }

    await simulator("state", { state: "printing", progress: 0, filename: SENT });
    await expect(status).toHaveText("Printing", { timeout: 30_000 });
    await expect(execution).toHaveText("Printing 0%", { timeout: 30_000 });
    const printing = await pulseCount(page, "Printing");
    expect(printing, "printers Today counts as printing").toBeGreaterThanOrEqual(1);
    const bedsToClear = await pulseCount(page, "Bed to clear");

    await simulator("state", { state: "printing", progress: 0.4 });
    await expect(execution).toHaveText("Printing 40%", { timeout: 30_000 });
    await expect(status).toHaveText("Printing");

    // The print ends. The bed holds it, so Farm and Today both show it to clear.
    await simulator("state", { state: "complete", progress: 1 });
    await expect(status).toHaveText("Bed to clear", { timeout: 30_000 });
    await expect(execution).toHaveText(/^Idle/);
    await expect.poll(() => pulseCount(page, "Printing"), { timeout: 30_000 }).toBe(printing - 1);
    expect(await pulseCount(page, "Bed to clear"), "printers Today counts with a bed to clear").toBe(
      bedsToClear + 1,
    );

    for (const tab of [page, farm]) {
      expect(await tab.evaluate(() => "ppNotReloaded" in window), "no page reload").toBe(true);
    }
    await expect(screen(page, "Today")).toBeVisible();
    await expect(farm.getByText("Live printer updates are down")).toHaveCount(0);
  } finally {
    await farm.close();
  }
});

step(10, "GRE-1515", async ({ page }) => {
  await openBuild(page, "Checkoff");
  await page.getByRole("checkbox", { name: / unit 1$/ }).first().click();
  await page.getByRole("button", { name: "Record 1 picked unit" }).click();
  const tally = page.getByRole("status", { name: "Checkoff tally" });
  await expect(tally).toContainText("Recorded: +1 printed.");
  await tally.getByRole("button", { name: "Undo" }).click();
  await expect(tally).toContainText("Undid the last burst.");
  // Until Step 2 imports Builds, the first Build is the one Step 5 created, with no print.
  unwritten("the checked-off units are on a Build Step 2 imported, from the print Step 8 sent");
});

step(11, "GRE-1887", async ({ page }) => {
  await openBuild(page);
  await expect(page.getByText(/\d+ of \d+ units printed/)).toBeVisible();
  unwritten(
    "Today shows what is left, Spoolman shortfalls are current, and a deduction is recorded once",
  );
});

step(12, "GRE-1888", async ({ page }) => {
  await signIn(page);
  await go(page, "Settings");
  await page.getByRole("button", { name: "Create backup", exact: true }).click();
  unwritten("the backup restores on a second container, which opens with the same Builds and history");
});
