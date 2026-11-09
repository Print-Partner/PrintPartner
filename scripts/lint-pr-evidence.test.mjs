import assert from "node:assert/strict";
import test from "node:test";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { lintPrEvidence, main, needsVisualEvidence } from "./lint-pr-evidence.mjs";

test("empty Evidence fails with a missing, empty, or none exemption", () => {
  for (const field of ["", "Exemption:", "Exemption: none", "Exemption: NONE"]) {
    assert.equal(lintPrEvidence(`## Evidence\n \t\n${field}`).ok, false);
  }
});

test("empty Evidence passes with any exemption reason", () => {
  assert.equal(lintPrEvidence("## Evidence\nExemption: Documentation update only.").ok, true);
});

test("non-empty Evidence passes without required fields or artifacts", () => {
  for (const evidence of ["Verified locally.", "`screenshot`", "http://192.168.1.80", "<a href=\"http://3232235856\">checked</a>", "```text\nAll checks passed\n```", "### Checks\nPassed."]) {
    assert.equal(lintPrEvidence(`## Evidence\n${evidence}\nExemption: none`).ok, true);
  }
});

test("comments and template placeholders count as empty Evidence", () => {
  for (const evidence of ["<!-- attach evidence -->", "<!-- first\nsecond -->", "<paste evidence here>", "- TODO: attach evidence", "TBD", "placeholder", "```text\n```", "<!-- unclosed"]) {
    assert.equal(lintPrEvidence(`## Evidence\n${evidence}`).ok, false);
  }
  assert.equal(lintPrEvidence(readFileSync(new URL("../.github/pull_request_template.md", import.meta.url), "utf8")).ok, false);
});

test("missing Evidence heading fails without an exemption and passes with a reason", () => {
  assert.equal(lintPrEvidence("## Summary\nTested locally.\nExemption: none").ok, false);
  assert.equal(lintPrEvidence("## Exemption\nExemption: No runtime change.").ok, true);
});

test("other sections and exemption metadata do not fill Evidence", () => {
  assert.equal(lintPrEvidence("## Evidence\nExemption: none\n## Test plan\nPassed.").ok, false);
  assert.equal(lintPrEvidence("## Evidence\n# Background\nPassed.").ok, false);
});

test("commented or fenced headings and exemptions do not supply metadata", () => {
  assert.equal(lintPrEvidence("<!-- ## Evidence\nPassed.\nExemption: docs only -->").ok, false);
  assert.equal(lintPrEvidence("```text\n## Evidence\nPassed.\nExemption: docs only\n```").ok, false);
  assert.equal(lintPrEvidence("## Evidence\n<!-- empty -->\nExemption: <!-- reason -->").ok, false);
  assert.equal(lintPrEvidence("## Evidence\nExemption: <reason for exemption>").ok, false);
});

test("comments around real Evidence and CRLF headings are accepted", () => {
  assert.equal(lintPrEvidence("## Evidence\r\n<!-- note --> Verified locally.\r\n## Exemption\r\nExemption: none").ok, true);
});

test("UI and desktop changes need a screenshot or video in Evidence", () => {
  const ui = ["web/apps/web/src/routes/Builds.tsx"];
  assert.equal(lintPrEvidence("## Evidence\nClicked through Builds.\nExemption: none", { changedFiles: ui }).ok, false);
  for (const media of [
    "![Builds page](https://github.com/user-attachments/assets/0b6c3f7e-4d1a-4c2b-9f00-1a2b3c4d5e6f)",
    "https://github.com/user-attachments/assets/0b6c3f7e-4d1a-4c2b-9f00-1a2b3c4d5e6f",
    "<img width=\"600\" src=\"https://raw.githubusercontent.com/Print-Partner/PrintPartner/evidence/x/window.png\">",
    "<video src=\"https://example.com/demo.webm\"></video>",
    "Recording: https://example.com/demo.mp4",
    "[tray](https://github.com/Print-Partner/PrintPartner/blob/evidence/x/tray.png?raw=true)",
  ]) {
    assert.equal(lintPrEvidence(`## Evidence\n${media}\nExemption: none`, { changedFiles: ui }).ok, true, media);
  }
});

test("fenced, commented, or non-https media does not count as visual evidence", () => {
  const ui = ["rust/crates/pp-desktop/src/tray.rs"];
  for (const media of ["```text\n![x](https://example.com/a.png)\n```\nChecked.", "<!-- ![x](https://example.com/a.png) -->Checked.", "![x](http://example.com/a.png)", "Saved shot.png locally."]) {
    assert.equal(lintPrEvidence(`## Evidence\n${media}\nExemption: none`, { changedFiles: ui }).ok, false, media);
  }
});

test("an Exemption reason waives the visual requirement", () => {
  const result = lintPrEvidence("## Evidence\nRefactor only.\nExemption: No visible change; types only.",
    { changedFiles: ["web/apps/web/src/lib/format.ts"] });
  assert.equal(result.ok, true);
});

test("tests, docs, and other paths do not need visual evidence", () => {
  assert.equal(needsVisualEvidence([
    "web/apps/web/src/routes/Builds.test.tsx", "web/apps/web/test/setup.ts",
    "rust/crates/pp-desktop/tests/tray.rs", "rust/crates/pp-desktop/README.md",
    "web/apps/server/src/index.ts", "rust/crates/pp-core/src/lib.rs", "docs/README.md",
  ]), false);
  assert.equal(needsVisualEvidence(["web/apps/web/index.html"]), true);
  assert.equal(needsVisualEvidence(["rust/crates/pp-desktop/tauri.conf.json"]), true);
});

test("the CLI reads changed files from CHANGED_FILES_PATH", () => {
  const dir = mkdtempSync(join(tmpdir(), "evidence-"));
  const list = join(dir, "files.txt");
  writeFileSync(list, "web/apps/web/src/App.tsx\n");
  const body = join(dir, "body.md");
  writeFileSync(body, "## Evidence\nLooked at it.\nExemption: none\n");
  const errors = [];
  const io = { log() {}, error: (line) => errors.push(line) };
  const previous = process.env.CHANGED_FILES_PATH;
  process.env.CHANGED_FILES_PATH = list;
  try {
    assert.equal(main(["--body-file", body], io), 1);
    assert.match(errors.join("\n"), /screenshot or video/);
  } finally {
    if (previous === undefined) delete process.env.CHANGED_FILES_PATH;
    else process.env.CHANGED_FILES_PATH = previous;
    rmSync(dir, { recursive: true, force: true });
  }
});
