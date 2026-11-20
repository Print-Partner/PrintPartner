import { describe, expect, it } from "vitest";
import {
  DEFAULT_STL_NAMING_PROFILE,
  parseSourceNamingEndpointError,
  parseSourceNamingPutInput,
  parseSourceNamingResponse,
  parseStlNamingProfile,
  parseStlNamingProfileOverride,
} from "./source-naming.js";

const digest = "0".repeat(64);

describe("Source naming contracts", () => {
  it("parses both write variants", () => {
    expect(parseSourceNamingPutInput({ use_defaults: true })).toEqual({
      use_defaults: true,
    });
    expect(
      parseSourceNamingPutInput({
        use_defaults: false,
        override: DEFAULT_STL_NAMING_PROFILE,
      }),
    ).toMatchObject({ use_defaults: false, override: DEFAULT_STL_NAMING_PROFILE });
  });

  it("limits markers to 64 units and symbol-bounded markers to 16", () => {
    const profile = (markers: string[]) => ({
      ...DEFAULT_STL_NAMING_PROFILE,
      roles: [{ id: "primary", label: "Primary", markers }],
    });
    const letters = Array.from({ length: 513 }, (_, i) => `missing-marker-${i}`);
    expect(() => parseStlNamingProfile(profile(letters))).not.toThrow();
    expect(() => parseStlNamingProfile(profile(Array(16).fill("[".repeat(64))))).not.toThrow();
    expect(() => parseStlNamingProfile(profile(Array(17).fill("[a]")))).toThrow("at most 16 symbol");
    for (const [marker, ok] of [["[".repeat(64), true], ["[".repeat(65), false], ["😀".repeat(32), true],
      ["😀".repeat(32) + "!", false], ["A" + "😀".repeat(31) + "B", true], ["A" + "😀".repeat(31) + "BB", false]] as const) {
      const parse = () => parseStlNamingProfile(profile([marker]));
      if (ok) expect(parse).not.toThrow(); else expect(parse).toThrow("at most 64 characters");
    }
  });

  it("preserves sparse nested overrides", () => {
    expect(
      parseStlNamingProfileOverride({
        roles: [{ id: "accent", markers: ["[accent]"] }],
        quantity: { default: 2 },
        slug: { strip_quantity: false },
      }),
    ).toEqual({
      roles: [{ id: "accent", markers: ["[accent]"] }],
      quantity: { default: 2 },
      slug: { strip_quantity: false },
    });
  });

  it.each([0, -1, 1.5, Number.POSITIVE_INFINITY, "2", null])(
    "rejects invalid default quantity %s",
    (defaultQuantity) => {
      expect(() =>
        parseSourceNamingPutInput({
          use_defaults: false,
          override: {
            ...DEFAULT_STL_NAMING_PROFILE,
            quantity: { ...DEFAULT_STL_NAMING_PROFILE.quantity, default: defaultQuantity },
          },
        }),
      ).toThrow(/quantity\.default/i);
    },
  );

  it("rejects malformed folder rules", () => {
    expect(() =>
      parseSourceNamingPutInput({
        use_defaults: false,
        override: {
          ...DEFAULT_STL_NAMING_PROFILE,
          folder_rules: [{ path_contains: "", role_id: "accent" }],
        },
      }),
    ).toThrow(/folder_rules/i);
  });

  it("parses a response without expanding its sparse override", () => {
    expect(
      parseSourceNamingResponse({
        use_defaults: false,
        override: { quantity: { default: 2 } },
        effective: { ...DEFAULT_STL_NAMING_PROFILE, quantity: { regex: "x([0-9]+)", default: 2 } },
        effective_digest: digest,
      }),
    ).toMatchObject({ override: { quantity: { default: 2 } } });
  });

  it("rejects malformed responses", () => {
    expect(() =>
      parseSourceNamingResponse({
        use_defaults: false,
        override: {},
        effective: { quantity: { regex: "x([0-9]+)", default: 1 } },
        effective_digest: 7,
      }),
    ).toThrow(/source naming response/i);
  });

  it("requires an endpoint error code to match its HTTP status", () => {
    expect(
      parseSourceNamingEndpointError(
        { code: "source_not_found", detail: "Source not found" },
        404,
      ),
    ).toMatchObject({ code: "source_not_found" });
    expect(() =>
      parseSourceNamingEndpointError(
        { code: "source_not_found", detail: "Source not found" },
        400,
      ),
    ).toThrow(/status/i);
  });
});
