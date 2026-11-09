import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");

function read(relativePath) {
  return readFileSync(join(root, relativePath), "utf8");
}

function assertLocalImageReferencesExist(documentPath, contents) {
  for (const match of contents.matchAll(/(?:src|srcset)="([^"]+)"/g)) {
    const reference = match[1];
    if (!reference || /^[a-z]+:/i.test(reference)) continue;
    const imagePath = join(root, dirname(documentPath), reference.split(/[?#]/, 1)[0]);
    assert.equal(existsSync(imagePath), true, `${documentPath} references missing image ${reference}`);
  }
}

test("public documentation matches the current product map", () => {
  const readme = read("README.md");
  const docsIndex = read("docs/README.md");
  const install = read("docs/INSTALL.md");
  const pages = read("docs/index.html");
  const pkg = JSON.parse(read("web/package.json"));

  for (const stage of ["Builds", "Sources", "Plan", "Checkoff", "Production"]) {
    assert.match(readme, new RegExp(stage));
    assert.match(docsIndex, new RegExp(stage));
    assert.match(pages, new RegExp(`pipeline-step">${stage}<`));
  }

  assert.match(readme, /github\.com\/Print-Partner\/PrintPartner/);
  assert.match(install, /git clone https:\/\/github\.com\/Print-Partner\/PrintPartner\.git/);
  assert.match(pkg.description, /Builds/);
  assert.match(pkg.description, /Production/);

  for (const name of ["library", "builds", "sources", "plan", "checkoff", "production"]) {
    assert.match(readme, new RegExp(`docs/screenshots/light/${name}\\.png`));
  }

  assert.doesNotMatch(docsIndex, /assistant-research-brief/);
  assert.doesNotMatch(docsIndex, /assistant-domain-ingest-schema/);
  assertLocalImageReferencesExist("README.md", readme);
  assertLocalImageReferencesExist("docs/index.html", pages);
});

test("screenshot capture names match their screens", () => {
  const capture = read("docs/scripts/capture-screenshots.mjs");
  const mappings = [
    ["/library", "library.png"],
    ["/builds", "builds.png"],
    ["/sources", "sources.png"],
    ["/plan", "plan.png"],
    ["/progress", "checkoff.png"],
    ["/export", "production.png"],
  ];

  for (const [path, file] of mappings) {
    assert.match(capture, new RegExp(`path: "${path.replace("/", "\\/")}"[\\s\\S]*?file: "${file.replace(".", "\\.")}"`));
  }
});
