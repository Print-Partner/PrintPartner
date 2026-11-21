import { describe, expect, it } from "vitest";
import {
  fileSize,
  folderOf,
  groupByFolder,
  selectedSourceId,
  thumbnailUrl,
} from "./sourceFilesModel";

describe("source file list helpers", () => {
  it("groups by folder without requiring the same folder to be contiguous", () => {
    const groups = groupByFolder([
      { path: "README.md" },
      { path: "stl/a.stl" },
      { path: "stl/b/c.stl" },
      { path: "stl/d.stl" },
    ]);
    expect(groups.map((group) => [group.folder, group.files.map((file) => file.path)])).toEqual([
      ["", ["README.md"]],
      ["stl", ["stl/a.stl", "stl/d.stl"]],
      ["stl/b", ["stl/b/c.stl"]],
    ]);
    expect(folderOf("stl/b/c.stl")).toBe("stl/b");
  });

  it("prints sizes in bytes, KB and MB", () => {
    expect(fileSize(0)).toBe("0 B");
    expect(fileSize(1024)).toBe("1 KB");
    expect(fileSize(1536)).toBe("1.5 KB");
    expect(fileSize(20 * 1024)).toBe("20 KB");
    expect(fileSize(2.5 * 1024 * 1024)).toBe("2.5 MB");
  });

  it("accepts only a positive source id in the path", () => {
    expect(selectedSourceId(undefined)).toBeNull();
    expect(selectedSourceId("0")).toBeNull();
    expect(selectedSourceId("01")).toBeNull();
    expect(selectedSourceId("nope")).toBeNull();
    expect(selectedSourceId("4")).toBe(4);
  });

  it("builds the thumbnail URL from the file-list route", () => {
    expect(thumbnailUrl(4, "stl/arms/arm.stl")).toBe(
      "/api/v1/sources/4/thumbnail?path=stl%2Farms%2Farm.stl",
    );
  });
});

import { readdirSync } from "node:fs";
import { dirname, extname, join, parse } from "node:path";
import { fileURLToPath } from "node:url";

describe("case-insensitive filesystem safety", () => {
  it("has no sibling files under web/apps/ui/src that collide on macOS/Windows", () => {
    const srcRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
    const collisions: string[] = [];
    const walk = (dir: string) => {
      const seenNames = new Map<string, string>();
      const seenTsStems = new Map<string, string>();
      for (const entry of readdirSync(dir, { withFileTypes: true })) {
        const full = join(dir, entry.name);
        if (entry.isDirectory()) {
          walk(full);
          continue;
        }
        const lower = entry.name.toLowerCase();
        const prev = seenNames.get(lower);
        if (prev) collisions.push(`${dir}: ${prev} vs ${entry.name}`);
        else seenNames.set(lower, entry.name);

        const ext = extname(entry.name);
        if (ext === ".ts" || ext === ".tsx") {
          const stem = parse(entry.name).name.toLowerCase();
          const prevStem = seenTsStems.get(stem);
          if (prevStem && prevStem !== entry.name) {
            collisions.push(`${dir}: ${prevStem} vs ${entry.name}`);
          } else {
            seenTsStems.set(stem, entry.name);
          }
        }
      }
    };
    walk(srcRoot);
    expect(collisions).toEqual([]);
  });
});
