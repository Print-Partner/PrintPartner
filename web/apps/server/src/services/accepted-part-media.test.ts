import { describe, expect, it } from "vitest";
import type { AcceptedOperationalPart } from "../db/accepted-plan-operational.js";
import { acceptedPartMediaIdentity } from "./accepted-part-media.js";

function part(
  overrides: Partial<
    Pick<
      AcceptedOperationalPart,
      "artifact" | "effectiveRole" | "filamentColorId" | "filamentCustomHex"
    >
  > = {},
): Pick<
  AcceptedOperationalPart,
  "artifact" | "effectiveRole" | "filamentColorId" | "filamentCustomHex"
> {
  return {
    artifact: {
      kind: "tracked",
      sourceId: 1,
      sourceRevisionId: 2,
      snapshotRoot: "/accepted/source",
      relativePath: "part.stl",
      expectedSha256: "a".repeat(64),
    },
    effectiveRole: " Primary ",
    filamentColorId: null,
    filamentCustomHex: null,
    ...overrides,
  };
}

describe("acceptedPartMediaIdentity", () => {
  it.each([
    { colorId: "asa::black", hex: "#000000", oldHex: "#bfb1a3" },
    { colorId: "asa::super-grey", hex: "#747874", oldHex: "#baa895" },
  ])(
    "renders $colorId in its filament color instead of packaging tan",
    ({ colorId, hex, oldHex }) => {
      const acceptedPart = part({ filamentColorId: colorId });
      for (const variant of ["mesh", "thumbnail", "preview"] as const) {
        const corrected = acceptedPartMediaIdentity(acceptedPart, variant);
        const previous = acceptedPartMediaIdentity(
          part({ filamentColorId: colorId, filamentCustomHex: oldHex }),
          variant,
        );

        expect(corrected.hex).toBe(hex);
        expect(corrected.basis).not.toBe(previous.basis);
      }
    },
  );

  it("returns the normalized accepted custom color and full derivative basis", () => {
    expect(
      acceptedPartMediaIdentity(part({ filamentCustomHex: " #AbCdEf " }), "thumbnail"),
    ).toEqual({
      hex: "#abcdef",
      basis: "6ee6d6c0172a7b2f06592dc1ec39cc27a39660943bb549e240064a5ea057a9bc",
    });
  });

  it("falls back to the bundled catalog only when the custom color is invalid", () => {
    const identity = acceptedPartMediaIdentity(
      part({
        filamentCustomHex: "not-a-color",
        filamentColorId: "abs-matte::black-pantone-7c",
      }),
      "preview",
    );

    expect(identity.hex).toMatch(/^#[0-9a-f]{6}$/);
    expect(identity.basis).toMatch(/^[0-9a-f]{64}$/);
  });

  it("uses no color when neither accepted color source resolves", () => {
    expect(
      acceptedPartMediaIdentity(
        part({ filamentCustomHex: "bad", filamentColorId: "missing-color" }),
        "mesh",
      ).hex,
    ).toBeNull();
  });

  it("rejects accepted Parts without tracked artifact identity", () => {
    expect(() =>
      acceptedPartMediaIdentity(
        part({ artifact: { kind: "unavailable", reason: "legacy" } }),
        "mesh",
      ),
    ).toThrow("Accepted Part artifact is unavailable");
  });
});
