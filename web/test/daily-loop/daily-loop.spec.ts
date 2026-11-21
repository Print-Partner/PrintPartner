import { Buffer } from "node:buffer";
import { spawnSync } from "node:child_process";
import process from "node:process";
import { crc32 } from "node:zlib";
import { expect, test, type Page } from "@playwright/test";
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

step(3, "GRE-1531", async ({ page }) => {
  await signIn(page);
  await go(page, "Settings");
  await page.getByRole("button", { name: /add (a )?printer/i }).click();
  unwritten(
    "a simulated Moonraker or PrusaLink printer passes the probe, is saved, and shows live status on Farm after a restart",
  );
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

// The owner creates Builds through the assistant, so this step goes over MCP.
step(5, "GRE-1885", async ({ page }) => {
  await signIn(page);
  // The assistant refuses to create a Build while the Orders Module is off, which it is
  // on a new installation, and tells the owner to turn it on in Settings.
  await go(page, "Settings");
  await page.getByRole("button", { name: "Turn Orders on" }).click();
  await page.getByRole("button", { name: "Confirm Module change" }).click();
  await expect(page.getByRole("button", { name: "Turn Orders off" })).toBeVisible();
  const assistant = await connectAssistant(page);
  const request = "One kit for the daily-loop test.";
  const proposed = await assistant.call("propose_create_build", { name: BUILD, request });
  expect(proposed.status, JSON.stringify(proposed)).toBe("proposed");
  const applied = await assistant.call("confirm_apply", { action_id: proposed.action.id });
  expect(applied.ok, JSON.stringify(applied)).toBe(true);
  await go(page, "Builds");
  const builds = page.getByRole("table", { name: "Builds" });
  await expect(builds.getByRole("link", { name: BUILD })).toBeVisible();
  // Its Sources: he gives the assistant the kit's repository.
  const inputs = [{ url: KIT_URL }];
  const plan = applied.result.plan_id;
  const sources = await assistant.call("propose_import_build_inputs", { plan_id: plan, inputs });
  expect(sources.status, JSON.stringify(sources)).toBe("proposed");
  unwritten(
    "the import is confirmed, propose_rebuild_plan saves a first draft Plan (GRE-1434), and the Build's Sources and Plan tabs show both",
  );
});

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

step(7, "GRE-1886", async ({ page }) => {
  await openBuild(page);
  await expect(page.getByText(/STL pack/i)).toBeVisible();
  await expect(page.getByText(/HTML checklist/i)).toBeVisible();
  unwritten("both downloads match the accepted Plan");
});

step(8, "GRE-1531", async ({ page }) => {
  await openBuild(page, "Production");
  await page.getByRole("button", { name: /send/i }).click();
  unwritten(
    "a simulated printer receives the sliced file once, and an unknown outcome asks for a reconcile and is never resent",
  );
});

step(9, "GRE-1531", async ({ page }) => {
  await signIn(page);
  await go(page, "Farm");
  const printers = page.getByRole("table", { name: "Printers" });
  await expect(printers.getByRole("cell", { name: "Printing" })).toBeVisible();
  unwritten("Farm and Today follow the simulated printer to the end of the print without a page reload");
});

step(10, "GRE-1515", async ({ page }) => {
  await openBuild(page, "Checkoff");
  await page.getByRole("checkbox", { name: / unit 1$/ }).first().click();
  await page.getByRole("button", { name: "Record 1 picked unit" }).click();
  const tally = page.getByRole("status", { name: "Checkoff tally" });
  await expect(tally).toContainText("Recorded: +1 printed.");
  await tally.getByRole("button", { name: "Undo" }).click();
  await expect(tally).toContainText("Undid the last burst.");
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
