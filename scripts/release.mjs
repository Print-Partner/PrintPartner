#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import {
  existsSync,
  readFileSync,
  renameSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const RELEASE_REPOSITORY = "Print-Partner/PrintPartner";
const IMAGE_REPOSITORY = "ghcr.io/print-partner/print-partner";
// Releases up to 3.3.0 were published here, before the repository moved to the Print-Partner
// organization. Compose files and docs keep naming it until the next release, which is the
// first one published at IMAGE_REPOSITORY, and the release plan then moves them over.
const LEGACY_IMAGE_REPOSITORY = "ghcr.io/poitee/print-partner";
const VERSION_PATTERN = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;
const SHA_PATTERN = /^[0-9a-f]{40}$/;
const DIGEST_PATTERN = /^sha256:[0-9a-f]{64}$/;
const VERSIONED_DOCS = ["README.md", "web/DEPLOY.md", "OPERATIONS.md"];
const COMPOSE_FILES = ["docker-compose.yml", "docker-compose.hosted.yml"];
const VERSION_TOKEN = /(?<![\d.])\d+\.\d+\.\d+(?![\d.])/g;

function fail(message) {
  throw new Error(message);
}

export function parseAppVersion(raw) {
  if (!VERSION_PATTERN.test(raw)) fail(`Invalid release version: ${raw}`);
  return raw;
}

function parseTag(raw) {
  if (!raw.startsWith("v")) fail(`Invalid release tag: ${raw}`);
  const version = parseAppVersion(raw.slice(1));
  return { tag: raw, version };
}

function parseCommit(raw) {
  if (!SHA_PATTERN.test(raw)) fail(`Invalid release commit: ${raw}`);
  return raw;
}

function parseDigest(raw) {
  if (!DIGEST_PATTERN.test(raw)) fail(`Invalid image digest: ${raw}`);
  return raw;
}

function compareVersions(left, right) {
  const leftParts = left.split(".").map(Number);
  const rightParts = right.split(".").map(Number);
  for (let index = 0; index < 3; index += 1) {
    if (leftParts[index] !== rightParts[index]) {
      return leftParts[index] - rightParts[index];
    }
  }
  return 0;
}

function read(root, path) {
  return readFileSync(join(root, path), "utf8");
}

function json(contents, path) {
  try {
    return JSON.parse(contents);
  } catch (error) {
    fail(`${path} is not valid JSON: ${error instanceof Error ? error.message : String(error)}`);
  }
}

function formatJson(value) {
  return `${JSON.stringify(value, null, 2)}\n`;
}

function markedVersionBlocks(contents, path) {
  const marker = /<!-- release-version:start -->([\s\S]*?)<!-- release-version:end -->/g;
  const blocks = [...contents.matchAll(marker)];
  if (blocks.length === 0) fail(`${path} has no release-version block`);
  return blocks;
}

function assertMarkedVersions(contents, expectedVersion, path) {
  for (const block of markedVersionBlocks(contents, path)) {
    const versions = [...block[0].matchAll(VERSION_TOKEN)].map((match) => match[0]);
    if (versions.length === 0) fail(`${path} has an empty release-version block`);
    const mismatch = versions.find((version) => version !== expectedVersion);
    if (mismatch) fail(`${path} release-version block contains ${mismatch}, expected ${expectedVersion}`);
  }
}

function replaceMarkedVersions(contents, currentVersion, nextVersion, path) {
  assertMarkedVersions(contents, currentVersion, path);
  return contents.replace(
    /<!-- release-version:start -->([\s\S]*?)<!-- release-version:end -->/g,
    (block) => block.replace(VERSION_TOKEN, nextVersion).replaceAll(currentVersion, nextVersion),
  );
}

function parseReleaseDate(raw) {
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(raw);
  if (!match) fail(`Invalid release date: ${raw}`);
  const [, year, month, day] = match;
  const parsed = new Date(Date.UTC(Number(year), Number(month) - 1, Number(day)));
  if (
    parsed.getUTCFullYear() !== Number(year) ||
    parsed.getUTCMonth() !== Number(month) - 1 ||
    parsed.getUTCDate() !== Number(day)
  ) {
    fail(`Invalid release date: ${raw}`);
  }
  return raw;
}

function promoteChangelog(contents, currentVersion, nextVersion, date) {
  const unreleased = "## [Unreleased]";
  const currentHeading = `## [${currentVersion}]`;
  const start = contents.indexOf(unreleased);
  const nextHeading = contents.indexOf(currentHeading, start + unreleased.length);
  if (start < 0 || nextHeading < 0) {
    fail(`CHANGELOG.md must contain ${unreleased} before ${currentHeading}`);
  }
  if (contents.includes(`## [${nextVersion}]`)) {
    fail(`CHANGELOG.md already contains ${nextVersion}`);
  }

  const pending = contents
    .slice(start + unreleased.length, nextHeading)
    .replace(/^\s+|\s+$/g, "");
  const promoted = `${unreleased}\n\n## [${nextVersion}] - ${date}\n\n${pending}\n\n`;
  let updated = `${contents.slice(0, start)}${promoted}${contents.slice(nextHeading)}`;

  const compareBase = `https://github.com/${RELEASE_REPOSITORY}`;
  const unreleasedLink = `[Unreleased]: ${compareBase}/compare/v${nextVersion}...HEAD`;
  if (/^\[Unreleased\]: .*$/m.test(updated)) {
    updated = updated.replace(/^\[Unreleased\]: .*$/m, unreleasedLink);
  } else {
    updated = `${updated.trimEnd()}\n${unreleasedLink}\n`;
  }
  updated = `${updated.trimEnd()}\n[${nextVersion}]: ${compareBase}/releases/tag/v${nextVersion}\n`;
  return updated;
}

export function planRelease({ repoRoot, nextVersion, date }) {
  const validatedNextVersion = parseAppVersion(nextVersion);
  const validatedDate = parseReleaseDate(date);

  const packagePath = "web/package.json";
  const packageContents = read(repoRoot, packagePath);
  const packageJson = json(packageContents, packagePath);
  const currentVersion = parseAppVersion(packageJson.version);
  if (compareVersions(validatedNextVersion, currentVersion) <= 0) {
    fail(`Next version ${validatedNextVersion} must be greater than ${currentVersion}`);
  }

  const changes = [];
  const add = (path, before, after) => {
    if (before === after) fail(`${path} produced no release change`);
    changes.push({ path, absolutePath: join(repoRoot, path), before, after });
  };

  packageJson.version = validatedNextVersion;
  add(packagePath, packageContents, formatJson(packageJson));

  const lockPath = "web/package-lock.json";
  const lockContents = read(repoRoot, lockPath);
  const lockJson = json(lockContents, lockPath);
  if (lockJson.version !== currentVersion || lockJson.packages?.[""]?.version !== currentVersion) {
    fail(`${lockPath} root versions do not match ${currentVersion}`);
  }
  lockJson.version = validatedNextVersion;
  lockJson.packages[""].version = validatedNextVersion;
  add(lockPath, lockContents, formatJson(lockJson));

  const dockerPath = "Dockerfile";
  const dockerContents = read(repoRoot, dockerPath);
  const dockerPattern = new RegExp(`^ARG PP_APP_VERSION=${currentVersion.replaceAll(".", "\\.")}$`, "m");
  if (!dockerPattern.test(dockerContents)) {
    fail(`${dockerPath} PP_APP_VERSION does not match ${currentVersion}`);
  }
  add(
    dockerPath,
    dockerContents,
    dockerContents.replace(dockerPattern, `ARG PP_APP_VERSION=${validatedNextVersion}`),
  );

  for (const composePath of COMPOSE_FILES) {
    const composeContents = read(repoRoot, composePath);
    const composeExpression = `PRINT_PARTNER_VERSION:-${currentVersion}`;
    if (composeContents.split(composeExpression).length !== 2) {
      fail(`${composePath} must contain exactly one ${composeExpression} expression`);
    }
    add(
      composePath,
      composeContents,
      composeContents
        .replace(composeExpression, `PRINT_PARTNER_VERSION:-${validatedNextVersion}`)
        .replaceAll(LEGACY_IMAGE_REPOSITORY, IMAGE_REPOSITORY),
    );
  }

  for (const path of VERSIONED_DOCS) {
    const contents = read(repoRoot, path);
    add(
      path,
      contents,
      replaceMarkedVersions(contents, currentVersion, validatedNextVersion, path).replaceAll(
        LEGACY_IMAGE_REPOSITORY,
        IMAGE_REPOSITORY,
      ),
    );
  }

  const changelogPath = "CHANGELOG.md";
  const changelogContents = read(repoRoot, changelogPath);
  add(
    changelogPath,
    changelogContents,
    promoteChangelog(changelogContents, currentVersion, validatedNextVersion, validatedDate),
  );

  return { repoRoot, currentVersion, nextVersion: validatedNextVersion, date: validatedDate, changes };
}

export function applyReleasePlan(plan) {
  const staged = [];
  try {
    for (const change of plan.changes) {
      const temporaryPath = `${change.absolutePath}.release-tmp-${process.pid}`;
      writeFileSync(temporaryPath, change.after);
      staged.push({ ...change, temporaryPath });
    }
    for (const change of staged) renameSync(change.temporaryPath, change.absolutePath);
  } finally {
    for (const change of staged) {
      if (existsSync(change.temporaryPath)) unlinkSync(change.temporaryPath);
    }
  }
}

export function resolveTagCommit(repoRoot, tag) {
  parseTag(tag);
  return execFileSync("git", ["rev-parse", `refs/tags/${tag}^{}`], {
    cwd: repoRoot,
    encoding: "utf8",
  }).trim();
}

function inspectWorkspace(repoRoot) {
  const packageJson = json(read(repoRoot, "web/package.json"), "web/package.json");
  const version = parseAppVersion(packageJson.version);
  const lockJson = json(read(repoRoot, "web/package-lock.json"), "web/package-lock.json");
  if (lockJson.version !== version || lockJson.packages?.[""]?.version !== version) {
    fail(`web/package-lock.json root versions do not match ${version}`);
  }
  const dockerVersions = [
    ...read(repoRoot, "Dockerfile").matchAll(/^ARG PP_APP_VERSION=(\S+)$/gm),
  ].map((match) => match[1]);
  if (dockerVersions.length !== 1 || dockerVersions[0] !== version) {
    fail(`Dockerfile PP_APP_VERSION does not match ${version}`);
  }
  for (const composePath of COMPOSE_FILES) {
    const composeVersions = [
      ...read(repoRoot, composePath).matchAll(/PRINT_PARTNER_VERSION:-([^}]+)}/g),
    ].map((match) => match[1]);
    if (composeVersions.length !== 1 || composeVersions[0] !== version) {
      fail(`${composePath} default image does not match ${version}`);
    }
  }
  for (const path of VERSIONED_DOCS) {
    assertMarkedVersions(read(repoRoot, path), version, path);
  }
  if (!read(repoRoot, "CHANGELOG.md").includes(`## [${version}]`)) {
    fail(`CHANGELOG.md does not contain ${version}`);
  }
  return version;
}

export function checkRelease({ repoRoot, tag, commit }) {
  const version = inspectWorkspace(repoRoot);
  const status = execFileSync("git", ["status", "--porcelain"], {
    cwd: repoRoot,
    encoding: "utf8",
  }).trim();
  if (status) fail("Release check requires a clean working tree");
  if ((tag === undefined) !== (commit === undefined)) {
    fail("Release check requires both --tag and --commit together");
  }
  if (tag !== undefined && commit !== undefined) {
    const parsedTag = parseTag(tag);
    const parsedCommit = parseCommit(commit);
    if (parsedTag.version !== version) fail(`Tag ${tag} does not match package version ${version}`);
    const peeled = resolveTagCommit(repoRoot, tag);
    if (peeled !== parsedCommit) fail(`Tag ${tag} peels to ${peeled}, not ${parsedCommit}`);
    const head = execFileSync("git", ["rev-parse", "HEAD^{commit}"], {
      cwd: repoRoot,
      encoding: "utf8",
    }).trim();
    if (head !== parsedCommit) fail(`Checked-out commit ${head} does not match ${parsedCommit}`);
  }
  return { version, runtimeVersion: `${version}-web` };
}

export function renderReleaseIdentity({ version, tag, commit, digest }) {
  const parsedVersion = parseAppVersion(version);
  const parsedTag = parseTag(tag);
  const parsedCommit = parseCommit(commit);
  const parsedDigest = parseDigest(digest);
  if (parsedTag.version !== parsedVersion) fail(`Tag ${tag} does not match version ${parsedVersion}`);

  return `${JSON.stringify(
    {
      schema_version: 1,
      version: parsedVersion,
      runtime_version: `${parsedVersion}-web`,
      tag,
      commit: parsedCommit,
      image: {
        repository: IMAGE_REPOSITORY,
        digest: parsedDigest,
        expected_aliases: [parsedVersion],
        mutable_aliases: ["latest"],
      },
      supported_deployment_modes: ["self-host"],
      github_release_url: `https://github.com/${RELEASE_REPOSITORY}/releases/tag/${tag}`,
    },
    null,
    2,
  )}\n`;
}

function option(args, name) {
  const index = args.indexOf(name);
  if (index < 0) return undefined;
  const value = args[index + 1];
  if (!value || value.startsWith("--")) fail(`${name} requires a value`);
  return value;
}

function today() {
  return new Date().toISOString().slice(0, 10);
}

function repoRoot() {
  return resolve(dirname(fileURLToPath(import.meta.url)), "..");
}

export function run(argv, output = process.stdout) {
  const [command, positional] = argv;
  const root = repoRoot();
  if (command === "prepare") {
    const nextVersion = positional && !positional.startsWith("--") ? positional : undefined;
    if (!nextVersion) fail("Usage: node scripts/release.mjs prepare X.Y.Z [--dry-run]");
    const plan = planRelease({ repoRoot: root, nextVersion, date: option(argv, "--date") ?? today() });
    output.write(`Prepare ${plan.currentVersion} -> ${plan.nextVersion}\n`);
    for (const change of plan.changes) output.write(`  ${change.path}\n`);
    if (argv.includes("--dry-run")) {
      output.write("Dry run: no files changed.\n");
    } else {
      applyReleasePlan(plan);
      output.write("Release files updated. Commit these changes before tagging.\n");
    }
    return 0;
  }
  if (command === "check") {
    const result = checkRelease({
      repoRoot: root,
      tag: option(argv, "--tag"),
      commit: option(argv, "--commit"),
    });
    output.write(`Release identity is consistent: ${result.runtimeVersion}\n`);
    return 0;
  }
  if (command === "render-asset") {
    const tag = option(argv, "--tag");
    const commit = option(argv, "--commit");
    const digest = option(argv, "--digest");
    const destination = option(argv, "--output");
    if (!tag || !commit || !digest || !destination) {
      fail("render-asset requires --tag, --commit, --digest, and --output");
    }
    const version = inspectWorkspace(root);
    writeFileSync(
      resolve(root, destination),
      renderReleaseIdentity({ version, tag, commit, digest }),
    );
    output.write(`Wrote ${destination}\n`);
    return 0;
  }
  fail("Usage: node scripts/release.mjs prepare|check|render-asset");
}

const isMain = process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isMain) {
  try {
    process.exitCode = run(process.argv.slice(2));
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  }
}
