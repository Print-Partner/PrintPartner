import { readFileSync } from "node:fs";
import { resolve } from "node:path";

export const STEPS_DOC = resolve(import.meta.dirname, "../../../docs/product/daily-loop.md");

/**
 * The twelve test titles: the "The owner does" column of the Steps table in
 * docs/product/daily-loop.md, in order. steps.test.mjs fails when the two differ.
 */
export const STEPS = [
  "Starts a fresh Rust-only image and opens the `pp-server bootstrap-url` link",
  "Imports a copy of his Node installation",
  "Adds a Moonraker or PrusaLink printer in Settings",
  "Adds a GitHub Source, or uploads a ZIP",
  "Creates a Build, through the assistant over MCP or in the UI",
  "Edits the Plan and passes the Accept gate",
  "Downloads the STL pack and the HTML checklist",
  "Sends a sliced file to a printer from Production",
  "Watches Farm and Today while it prints",
  "Checks off the finished print",
  "Reads remaining work and filament stock",
  "Makes a backup and restores it on a copy",
];

/** The numbered rows of the Steps table as `{ number, title }`. */
export function documentedSteps(markdown = readFileSync(STEPS_DOC, "utf8")) {
  return [...markdown.matchAll(/^\| (\d+) \| ([^|]+) \|/gm)].map(([, number, title]) => ({
    number: Number(number),
    title: title.trim(),
  }));
}
