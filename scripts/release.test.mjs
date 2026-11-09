import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { cpSync, existsSync, mkdtempSync, mkdirSync, readFileSync, rmSync, statSync, utimesSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  applyReleasePlan,
  checkRelease,
  parseAppVersion,
  planRelease,
  renderReleaseIdentity,
  resolveTagCommit,
} from "./release.mjs";

const REPO_ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");

function write(root, path, contents) {
  const absolute = join(root, path);
  mkdirSync(join(absolute, ".."), { recursive: true });
  writeFileSync(absolute, contents);
}

function fixture(context) {
  const root = mkdtempSync(join(tmpdir(), "print-partner-release-"));
  context.after(() => rmSync(root, { recursive: true, force: true }));
  write(root, "web/package.json", '{\n  "version": "3.1.0"\n}\n');
  write(
    root,
    "web/package-lock.json",
    '{\n  "version": "3.1.0",\n  "packages": { "": { "version": "3.1.0" }, "node_modules/example": { "version": "1.0.0" } }\n}\n',
  );
  write(root, "Dockerfile", "ARG PP_APP_VERSION=3.1.0\n");
  write(
    root,
    "docker-compose.yml",
    "image: example:${PRINT_PARTNER_VERSION:-3.1.0}\n# historical example: 3.1.0\n",
  );
  write(
    root,
    "docker-compose.hosted.yml",
    "image: ghcr.io/poitee/print-partner:${PRINT_PARTNER_VERSION:-3.1.0}\n",
  );
  const versioned = (text) =>
    `<!-- release-version:start -->\n${text}\n<!-- release-version:end -->\n`;
  write(root, "README.md", versioned("Current release: 3.1.0. Runtime: 3.1.0-web."));
  write(
    root,
    "web/DEPLOY.md",
    `${versioned("Deploy 3.1.0 with PP_VERSION=3.1.0-web.")}Historical v3.1.0 incident.\n`,
  );
  write(
    root,
    "OPERATIONS.md",
    `${versioned("Version: 3.1.0")}cosign verify "ghcr.io/poitee/print-partner@$DIGEST"\n`,
  );
  write(
    root,
    "CHANGELOG.md",
    "# Changelog\n\n## [Unreleased]\n\n### Added\n\n- New work.\n\n## [3.1.0] - 2026-08-15\n\n- Old work.\n\n[3.1.0]: https://example.test/v3.1.0\n",
  );
  return root;
}

test("parseAppVersion accepts release SemVer and rejects tags or suffixes", () => {
  assert.equal(parseAppVersion("3.2.0"), "3.2.0");
  for (const invalid of ["v3.2.0", "3.2.0-web", "3.2", "03.2.0", "3.2.0-beta.1"]) {
    assert.throws(() => parseAppVersion(invalid), /version/i);
  }
});

test("release dry-run plan updates every current sink without writing", (context) => {
  const root = fixture(context);
  const before = readFileSync(join(root, "web/package.json"), "utf8");

  const plan = planRelease({ repoRoot: root, nextVersion: "3.2.0", date: "2026-08-20" });

  assert.equal(readFileSync(join(root, "web/package.json"), "utf8"), before);
  assert.deepEqual(
    plan.changes.map((change) => change.path),
    [
      "web/package.json",
      "web/package-lock.json",
      "Dockerfile",
      "docker-compose.yml",
      "docker-compose.hosted.yml",
      "README.md",
      "web/DEPLOY.md",
      "OPERATIONS.md",
      "CHANGELOG.md",
    ],
  );
});

test("release preparation rejects impossible calendar dates", (context) => {
  const root = fixture(context);
  assert.throws(
    () => planRelease({ repoRoot: root, nextVersion: "3.2.0", date: "2026-02-30" }),
    /release date/i,
  );
});

test("applying a release plan preserves private dependency versions and changelog history", (context) => {
  const root = fixture(context);
  const plan = planRelease({ repoRoot: root, nextVersion: "3.2.0", date: "2026-08-20" });
  applyReleasePlan(plan);

  const lock = JSON.parse(readFileSync(join(root, "web/package-lock.json"), "utf8"));
  assert.equal(lock.version, "3.2.0");
  assert.equal(lock.packages[""].version, "3.2.0");
  assert.equal(lock.packages["node_modules/example"].version, "1.0.0");
  assert.match(
    readFileSync(join(root, "docker-compose.yml"), "utf8"),
    /historical example: 3\.1\.0/,
  );
  assert.equal(
    readFileSync(join(root, "docker-compose.hosted.yml"), "utf8"),
    "image: ghcr.io/print-partner/print-partner:${PRINT_PARTNER_VERSION:-3.2.0}\n",
  );
  assert.match(
    readFileSync(join(root, "OPERATIONS.md"), "utf8"),
    /cosign verify "ghcr\.io\/print-partner\/print-partner@\$DIGEST"\n$/,
  );

  const changelog = readFileSync(join(root, "CHANGELOG.md"), "utf8");
  assert.match(changelog, /## \[Unreleased\]\n\n## \[3\.2\.0\] - 2026-08-20/);
  assert.match(changelog, /## \[3\.1\.0\] - 2026-08-15/);
  assert.match(changelog, /\[Unreleased\]: .*v3\.2\.0\.\.\.HEAD/);
  assert.match(changelog, /\[3\.2\.0\]: .*releases\/tag\/v3\.2\.0/);
  assert.match(
    readFileSync(join(root, "web/DEPLOY.md"), "utf8"),
    /Historical v3\.1\.0 incident/,
  );
});

test("release identity output is deterministic and binds the digest to the peeled commit", () => {
  const input = {
    version: "3.2.0",
    tag: "v3.2.0",
    commit: "a".repeat(40),
    digest: `sha256:${"b".repeat(64)}`,
  };
  const first = renderReleaseIdentity(input);
  const second = renderReleaseIdentity(input);

  assert.equal(first, second);
  assert.deepEqual(JSON.parse(first), {
    schema_version: 1,
    version: "3.2.0",
    runtime_version: "3.2.0-web",
    tag: "v3.2.0",
    commit: "a".repeat(40),
    image: {
      repository: "ghcr.io/print-partner/print-partner",
      digest: `sha256:${"b".repeat(64)}`,
      expected_aliases: ["3.2.0"],
      mutable_aliases: ["latest"],
    },
    supported_deployment_modes: ["self-host"],
    github_release_url:
      "https://github.com/Print-Partner/PrintPartner/releases/tag/v3.2.0",
  });
});

test("release guide asks maintainers for the next version", () => {
  const currentVersion = JSON.parse(readFileSync(join(REPO_ROOT, "web/package.json"), "utf8")).version;
  const deployGuide = readFileSync(join(REPO_ROOT, "web/DEPLOY.md"), "utf8");

  assert.doesNotMatch(
    deployGuide,
    new RegExp(`release\\.mjs prepare ${currentVersion.replaceAll(".", "\\.")}`),
  );
  assert.match(deployGuide, /NEXT_VERSION=X\.Y\.Z/);
});

test("annotated tag validation uses the peeled commit, not the tag object", (context) => {
  const root = mkdtempSync(join(tmpdir(), "print-partner-tag-"));
  context.after(() => rmSync(root, { recursive: true, force: true }));
  execFileSync("git", ["init", "-q"], { cwd: root });
  execFileSync("git", ["config", "user.email", "release-test@example.test"], { cwd: root });
  execFileSync("git", ["config", "user.name", "Release Test"], { cwd: root });
  write(root, "tracked.txt", "one\n");
  execFileSync("git", ["add", "tracked.txt"], { cwd: root });
  execFileSync("git", ["commit", "-qm", "fixture"], { cwd: root });
  execFileSync("git", ["tag", "-a", "v3.2.0", "-m", "Release v3.2.0"], { cwd: root });

  const tagObject = execFileSync("git", ["rev-parse", "refs/tags/v3.2.0"], {
    cwd: root,
    encoding: "utf8",
  }).trim();
  const commit = execFileSync("git", ["rev-parse", "HEAD^{commit}"], {
    cwd: root,
    encoding: "utf8",
  }).trim();

  assert.notEqual(tagObject, commit);
  assert.equal(resolveTagCommit(root, "v3.2.0"), commit);
});

test("release check rejects a dirty worktree", (context) => {
  const root = fixture(context);
  execFileSync("git", ["init", "-q"], { cwd: root });
  execFileSync("git", ["config", "user.email", "release-test@example.test"], { cwd: root });
  execFileSync("git", ["config", "user.name", "Release Test"], { cwd: root });
  execFileSync("git", ["add", "."], { cwd: root });
  execFileSync("git", ["commit", "-qm", "fixture"], { cwd: root });
  assert.equal(checkRelease({ repoRoot: root }).version, "3.1.0");

  write(root, "README.md", `${readFileSync(join(root, "README.md"), "utf8")}dirty\n`);
  assert.throws(() => checkRelease({ repoRoot: root }), /working tree/i);
});

test("release check ignores commented versions and validates the active Docker argument", (context) => {
  const root = fixture(context);
  write(root, "Dockerfile", "# ARG PP_APP_VERSION=3.1.0\nARG PP_APP_VERSION=3.0.0\n");
  execFileSync("git", ["init", "-q"], { cwd: root });
  execFileSync("git", ["config", "user.email", "release-test@example.test"], { cwd: root });
  execFileSync("git", ["config", "user.name", "Release Test"], { cwd: root });
  execFileSync("git", ["add", "."], { cwd: root });
  execFileSync("git", ["commit", "-qm", "fixture"], { cwd: root });

  assert.throws(() => checkRelease({ repoRoot: root }), /Dockerfile PP_APP_VERSION/);
});

const ROOT_NOTICE = "THIRD_PARTY_NOTICES.md";
const SERVER_NOTICE = "web/apps/server/src/data/legal/THIRD_PARTY_NOTICES.md";
const STALE_NOTICE = `# Third-party notices

| Package | Version | License | URL |
|---------|---------|---------|-----|
| widget | 1.0.0 | MIT | https://example.test/widget |

Versions reflect the installed packages.
`;
const FRESH_NOTICE = `# Third-party notices

| Package | Version | License | URL |
|---------|---------|---------|-----|
| widget | 2.0.0 | MIT | https://example.test/widget |

Versions reflect the installed packages.
`;

function noticesRepo(context) {
  const root = mkdtempSync(join(tmpdir(), "print-partner-notices-"));
  context.after(() => rmSync(root, { recursive: true, force: true }));
  mkdirSync(join(root, "scripts"));
  cpSync(
    join(REPO_ROOT, "scripts/update-third-party-notices.mjs"),
    join(root, "scripts/update-third-party-notices.mjs"),
  );
  write(
    root,
    "web/package.json",
    '{"name":"notices-fixture","private":true,"workspaces":["apps/*","packages/*"]}\n',
  );
  write(
    root,
    "web/package-lock.json",
    '{"name":"notices-fixture","lockfileVersion":3,"requires":true,"packages":{"":{"name":"notices-fixture"},"node_modules/widget":{"version":"2.0.0","license":"MIT"}}}\n',
  );
  write(root, "web/apps/server/package.json", '{"dependencies":{"widget":"2.0.0"}}\n');
  write(root, "web/apps/web/package.json", '{"dependencies":{}}\n');
  write(root, "web/packages/contracts/package.json", '{"dependencies":{}}\n');
  write(root, "web/packages/domain/package.json", '{"dependencies":{}}\n');
  write(
    root,
    "web/node_modules/widget/package.json",
    '{"name":"widget","version":"2.0.0","license":"MIT","homepage":"https://example.test/widget"}\n',
  );
  mkdirSync(join(root, "web/apps/server/src/data/legal"), { recursive: true });
  return root;
}

function noticeSnapshot(root, relativePath) {
  const absolute = join(root, relativePath);
  if (!existsSync(absolute)) return { absent: true };
  return {
    absent: false,
    sha256: createHash("sha256").update(readFileSync(absolute)).digest("hex"),
    mtimeNs: statSync(absolute, { bigint: true }).mtimeNs.toString(),
  };
}

function pinNotice(root, relativePath) {
  const absolute = join(root, relativePath);
  const frozen = new Date("2020-01-01T00:00:00.000Z");
  utimesSync(absolute, frozen, frozen);
  return noticeSnapshot(root, relativePath);
}

function runNotices(root, args) {
  return spawnSync(
    process.execPath,
    [join(root, "scripts/update-third-party-notices.mjs"), ...args],
    { cwd: root, encoding: "utf8" },
  );
}

test("notices --check rejects a stale root copy without writing either file", (context) => {
  const root = noticesRepo(context);
  write(root, ROOT_NOTICE, STALE_NOTICE);
  write(root, SERVER_NOTICE, FRESH_NOTICE);
  const beforeRoot = pinNotice(root, ROOT_NOTICE);
  const beforeServer = pinNotice(root, SERVER_NOTICE);
  const legalDir = statSync(join(root, "web/apps/server/src/data/legal"), { bigint: true }).mtimeNs.toString();

  const result = runNotices(root, ["--check"]);

  assert.equal(result.status, 1, result.stderr);
  assert.equal(beforeRoot.sha256, createHash("sha256").update(STALE_NOTICE).digest("hex"));
  assert.deepEqual(noticeSnapshot(root, ROOT_NOTICE), beforeRoot);
  assert.deepEqual(noticeSnapshot(root, SERVER_NOTICE), beforeServer);
  assert.equal(statSync(join(root, "web/apps/server/src/data/legal"), { bigint: true }).mtimeNs.toString(), legalDir);
});

test("notices --check rejects a stale server copy without writing either file", (context) => {
  const root = noticesRepo(context);
  write(root, ROOT_NOTICE, FRESH_NOTICE);
  write(root, SERVER_NOTICE, STALE_NOTICE);
  const beforeRoot = pinNotice(root, ROOT_NOTICE);
  const beforeServer = pinNotice(root, SERVER_NOTICE);

  const result = runNotices(root, ["--check"]);

  assert.equal(result.status, 1, result.stderr);
  assert.equal(beforeServer.sha256, createHash("sha256").update(STALE_NOTICE).digest("hex"));
  assert.deepEqual(noticeSnapshot(root, ROOT_NOTICE), beforeRoot);
  assert.deepEqual(noticeSnapshot(root, SERVER_NOTICE), beforeServer);
});

test("notices list the dependencies of every declared workspace", (context) => {
  const root = noticesRepo(context);
  write(root, "web/apps/ui/package.json", '{"dependencies":{"router":"1.0.0"}}\n');
  write(
    root,
    "web/node_modules/router/package.json",
    '{"name":"router","version":"1.0.0","license":"MIT","homepage":"https://example.test/router"}\n',
  );
  write(root, ROOT_NOTICE, FRESH_NOTICE);
  write(root, SERVER_NOTICE, FRESH_NOTICE);

  const result = runNotices(root, []);

  assert.equal(result.status, 0, result.stderr);
  const notices = readFileSync(join(root, ROOT_NOTICE), "utf8");
  assert.match(notices, /\| router \| 1\.0\.0 \| MIT \| https:\/\/example\.test\/router \|/);
  assert.match(notices, /\| widget \| 2\.0\.0 \|/);
  assert.equal(readFileSync(join(root, SERVER_NOTICE), "utf8"), notices);
});

test("notices --check rejects a missing root copy without creating it", (context) => {
  const root = noticesRepo(context);
  write(root, SERVER_NOTICE, FRESH_NOTICE);
  const beforeServer = pinNotice(root, SERVER_NOTICE);

  const result = runNotices(root, ["--check"]);

  assert.equal(result.status, 1, result.stderr);
  assert.deepEqual(noticeSnapshot(root, ROOT_NOTICE), { absent: true });
  assert.deepEqual(noticeSnapshot(root, SERVER_NOTICE), beforeServer);
});

test("notices --check rejects a missing server copy without creating it", (context) => {
  const root = noticesRepo(context);
  write(root, ROOT_NOTICE, FRESH_NOTICE);
  const beforeRoot = pinNotice(root, ROOT_NOTICE);
  const legalDir = statSync(join(root, "web/apps/server/src/data/legal"), { bigint: true }).mtimeNs.toString();

  const result = runNotices(root, ["--check"]);

  assert.equal(result.status, 1, result.stderr);
  assert.deepEqual(noticeSnapshot(root, ROOT_NOTICE), beforeRoot);
  assert.deepEqual(noticeSnapshot(root, SERVER_NOTICE), { absent: true });
  assert.equal(statSync(join(root, "web/apps/server/src/data/legal"), { bigint: true }).mtimeNs.toString(), legalDir);
});

test("notices --check accepts two copies that match the installed table", (context) => {
  const root = noticesRepo(context);
  write(root, ROOT_NOTICE, FRESH_NOTICE);
  write(root, SERVER_NOTICE, FRESH_NOTICE);
  const beforeRoot = pinNotice(root, ROOT_NOTICE);
  const beforeServer = pinNotice(root, SERVER_NOTICE);

  const result = runNotices(root, ["--check"]);

  assert.equal(result.status, 0, result.stderr);
  assert.equal(beforeRoot.sha256, createHash("sha256").update(FRESH_NOTICE).digest("hex"));
  assert.deepEqual(noticeSnapshot(root, ROOT_NOTICE), beforeRoot);
  assert.deepEqual(noticeSnapshot(root, SERVER_NOTICE), beforeServer);
});

test("notices generation writes the installed table to both copies", (context) => {
  const root = noticesRepo(context);
  write(root, ROOT_NOTICE, STALE_NOTICE);

  const result = runNotices(root, []);

  assert.equal(result.status, 0, result.stderr);
  assert.equal(readFileSync(join(root, ROOT_NOTICE), "utf8"), FRESH_NOTICE);
  assert.equal(readFileSync(join(root, SERVER_NOTICE), "utf8"), FRESH_NOTICE);
});
