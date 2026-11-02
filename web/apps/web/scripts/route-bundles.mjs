#!/usr/bin/env node
// Checks JavaScript byte budgets and static three.js imports after `vite build --manifest`.
import { readFileSync, readdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import process from "node:process";
import { fileURLToPath, URL } from "node:url";
import { gzipSync } from "node:zlib";

const dist = join(dirname(fileURLToPath(import.meta.url)), "..", "dist");
const manifest = JSON.parse(readFileSync(join(dist, ".vite", "manifest.json"), "utf8"));
const budgetsPath = new URL("./route-budgets.json", import.meta.url);
const args = process.argv.slice(2);
if (args.length && (args.length !== 1 || args[0] !== "--write-budgets")) {
  throw new Error("Usage: node scripts/route-bundles.mjs [--write-budgets]");
}

function staticClosure(key, seen = new Set()) {
  if (seen.has(key)) return seen;
  seen.add(key);
  for (const next of manifest[key].imports ?? []) staticClosure(next, seen);
  return seen;
}

const fileCache = new Map();
function fileByName(name) {
  if (!fileCache.has(name)) {
    const bytes = readFileSync(join(dist, name));
    fileCache.set(name, {
      raw: bytes.length,
      gzip: gzipSync(bytes).length,
      hasThree: bytes.includes("WebGLRenderer"),
    });
  }
  return fileCache.get(name);
}

const file = (key) => fileByName(manifest[key].file);
const sum = (keys, field) => [...new Set([...keys].map((key) => manifest[key].file))]
  .reduce((total, name) => total + fileByName(name)[field], 0);
const size = (keys) => ({ raw: sum(keys, "raw"), gzip: sum(keys, "gzip") });
const kb = (bytes) => `${Math.round(bytes / 1024)} KB`;

const entry = Object.keys(manifest).find((key) => manifest[key].isEntry);
const shell = Object.keys(manifest).find((key) => key.endsWith("src/AuthenticatedApp.tsx"));
const publicBase = staticClosure(entry);
const authenticatedBase = new Set([...publicBase, ...staticClosure(shell)]);
const publicPages = new Set([
  "src/pages/LoginPage.tsx",
  "src/pages/ForgotPasswordPage.tsx",
  "src/pages/ResetPasswordPage.tsx",
]);
const pages = Object.keys(manifest)
  .filter((key) => key.startsWith("src/pages/") && key.endsWith(".tsx"))
  .sort();
if (!pages.length) throw new Error("No route chunks found in the Vite manifest.");
const measurements = {
  "public visits": size(publicBase),
  "authenticated visits": size(authenticatedBase),
};

const rows = [
  ["route", "baseline", "raw", "gzip", "three.js"],
  ["(public visits)", "", kb(sum(publicBase, "raw")), kb(sum(publicBase, "gzip")), ""],
  ["(authenticated visits)", "", kb(sum(authenticatedBase, "raw")), kb(sum(authenticatedBase, "gzip")), ""],
];
const leaks = [];
for (const page of pages) {
  const isPublic = publicPages.has(page);
  const base = isPublic ? publicBase : authenticatedBase;
  measurements[page] = size(new Set([...base, ...staticClosure(page)]));
  const extra = new Set([...staticClosure(page)].filter((key) => !base.has(key)));
  const three = [...staticClosure(page)].some((key) => file(key).hasThree);
  if (three) leaks.push(page);
  rows.push([page.replace(/^src\/pages\//, ""), isPublic ? "public" : "authenticated", `+${kb(sum(extra, "raw"))}`, `+${kb(sum(extra, "gzip"))}`, three ? "static" : ""]);
}
for (const [label, base] of [["public visits", publicBase], ["authenticated visits", authenticatedBase]]) {
  if ([...base].some((key) => file(key).hasThree)) leaks.push(`(${label})`);
}

const widths = rows[0].map((_, column) => Math.max(...rows.map((row) => row[column].length)));
for (const row of rows) process.stdout.write(`${row.map((cell, column) => cell.padEnd(widths[column])).join("  ")}\n`);

const javaScript = readdirSync(dist, { recursive: true })
  .filter((name) => /\.[cm]?js$/.test(name)).sort();
if (!javaScript.length) throw new Error("No JavaScript files found in dist.");
measurements["all JavaScript"] = { raw: 0, gzip: 0 };
measurements["largest JavaScript file"] = { raw: 0, gzip: 0 };
for (const name of javaScript) {
  for (const field of ["raw", "gzip"]) {
    const bytes = fileByName(name)[field];
    measurements["all JavaScript"][field] += bytes;
    measurements["largest JavaScript file"][field] = Math.max(
      measurements["largest JavaScript file"][field], bytes,
    );
  }
}
for (const label of ["all JavaScript", "largest JavaScript file"]) {
  process.stdout.write(`${label}: ${measurements[label].raw} raw bytes, ${measurements[label].gzip} gzip bytes\n`);
}

if (leaks.length) {
  process.stderr.write(`\nthree.js is in the static import closure of: ${leaks.join(", ")}. Load 3D code with lazy() or import().\n`);
  process.exit(1);
}

if (args[0] === "--write-budgets") {
  // Allow 5% growth, rounded up to the next KiB. Changes require PR review.
  const limits = Object.fromEntries(Object.entries(measurements).map(([label, measured]) =>
    [label, Object.fromEntries(Object.entries(measured).map(([field, bytes]) =>
      [field, Math.ceil(bytes * 1.05 / 1024) * 1024]))]));
  writeFileSync(budgetsPath, `${JSON.stringify(limits, null, 2)}\n`);
  process.stdout.write("Wrote route-budgets.json. Review budget changes before committing.\n");
} else {
  const limits = JSON.parse(readFileSync(budgetsPath, "utf8"));
  const failures = [];
  for (const [label, measured] of Object.entries(measurements)) {
    for (const field of ["raw", "gzip"]) {
      const limit = limits[label]?.[field];
      if (!Number.isSafeInteger(limit) || limit <= 0) {
        failures.push(`${label} ${field}: missing or invalid byte budget`);
      } else if (measured[field] > limit) {
        failures.push(`${label} ${field}: ${measured[field]} bytes exceeds ${limit} by ${measured[field] - limit} bytes`);
      }
    }
  }
  if (failures.length) {
    process.stderr.write(`\nBundle budget failed:\n${failures.join("\n")}\nRemove unnecessary imports or review an explicit budget change.\n`);
    process.exit(1);
  }
  process.stdout.write("All JavaScript byte budgets pass.\n");
}
