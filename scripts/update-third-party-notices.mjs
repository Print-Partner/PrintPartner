#!/usr/bin/env node

import { access, readdir, readFile, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";

const repoRoot = resolve(import.meta.dirname, "..");
const checkOnly = process.argv.slice(2).includes("--check");
const webRoot = join(repoRoot, "web");

// Every workspace that web/package.json declares, so a new one cannot be left out.
async function workspacePackageFiles() {
  const root = JSON.parse(await readFile(join(webRoot, "package.json"), "utf8"));
  const files = [];
  for (const pattern of root.workspaces ?? []) {
    const parent = pattern.slice(0, -"/*".length);
    if (!pattern.endsWith("/*") || parent.includes("*")) {
      throw new Error(`Unsupported workspace pattern: ${pattern}`);
    }
    const entries = await readdir(join(webRoot, parent), { withFileTypes: true });
    const names = entries.filter((entry) => entry.isDirectory()).map((entry) => entry.name).sort();
    for (const name of names) {
      const relativePath = `${parent}/${name}/package.json`;
      try {
        await access(join(webRoot, relativePath));
        files.push(relativePath);
      } catch (error) {
        if (error?.code !== "ENOENT") throw error;
      }
    }
  }
  if (files.length === 0) throw new Error("web/package.json declares no workspaces");
  return files;
}

function normalizeRepositoryUrl(raw) {
  const url = raw
    .replace(/^git\+/, "")
    .replace(/^git:\/\/github\.com\//, "https://github.com/")
    .replace(/^http:\/\/github\.com\//, "https://github.com/")
    .replace(/\.git$/, "");
  if (url.startsWith("github:")) {
    return `https://github.com/${url.slice("github:".length)}`;
  }
  return /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(url)
    ? `https://github.com/${url}`
    : url;
}

function repositoryUrl(repository) {
  if (typeof repository === "string") return normalizeRepositoryUrl(repository);
  if (repository && typeof repository.url === "string") {
    return normalizeRepositoryUrl(repository.url);
  }
  return "";
}

function cell(value) {
  return String(value).replaceAll("|", "\\|");
}

async function dependencyManifestPath(workspaceDirectory, name) {
  let directory = workspaceDirectory;
  while (directory !== dirname(directory)) {
    const candidate = join(directory, "node_modules", ...name.split("/"), "package.json");
    try {
      await readFile(candidate, "utf8");
      return candidate;
    } catch (error) {
      if (error?.code !== "ENOENT") throw error;
    }
    directory = dirname(directory);
  }
  throw new Error(`Could not resolve package manifest for ${name}`);
}

const directDependencies = new Map();
for (const relativePath of await workspacePackageFiles()) {
  const workspaceManifestPath = join(webRoot, relativePath);
  const manifest = JSON.parse(await readFile(workspaceManifestPath, "utf8"));
  for (const name of Object.keys(manifest.dependencies ?? {})) {
    if (name.startsWith("@print-partner/")) continue;
    const dependencyPath = await dependencyManifestPath(dirname(workspaceManifestPath), name);
    const paths = directDependencies.get(name) ?? new Set();
    paths.add(dependencyPath);
    directDependencies.set(name, paths);
  }
}

const rows = [];
for (const [name, dependencyPaths] of [...directDependencies].sort(([left], [right]) => left.localeCompare(right))) {
  for (const dependencyPath of dependencyPaths) {
    const manifest = JSON.parse(await readFile(dependencyPath, "utf8"));
    const license =
      typeof manifest.license === "string"
        ? manifest.license
        : Array.isArray(manifest.licenses)
          ? manifest.licenses.map((item) => item.type).filter(Boolean).join(", ")
          : "SEE PACKAGE";
    const url = normalizeRepositoryUrl(
      manifest.homepage || repositoryUrl(manifest.repository),
    );
    rows.push(`| ${cell(name)} | ${cell(manifest.version)} | ${cell(license)} | ${cell(url)} |`);
  }
}

const noticesPath = join(repoRoot, "THIRD_PARTY_NOTICES.md");
const notices = await readFile(noticesPath, "utf8");
const tableStart = notices.indexOf("| Package | Version | License | URL |");
const tableEnd = notices.indexOf("\n\nVersions reflect", tableStart);

if (tableStart === -1 || tableEnd === -1) {
  throw new Error("Could not find the dependency table in THIRD_PARTY_NOTICES.md");
}

const table = [
  "| Package | Version | License | URL |",
  "|---------|---------|---------|-----|",
  ...rows,
].join("\n");
const updated = `${notices.slice(0, tableStart)}${table}${notices.slice(tableEnd)}`;
const targets = [
  noticesPath,
  join(repoRoot, "web/apps/server/src/data/legal/THIRD_PARTY_NOTICES.md"),
];
const labels = targets.map((target) => target.slice(repoRoot.length + 1));

if (checkOnly) {
  const problems = [];
  for (let index = 0; index < targets.length; index += 1) {
    const label = labels[index];
    const target = targets[index];
    try {
      const actual = await readFile(target, "utf8");
      if (actual !== updated) problems.push(`${label} differs`);
    } catch (error) {
      if (error?.code !== "ENOENT") throw error;
      problems.push(`${label} is missing`);
    }
  }
  if (problems.length > 0) {
    process.stderr.write(`Dependency notices check failed: ${problems.join("; ")}\n`);
    process.exitCode = 1;
  } else {
    process.stdout.write(`Dependency notices match ${labels.join(" and ")}\n`);
  }
} else {
  for (const target of targets) {
    await writeFile(target, updated);
  }
  process.stdout.write(`Updated ${labels.join(" and ")}\n`);
}
