import { describe, expect, it } from "vitest";
import type { AppRepository } from "../db/repository.js";
import {
  categoryNotFoundError,
  countSourcesUnderCategory,
  existingSourceFor,
  sourceByName,
  sourceNotFoundError,
  summarizeSourceCategories,
} from "./source-tool-model.js";

type Source = { id: number; name: string; category: string | null; url?: string };

function repoStub(sources: Source[], categories: string[]): AppRepository {
  return {
    listSources: () => sources,
    getSourceCategories: () => categories,
  } as unknown as AppRepository;
}

describe("assistant source tool model", () => {
  it("resolves source names exactly, case-insensitively, and separator-insensitively", () => {
    const repo = repoStub(
      [
        { id: 1, name: "Voron-Parts", category: null },
        { id: 2, name: "Voron-Parts R2", category: null },
      ],
      [],
    );

    expect(sourceByName(repo, "Voron-Parts")?.id).toBe(1);
    expect(sourceByName(repo, "voron-parts")?.id).toBe(1);
    expect(sourceByName(repo, "Voron Parts")?.id).toBe(1);
    expect(sourceByName(repo, "Voron Parts R2 release")?.id).toBe(2);
  });

  describe("existingSourceFor", () => {
    const repo = repoStub(
      [
        { id: 44, name: "A4T", category: null, url: "https://github.com/Armchair-Heavy-Industries/A4T" },
        { id: 54, name: "Nevermore-StealthMax", category: null, url: "https://github.com/nevermore3d/StealthMax" },
        { id: 60, name: "---", category: null, url: "" },
      ],
      [],
    );

    it.each([
      ["A4Tv2", "https://github.com/DW-Tas/A4Tv2"],
      ["DW-Tas-A4Tv2", "https://github.com/DW-Tas/A4Tv2"],
      ["Nevermore-StealthMax-V2", null],
      ["StealthMax", null],
    ])("lets %s through when it only contains or is contained in another name", (name, url) => {
      expect(existingSourceFor(repo, name, url)).toBeNull();
      // Lookups by name still resolve these to the closest Source.
      expect(sourceByName(repo, name)).not.toBeNull();
    });

    it.each([
      ["A4T", null, 44],
      [" a4t ", null, 44],
      ["nevermore stealthmax", null, 54],
      ["Nevermore_StealthMax", null, 54],
      ["Renamed A4T", "https://github.com/armchair-heavy-industries/a4t.git", 44],
      ["Renamed A4T", "git@github.com:Armchair-Heavy-Industries/A4T.git", 44],
      ["Renamed A4T", "https://github.com/Armchair-Heavy-Industries/A4T/tree/main/", 44],
      ["---", null, 60],
      ["___", null, 60],
      [" - _ ", null, 60],
    ])("rejects %s from %s as Source %i", (name, url, id) => {
      expect(existingSourceFor(repo, name, url)?.id).toBe(id);
    });

    it("matches no Source by name when the name is blank", () => {
      expect(existingSourceFor(repo, "   ", null)).toBeNull();
    });
  });

  it("summarizes nested categories with direct and recursive counts", () => {
    const repo = repoStub(
      [
        { id: 1, name: "Base", category: "Printers" },
        { id: 2, name: "Frame", category: "Printers/Frame" },
        { id: 3, name: "Loose", category: null },
      ],
      ["Printers", "Printers/Frame"],
    );

    expect(countSourcesUnderCategory(repo, "Printers")).toBe(2);
    expect(summarizeSourceCategories(repo)).toMatchObject({
      separator: "/",
      uncategorized_sources: 1,
      categories: [
        { path: "Printers", sources: 1, sources_including_subcategories: 2 },
        { path: "Printers/Frame", sources: 1, sources_including_subcategories: 1 },
      ],
    });
  });

  it("formats Source and category errors with useful hints", () => {
    const repo = repoStub([{ id: 1, name: "Voron-Parts", category: null }], ["Printers"]);

    expect(JSON.parse(categoryNotFoundError(repo, "Unknown"))).toMatchObject({
      categories: ["Printers"],
    });
    expect(
      JSON.parse(sourceNotFoundError(repo, "Voron Part", "Call list_sources first.")).error,
    ).toContain("Did you mean");
  });
});
