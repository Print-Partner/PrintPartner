#!/usr/bin/env node

import { existsSync, readFileSync, statSync } from "node:fs";
import { dirname, extname, join, normalize, resolve } from "node:path";
import { spawnSync } from "node:child_process";

const repoRoot = resolve(import.meta.dirname, "..");
const canonicalRepository = "poitee/PrintPartner";
const selfPath = "scripts/audit-public-release.mjs";

const requiredFiles = [
  "README.md",
  "LICENSE",
  "LICENSE-SUMMARY.md",
  "ATTRIBUTION.md",
  "THIRD_PARTY_NOTICES.md",
  "SECURITY.md",
  "OPERATIONS.md",
  "CHANGELOG.md",
  "docs/README.md",
  "docs/INSTALL.md",
  "docs/api.md",
  "docs/architecture.md",
  "docs/assistant-mcp.md",
  "docs/screenshots/README.md",
];

const forbiddenPathPrefixes = [
  ".audit/",
  ".superpowers/",
  "docs/research/",
  "docs/superpowers/",
];

const forbiddenFiles = new Set([
  "docs/assistant-domain-ingest-schema.md",
  "docs/assistant-research-brief.md",
]);

function gitignoreIgnoresPrefix(gitignore, prefix) {
  const exact = prefix.replace(/\/$/, "");
  return gitignore.split(/\r?\n/).some((line) => {
    const trimmed = line.trim();
    return trimmed === prefix || trimmed === exact || trimmed === `${exact}/`;
  });
}

const screenshotNames = [
  "library.png",
  "builds.png",
  "sources.png",
  "plan.png",
  "checkoff.png",
  "production.png",
];

const forbiddenText = [
  ["private repository name", "PrintPartner-private"],
  ["retired public repository name", "PrintPartnerPartner"],
  ["local maintainer path", "/Users/poitee"],
];

function listFiles() {
  const result = spawnSync(
    "git",
    ["ls-files", "--cached", "--others", "--exclude-standard", "-z"],
    { cwd: repoRoot, encoding: "utf8" },
  );
  if (result.status !== 0) {
    process.stderr.write(result.stderr);
    process.exit(result.status ?? 1);
  }
  return result.stdout
    .split("\0")
    .filter(Boolean)
    .filter((file) => {
      const absolute = join(repoRoot, file);
      return existsSync(absolute) && statSync(absolute).isFile();
    })
    .sort();
}

function isTextFile(file) {
  if (["LICENSE", "Caddyfile", "Dockerfile"].includes(file)) return true;
  return new Set([
    ".cjs",
    ".css",
    ".env",
    ".html",
    ".js",
    ".json",
    ".jsonc",
    ".md",
    ".mjs",
    ".mts",
    ".sh",
    ".ts",
    ".tsx",
    ".txt",
    ".yaml",
    ".yml",
  ]).has(extname(file).toLowerCase());
}

function localTarget(documentPath, rawTarget) {
  const target = rawTarget.trim().replace(/^<|>$/g, "").split(/\s+["']/u, 1)[0];
  if (!target || target.startsWith("#") || /^(?:[a-z]+:|\/\/)/i.test(target)) {
    return null;
  }
  const pathOnly = decodeURIComponent(target.split("#", 1)[0].split("?", 1)[0]);
  return normalize(
    pathOnly.startsWith("/")
      ? pathOnly.slice(1)
      : join(dirname(documentPath), pathOnly),
  );
}

function pngDimensions(file) {
  const data = readFileSync(join(repoRoot, file));
  if (data.length < 24 || data.toString("ascii", 1, 4) !== "PNG") return null;
  return { width: data.readUInt32BE(16), height: data.readUInt32BE(20) };
}

const files = listFiles();
const fileSet = new Set(files.map((file) => normalize(file)));
const failures = [];

for (const file of requiredFiles) {
  if (!fileSet.has(file)) failures.push(`Missing required public file: ${file}`);
}

for (const file of files) {
  if (file.endsWith("/.DS_Store") || file === ".DS_Store") {
    failures.push(`macOS metadata must not ship: ${file}`);
  }
  if (forbiddenFiles.has(file) || forbiddenPathPrefixes.some((prefix) => file.startsWith(prefix))) {
    failures.push(`Internal-only file must not ship: ${file}`);
  }
}

const gitignoreFile = files.includes(".gitignore")
  ? readFileSync(join(repoRoot, ".gitignore"), "utf8")
  : "";
if (!gitignoreFile) {
  failures.push("Missing required public file: .gitignore");
} else {
  for (const prefix of forbiddenPathPrefixes) {
    if (!gitignoreIgnoresPrefix(gitignoreFile, prefix)) {
      failures.push(`.gitignore must ignore internal prefix ${prefix}`);
    }
  }
}

for (const file of files.filter((candidate) => isTextFile(candidate) && candidate !== selfPath)) {
  const contents = readFileSync(join(repoRoot, file), "utf8");
  for (const [label, text] of forbiddenText) {
    if (contents.includes(text)) failures.push(`${file} contains ${label}: ${text}`);
  }
}

for (const file of files.filter((candidate) => candidate.endsWith(".md"))) {
  const contents = readFileSync(join(repoRoot, file), "utf8");
  for (const match of contents.matchAll(/!?\[[^\]]*\]\(([^)]+)\)/g)) {
    const target = localTarget(file, match[1]);
    if (target && !fileSet.has(target) && !existsSync(join(repoRoot, target))) {
      failures.push(`${file} links to missing local file: ${match[1]}`);
    }
  }
}

for (const file of files.filter((candidate) => candidate === "docs/index.html")) {
  const contents = readFileSync(join(repoRoot, file), "utf8");
  for (const match of contents.matchAll(/(?:href|src|srcset)="([^"]+)"/g)) {
    const target = localTarget(file, match[1]);
    if (target && !fileSet.has(target) && !existsSync(join(repoRoot, target))) {
      failures.push(`${file} references missing local file: ${match[1]}`);
    }
  }
}

for (const theme of ["light", "dark"]) {
  for (const name of screenshotNames) {
    const file = `docs/screenshots/${theme}/${name}`;
    if (!fileSet.has(file)) {
      failures.push(`Missing ${theme} screenshot: ${name}`);
      continue;
    }
    const dimensions = pngDimensions(file);
    if (!dimensions || dimensions.width !== 1440 || dimensions.height !== 900) {
      failures.push(`${file} must be a 1440x900 PNG`);
    }
  }
}

const readme = fileSet.has("README.md") ? readFileSync(join(repoRoot, "README.md"), "utf8") : "";
if (!readme.includes(`github.com/${canonicalRepository}`)) {
  failures.push(`README.md must link to github.com/${canonicalRepository}`);
}
for (const stage of ["Builds", "Sources", "Plan", "Checkoff", "Production"]) {
  if (!readme.includes(stage)) failures.push(`README.md does not describe ${stage}`);
}

if (failures.length > 0) {
  process.stderr.write(`Public release audit failed (${failures.length}):\n`);
  for (const failure of failures) process.stderr.write(`- ${failure}\n`);
  process.exit(1);
}

process.stdout.write(
  `Public release audit passed: ${files.length} files, ${requiredFiles.length} required docs, ${screenshotNames.length * 2} screenshots.\n`,
);
