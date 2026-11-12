// Frame accessibility check in the system WebKitGTK (the Linux release webview): serve the
// built UI on loopback, run axe at WCAG 2.2 AA on every frame page in every theme the token
// gate knows, then fail on sideways scrolling at 200% text and at 400% zoom.
// PP_UI_EVIDENCE_DIR=<dir> also saves a screenshot of each load there. A missing engine
// prints SKIP and exits 77; a SKIP is not a pass. Run `npm run build` first.
import axe from "axe-core";
import { log } from "node:console";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import process from "node:process";
import { DESTINATIONS } from "../../src/frame/destinations.ts";
import { THEMES } from "../../src/theme/tokenGate.ts";
import { fakeCore } from "./coreFixture.mjs";
import { runWebKitGtk, serveDist } from "./harness.mjs";

const EVIDENCE = process.env.PP_UI_EVIDENCE_DIR;
const TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22a", "wcag22aa"];
const themeAttrs = (name) => THEMES.find((t) => t.name === name).attrs;

const key = (init) =>
  `document.dispatchEvent(new KeyboardEvent("keydown", ${JSON.stringify({ ...init, bubbles: true })}));`;
// P on the gear's first unit: previews its Plate's units before anything is recorded.
const PLATE_KEY = `
  while (!document.querySelector("[data-token]")) await sleep(50);
  const unit = document.querySelector('[aria-label="fixture_gear.stl unit 1"]');
  unit.focus();
  unit.dispatchEvent(new KeyboardEvent("keydown", { key: "p", bubbles: true }));`;
// Opens the Accept gate and waits for its preview.
const GATE = `
  while (![...document.querySelectorAll("button")].some((b) => b.textContent === "Review and accept")) await sleep(50);
  [...document.querySelectorAll("button")].find((b) => b.textContent === "Review and accept").click();
  while (!document.querySelector(".pp-gate-snapshot")) await sleep(50);`;
const STATES = ["loading", "empty", "error", "module-off", "offline", "ready"];
// [name, path, setup]. "?core=down" makes /health fail for that page, so the frame is offline.
const PAGES = [
  ...DESTINATIONS.map((d) => [d.path.slice(1), d.path, ""]),
  ...STATES.map((s) => [`states-${s}`, `/states/${s}`, ""]),
  ["offline-frame", "/builds?core=down", ""],
  ["build-overview", "/builds/3", ""],
  ["build-tab", "/builds/3/sources", ""],
  ["build-checkoff", "/builds/3/checkoff", ""],
  ["build-checkoff-plate", "/builds/3/checkoff", PLATE_KEY],
  ["build-history", "/builds/3/history", ""],
  ["farm-log", "/farm/log", ""],
  ["build-plan", "/builds/3/plan", ""],
  ["build-plan-gate", "/builds/3/plan", GATE],
  ["printer-states", "/states/printers", ""],
  ["palette", "/today", key({ key: "k", ctrlKey: true })],
  ["shortcuts", "/today", key({ key: "?" })],
  ["sign-in", "/login", ""],
  ["setup", "/setup", ""],
];

/** The async body run in the page: wait for the frame, set up, then measure. */
const body = ({ setup = "", audit = false, theme = "dark" }) => `
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const deadline = Date.now() + 15000;
  // The sign-in and setup screens have no frame; they are ready once the core answered.
  const ready = () => document.querySelector("h1") && (document.querySelector(".pp-auth[data-ready] form") ||
    document.querySelector(".pp-connection:not([data-status=checking])")) &&
    // A data screen is audited once its read settles; the loading specimen stays loading.
    (location.pathname.startsWith("/states/") || !document.querySelector("[data-state=loading]"));
  while (!ready()) {
    if (Date.now() > deadline) throw new Error("the frame did not render");
    await sleep(50);
  }
  await document.fonts.ready;
  ${setup}
  await sleep(250);
  const root = document.documentElement;
  // The dialog backdrop is its own scroll container, so the root alone would miss a
  // palette or shortcut table that scrolls sideways inside it.
  const scrollers = [root, ...document.querySelectorAll(".pp-backdrop, .pp-dialog")];
  const overflow = Math.max(...scrollers.map((el) => el.scrollWidth - el.clientWidth));
  const dialogs = document.querySelectorAll(".pp-dialog").length;
  const out = { overflow, dialogs, scrollHeight: root.scrollHeight,
    innerWidth: window.innerWidth, violations: [] };
  const applyTheme = (attrs) => {
    root.removeAttribute("data-theme");
    for (const [name, value] of Object.entries(attrs)) root.setAttribute(name, value);
  };
  for (const theme of ${audit ? JSON.stringify(THEMES) : "[]"}) {
    applyTheme(theme.attrs);
    const result = await runAxe({ runOnly: { type: "tag", values: ${JSON.stringify(TAGS)} } });
    for (const v of result.violations)
      out.violations.push(theme.name + " " + v.id + ": " + v.nodes.map((n) => n.target.join(" ")).join(", "));
  }
  applyTheme(${JSON.stringify(themeAttrs(theme))});
  return JSON.stringify(out);
`;

function jobs(origin) {
  const png = (name) => (EVIDENCE ? path.join(EVIDENCE, `webkitgtk-${name}.png`) : null);
  const list = [];
  const add = (label, pagePath, options, zoom = {}) =>
    list.push({
      label,
      url: `${origin}${pagePath}`,
      body: body(options),
      png: png(label),
      ...zoom,
    });
  for (const [name, pagePath, setup] of PAGES) add(name, pagePath, { setup, audit: true });
  for (const { name } of THEMES.slice(1)) add(`today-${name}`, "/today", { theme: name });
  for (const [name, pagePath, setup] of PAGES) {
    add(`${name}-text-200`, pagePath, { setup }, { zoom: 2, text_only: true });
    add(`${name}-reflow-400`, pagePath, { setup }, { zoom: 4 });
  }
  return list;
}

async function run(list) {
  const dir = mkdtempSync(path.join(tmpdir(), "pp-ui-frame-"));
  const spec = path.join(dir, "jobs.json");
  try {
    writeFileSync(spec, JSON.stringify({ axe: axe.source, jobs: list }));
    const outcome = await runWebKitGtk("webkitgtk-frame.py", [spec]);
    return outcome.skip ? outcome : { results: JSON.parse(outcome.stdout) };
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

// Three Builds, so the Builds list and a Build's Overview are audited with data.
const server = await serveDist({ core: fakeCore(3) });
const list = jobs(`http://127.0.0.1:${server.address().port}`);
let outcome;
try {
  outcome = await run(list);
} finally {
  server.close();
}
if (outcome.skip) {
  log(`SKIP webkitgtk: ${outcome.skip}`);
  process.exit(77);
}
const problems = outcome.results.flatMap(({ result }, i) => [
  ...result.violations.map((v) => `${list[i].label}: ${v}`),
  ...(result.overflow > 0 ? [`${list[i].label}: scrolls sideways by ${result.overflow}px`] : []),
  // The dialog jobs must measure an open dialog, or the overflow check proves nothing.
  ...(/^(palette|shortcuts)/.test(list[i].label) && result.dialogs !== 1
    ? [`${list[i].label}: expected one open dialog, found ${result.dialogs}`]
    : []),
]);
if (problems.length) throw new Error(`webkitgtk:\n  ${problems.join("\n  ")}`);
log(
  `PASS webkitgtk: axe WCAG 2.2 AA clean on ${PAGES.length} pages x ${THEMES.length} themes; ` +
    `no sideways scroll in ${list.length} loads, ${PAGES.length} each at 200% text and 400% zoom`,
);
if (EVIDENCE) log(`screenshots in ${EVIDENCE}`);
