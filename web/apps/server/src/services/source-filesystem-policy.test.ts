import { afterEach, describe, expect, it } from "vitest";
import { mkdirSync, mkdtempSync, realpathSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  ISOLATED_SOURCE_FILESYSTEM,
  resolveSourceFilesystemRoot,
} from "./source-filesystem-policy.js";

const roots: string[] = [];

afterEach(() => {
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true });
});

function fixture() {
  const root = realpathSync(mkdtempSync(join(tmpdir(), "pp-source-filesystem-alias-")));
  roots.push(root);
  const repos = join(root, "repos");
  const alias = join(root, "repos-alias");
  const sourceRoot = join(repos, "7");
  const revision = join(sourceRoot, "revisions", "r1");
  mkdirSync(revision, { recursive: true });
  symlinkSync(repos, alias, "dir");
  const outside = join(root, "outside");
  mkdirSync(outside);
  return { root, repos, alias, sourceRoot, revision, outside };
}

describe("isolated Source filesystem aliases", () => {
  it("allows a canonical stored revision under an aliased repository directory", () => {
    const { alias, revision } = fixture();
    expect(resolveSourceFilesystemRoot(ISOLATED_SOURCE_FILESYSTEM, alias, 7, revision)).toBe(
      revision,
    );
  });

  it("allows a stored revision using the configured repository alias", () => {
    const { alias, revision } = fixture();
    expect(
      resolveSourceFilesystemRoot(
        ISOLATED_SOURCE_FILESYSTEM,
        alias,
        7,
        join(alias, "7", "revisions", "r1"),
      ),
    ).toBe(revision);
  });

  it("allows the canonical Source root under an aliased repository directory", () => {
    const { alias, sourceRoot } = fixture();
    expect(resolveSourceFilesystemRoot(ISOLATED_SOURCE_FILESYSTEM, alias, 7, sourceRoot)).toBe(
      sourceRoot,
    );
  });

  it("keeps canonical repository directories valid", () => {
    const { repos, revision } = fixture();
    expect(resolveSourceFilesystemRoot(ISOLATED_SOURCE_FILESYSTEM, repos, 7, revision)).toBe(
      revision,
    );
  });

  it.each(["repos", "alias"] as const)(
    "rejects outside and unrelated Source paths with %s configuration",
    (configured) => {
      const paths = fixture();
      const sibling = join(paths.repos, "8", "revisions", "r1");
      const prefixSibling = join(paths.repos, "70", "revisions", "r1");
      mkdirSync(sibling, { recursive: true });
      mkdirSync(prefixSibling, { recursive: true });
      for (const candidate of [
        paths.outside,
        sibling,
        prefixSibling,
        join(paths.alias, "8", "revisions", "r1"),
        join(paths.alias, "70", "revisions", "r1"),
      ]) {
        expect(
          resolveSourceFilesystemRoot(ISOLATED_SOURCE_FILESYSTEM, paths[configured], 7, candidate),
        ).toBeNull();
      }
    },
  );

  it("rejects a Source root symlink even under a repository alias", () => {
    const { alias, sourceRoot, outside } = fixture();
    rmSync(sourceRoot, { recursive: true });
    symlinkSync(outside, sourceRoot, "dir");
    expect(
      resolveSourceFilesystemRoot(ISOLATED_SOURCE_FILESYSTEM, alias, 7, sourceRoot),
    ).toBeNull();
    expect(
      resolveSourceFilesystemRoot(ISOLATED_SOURCE_FILESYSTEM, alias, 7, join(alias, "7")),
    ).toBeNull();
  });

  it("rejects a nested symlink escaping the Source workspace", () => {
    const { alias, sourceRoot, outside } = fixture();
    const escape = join(sourceRoot, "escape");
    symlinkSync(outside, escape, "dir");
    expect(resolveSourceFilesystemRoot(ISOLATED_SOURCE_FILESYSTEM, alias, 7, escape)).toBeNull();
    expect(
      resolveSourceFilesystemRoot(ISOLATED_SOURCE_FILESYSTEM, alias, 7, join(alias, "7", "escape")),
    ).toBeNull();
  });

  it("rejects a nested symlink to another Source workspace", () => {
    const { alias, repos, sourceRoot } = fixture();
    const other = join(repos, "8");
    mkdirSync(other);
    const escape = join(sourceRoot, "other-source");
    symlinkSync(other, escape, "dir");
    expect(resolveSourceFilesystemRoot(ISOLATED_SOURCE_FILESYSTEM, alias, 7, escape)).toBeNull();
    expect(
      resolveSourceFilesystemRoot(
        ISOLATED_SOURCE_FILESYSTEM,
        alias,
        7,
        join(alias, "7", "other-source"),
      ),
    ).toBeNull();
  });

  it("rejects an external shortcut that resolves inside the owned Source", () => {
    const { root, alias, sourceRoot } = fixture();
    const shortcut = join(root, "external-shortcut");
    symlinkSync(sourceRoot, shortcut, "dir");
    expect(resolveSourceFilesystemRoot(ISOLATED_SOURCE_FILESYSTEM, alias, 7, shortcut)).toBeNull();
  });

  it("rejects file, missing and null paths", () => {
    const { alias, sourceRoot } = fixture();
    const file = join(sourceRoot, "cube.stl");
    writeFileSync(file, "solid fixture\nendsolid fixture\n");
    for (const candidate of [file, join(sourceRoot, "missing"), null]) {
      expect(
        resolveSourceFilesystemRoot(ISOLATED_SOURCE_FILESYSTEM, alias, 7, candidate),
      ).toBeNull();
    }
  });

  it("fails closed when the configured repository directory is absent", () => {
    const { root, sourceRoot } = fixture();
    expect(
      resolveSourceFilesystemRoot(
        ISOLATED_SOURCE_FILESYSTEM,
        join(root, "missing-repos"),
        7,
        sourceRoot,
      ),
    ).toBeNull();
  });
});
