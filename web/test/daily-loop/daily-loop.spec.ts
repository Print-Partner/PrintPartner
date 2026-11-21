import { spawnSync } from "node:child_process";
import process from "node:process";
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

step(4, "GRE-1884", async ({ page }) => {
  await signIn(page);
  await go(page, "Library");
  await page.getByRole("button", { name: /add (a )?source|upload/i }).click();
  unwritten(
    "an uploaded ZIP's files and thumbnails appear in Library, and sync shows progress and resumes after a restart",
  );
});

// The owner creates Builds through the assistant, so this step goes over MCP.
step(5, null, async ({ page }) => {
  await signIn(page);
  const origin = new URL(page.url()).origin;
  // The kit's printable files, as a ZIP in a new Library Source. Step 4 tests the Library's
  // upload screen; this step uploads through the same routes so it does not depend on it.
  const KIT = "Daily loop kit parts";
  const STL = "bracket.stl";
  const made = await page.request.post(`${origin}/api/v1/sources`, {
    data: { name: KIT, source_kind: "archive" },
    headers: { Origin: origin },
  });
  expect(made.ok(), `POST /api/v1/sources: ${made.status()} ${await made.text()}`).toBe(true);
  const kit = (await made.json()) as { id: number };
  const facet =
    "facet normal 0 0 1\nouter loop\nvertex 0 0 0\nvertex 1 0 0\nvertex 0 1 0\nendloop\nendfacet";
  const upload = await page.request.post(`${origin}/api/v1/sources/${kit.id}/upload-zip`, {
    multipart: {
      file: {
        name: "kit.zip",
        mimeType: "application/zip",
        buffer: storedZip(STL, `solid bracket\n${facet}\nendsolid bracket\n`),
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
