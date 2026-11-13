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
        return JSON.parse(answer).result;
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
  // Signed out, he asks for Builds; the sign-in screen takes him there. Until GRE-1453,
  // sign-out also stops pp-server: Docker starts it again, perhaps on another host port,
  // and the browser needs a new sign-in link. A server that keeps serving takes one too.
  // Each try starts from a browser with no cookies and runs again if the stop cuts it off.
  await expect(async () => {
    await page.context().clearCookies();
    await openSignInLink(page);
    await page.goto(new URL("/builds", page.url()).href);
    await fillSignIn(page);
    await expect(screen(page, "Builds")).toBeVisible();
  }).toPass({ timeout: 120_000 });
});

// GRE-412's parts land one by one, and the marker names the open one. The import is to
// write the synthetic installation (22 Builds with accepted Plans, 10 Sources, 2 printers)
// into the data volume, and the server is to restart on it. Steps 6 and 10 use its Builds.
step(2, "GRE-1450", async ({ page }) => {
  await serverReady();
  const imported = inContainer("pp-server", "import", "--from", "/tmp/node-installation");
  expect(imported.status, imported.stderr).toBe(0);
  await signIn(page);
  await go(page, "Builds");
  unwritten(
    "the synthetic installation is imported and the server restarted on it: its Builds, accepted Plans, Plates, Checkoff history, printers and Sources open, Plan, Plate and Required-unit IDs match, and the original is unchanged",
  );
});

step(3, "GRE-1431", async ({ page }) => {
  await signIn(page);
  await go(page, "Settings");
  await page.getByRole("button", { name: /add (a )?printer/i }).click();
  unwritten(
    "a simulated Moonraker or PrusaLink printer passes the probe, is saved, and shows live status on Farm after a restart",
  );
});

step(4, "GRE-1365", async ({ page }) => {
  await signIn(page);
  await go(page, "Library");
  await page.getByRole("button", { name: /add (a )?source|upload/i }).click();
  unwritten(
    "an uploaded ZIP's files and thumbnails appear in Library, and sync shows progress and resumes after a restart",
  );
});

// The owner creates Builds through the assistant, so this step goes over MCP.
step(5, "GRE-1300", async ({ page }) => {
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

step(6, "GRE-1450", async ({ page }) => {
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

step(7, "GRE-423", async ({ page }) => {
  await openBuild(page);
  await expect(page.getByText(/STL pack/i)).toBeVisible();
  await expect(page.getByText(/HTML checklist/i)).toBeVisible();
  unwritten("both downloads match the accepted Plan");
});

step(8, "GRE-1103", async ({ page }) => {
  await openBuild(page, "Production");
  await page.getByRole("button", { name: /send/i }).click();
  unwritten(
    "a simulated printer receives the sliced file once, and an unknown outcome asks for a reconcile and is never resent",
  );
});

step(9, "GRE-1103", async ({ page }) => {
  await signIn(page);
  await go(page, "Farm");
  const printers = page.getByRole("table", { name: "Printers" });
  await expect(printers.getByRole("cell", { name: "Printing" })).toBeVisible();
  unwritten("Farm and Today follow the simulated printer to the end of the print without a page reload");
});

step(10, "GRE-1450", async ({ page }) => {
  await openBuild(page, "Checkoff");
  await page.getByRole("checkbox", { name: / unit 1$/ }).first().click();
  await page.getByRole("button", { name: "Record 1 picked unit" }).click();
  const tally = page.getByRole("status", { name: "Checkoff tally" });
  await expect(tally).toContainText("Recorded: +1 printed.");
  await tally.getByRole("button", { name: "Undo" }).click();
  await expect(tally).toContainText("Undid the last burst.");
});

step(11, "GRE-1102", async ({ page }) => {
  await openBuild(page);
  await expect(page.getByText(/\d+ of \d+ units printed/)).toBeVisible();
  unwritten(
    "Today shows what is left, Spoolman shortfalls are current, and a deduction is recorded once",
  );
});

step(12, "GRE-1340", async ({ page }) => {
  await signIn(page);
  await go(page, "Settings");
  await page.getByRole("button", { name: /back ?up/i }).click();
  unwritten("the backup restores on a second container, which opens with the same Builds and history");
});
