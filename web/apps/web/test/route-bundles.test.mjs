import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import process from "node:process";
import test from "node:test";
import { URL } from "node:url";
import { gzipSync } from "node:zlib";

function fixture(t, rendererChunk) {
  const root = mkdtempSync(join(tmpdir(), "pp-route-bundles-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  mkdirSync(join(root, "scripts"));
  mkdirSync(join(root, "dist", ".vite"), { recursive: true });
  mkdirSync(join(root, "dist", "assets"));
  const script = join(root, "scripts", "route-bundles.mjs");
  copyFileSync(new URL("../scripts/route-bundles.mjs", import.meta.url), script);
  const manifest = {
    "index.html": { isEntry: true, file: "assets/entry.js", imports: ["shared"] },
    "shared": { file: "assets/shared.js" },
    "src/AuthenticatedApp.tsx": { file: "assets/shell.js", imports: ["shared", "authenticated"] },
    "authenticated": { file: "assets/authenticated.js" },
    "src/pages/LoginPage.tsx": { file: "assets/login.js", imports: ["shared", "public"], dynamicImports: ["preview"] },
    "src/pages/ForgotPasswordPage.tsx": { file: "assets/forgot.js", imports: ["shared", "public"] },
    "src/pages/ResetPasswordPage.tsx": { file: "assets/reset.js", imports: ["shared", "public"] },
    "public": { file: "assets/public.js" },
    "src/pages/PartsPage.tsx": { file: "assets/parts.js", imports: ["shared", "authenticated"], dynamicImports: ["preview"] },
    "preview": { file: "assets/preview.js" },
  };
  writeFileSync(join(root, "dist", ".vite", "manifest.json"), JSON.stringify(manifest));
  for (const entry of Object.values(manifest)) {
    const renderer = entry.file === "assets/preview.js" || entry.file === rendererChunk;
    const contents = (renderer ? "WebGLRenderer" : "a").padEnd(1024, "a");
    writeFileSync(join(root, "dist", entry.file), contents);
  }
  const labels = ["public visits", "authenticated visits", "all JavaScript", "largest JavaScript file",
    ...Object.keys(manifest).filter((key) => key.startsWith("src/pages/"))];
  writeFileSync(join(root, "scripts", "route-budgets.json"), JSON.stringify(
    Object.fromEntries(labels.map((label) => [label, { raw: 1_000_000, gzip: 1_000_000 }]))));
  return script;
}

function budget(script, label, field, limit) {
  const path = join(dirname(script), "route-budgets.json");
  const limits = JSON.parse(readFileSync(path, "utf8"));
  limits[label][field] = limit;
  writeFileSync(path, JSON.stringify(limits));
}

function editManifest(script, edit) {
  const path = join(dirname(script), "..", "dist", ".vite", "manifest.json");
  const manifest = JSON.parse(readFileSync(path, "utf8"));
  edit(manifest);
  writeFileSync(path, JSON.stringify(manifest));
}

function exactPublicBudget(script) {
  budget(script, "public visits", "raw", 2048);
  budget(script, "public visits", "gzip", 2 * gzipSync("a".repeat(1024)).length);
}

function fails(script, message) {
  assert.throws(() => execFileSync(process.execPath, [script], { stdio: "pipe" }),
    (error) => error.status === 1 && error.stderr.toString().includes(message));
}

test("public pages exclude the authenticated shell and dynamic previews", (t) => {
  const output = execFileSync(process.execPath, [fixture(t)], { encoding: "utf8" });
  assert.match(output, /\(public visits\)\s+2 KB/);
  assert.match(output, /\(authenticated visits\)\s+4 KB/);
  for (const page of ["LoginPage", "ForgotPasswordPage", "ResetPasswordPage"]) {
    assert.match(output, new RegExp(`${page}\\.tsx\\s+public\\s+\\+2 KB`));
  }
  assert.match(output, /PartsPage\.tsx\s+authenticated\s+\+1 KB/);
});

test("three.js in an authenticated shell dependency fails the check", (t) => {
  assert.throws(() => execFileSync(process.execPath, [fixture(t, "assets/authenticated.js")], { stdio: "pipe" }),
    (error) => error.status === 1 && error.stderr.toString().includes("(authenticated visits)"));
});

test("three.js in a public page dependency fails the check", (t) => {
  assert.throws(() => execFileSync(process.execPath, [fixture(t, "assets/public.js")], { stdio: "pipe" }),
    (error) => error.status === 1 && error.stderr.toString().includes("src/pages/LoginPage.tsx"));
});

for (const label of ["public visits", "authenticated visits", "src/pages/PartsPage.tsx",
  "all JavaScript", "largest JavaScript file"]) {
  for (const field of ["raw", "gzip"]) {
    test(`${label} fails when its ${field} byte budget is exceeded`, (t) => {
      const script = fixture(t);
      budget(script, label, field, 1);
      fails(script, `${label} ${field}:`);
    });
  }
}

test("exact limits pass and a one-byte regression fails without display rounding", (t) => {
  const script = fixture(t);
  exactPublicBudget(script);
  execFileSync(process.execPath, [script], { stdio: "pipe" });
  budget(script, "public visits", "raw", 2047);
  fails(script, "2048 bytes exceeds 2047 by 1 bytes");
});

test("route budgets include their shared baseline", (t) => {
  const script = fixture(t);
  budget(script, "src/pages/PartsPage.tsx", "raw", 5119);
  fails(script, "5120 bytes exceeds 5119");
});

test("static closures handle cycles and count each output file once", (t) => {
  const script = fixture(t);
  editManifest(script, (manifest) => {
    manifest.alias = { file: "assets/shared.js", imports: ["index.html"] };
    manifest["index.html"].imports.push("alias");
  });
  exactPublicBudget(script);
  execFileSync(process.execPath, [script], { stdio: "pipe" });
});

test("growing a dynamic preview fails total and single-file limits", (t) => {
  const script = fixture(t);
  budget(script, "all JavaScript", "raw", 10240);
  budget(script, "largest JavaScript file", "raw", 1024);
  execFileSync(process.execPath, [script], { stdio: "pipe" });
  writeFileSync(join(dirname(script), "..", "dist", "assets", "preview.js"),
    "WebGLRenderer".padEnd(1025, "a"));
  fails(script, "all JavaScript raw: 10241 bytes exceeds 10240");
  fails(script, "largest JavaScript file raw: 1025 bytes exceeds 1024");
});

test("JavaScript outside the manifest is counted", (t) => {
  const script = fixture(t);
  budget(script, "all JavaScript", "raw", 10240);
  writeFileSync(join(dirname(script), "..", "dist", "worker.js"), "a");
  fails(script, "all JavaScript raw: 10241 bytes exceeds 10240");
});

test("a nested route without a budget fails", (t) => {
  const script = fixture(t);
  editManifest(script, (manifest) => {
    manifest["src/pages/admin/SettingsPage.tsx"] = { file: "assets/parts.js" };
  });
  fails(script, "src/pages/admin/SettingsPage.tsx raw: missing or invalid byte budget");
});

test("invalid or missing budgets fail instead of disabling the check", (t) => {
  const script = fixture(t);
  for (const limit of [null, -1, "1000000", 1.5]) {
    budget(script, "public visits", "raw", limit);
    fails(script, "public visits raw: missing or invalid byte budget");
  }
  rmSync(join(dirname(script), "route-budgets.json"));
  fails(script, "ENOENT");
});

test("missing manifest imports fail", (t) => {
  const script = fixture(t);
  editManifest(script, (manifest) => {
    delete manifest.shared;
  });
  fails(script, "TypeError");
});

test("explicit budget generation is repeatable and the generated limits pass", (t) => {
  const script = fixture(t);
  const path = join(dirname(script), "route-budgets.json");
  execFileSync(process.execPath, [script, "--write-budgets"], { stdio: "pipe" });
  const first = readFileSync(path, "utf8");
  assert.equal(JSON.parse(first)["public visits"].raw, 3072);
  execFileSync(process.execPath, [script, "--write-budgets"], { stdio: "pipe" });
  assert.equal(readFileSync(path, "utf8"), first);
  execFileSync(process.execPath, [script], { stdio: "pipe" });
});

test("budget generation cannot accept a static three.js leak", (t) => {
  assert.throws(() => execFileSync(process.execPath,
    [fixture(t, "assets/authenticated.js"), "--write-budgets"], { stdio: "pipe" }),
    (error) => error.status === 1 && error.stderr.toString().includes("(authenticated visits)"));
});
