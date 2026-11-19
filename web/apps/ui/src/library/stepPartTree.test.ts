import { describe, expect, it } from "vitest";
import { isStepPath, nestParts } from "./StepPartTree";

const part = (name: string, depth: number) => ({ name, product_id: name, instance: null, depth });

describe("STEP part tree helpers", () => {
  it("nests a depth-first list by depth", () => {
    const nested = nestParts([
      part("A", 0),
      part("B", 1),
      part("C", 2),
      part("D", 3),
      part("E", 1),
      part("F", 0),
    ]);
    const names = (nodes: ReturnType<typeof nestParts>): unknown[] =>
      nodes.map((node) => [node.part.name, names(node.children)]);
    expect(names(nested)).toEqual([
      ["A", [["B", [["C", [["D", []]]]]], ["E", []]]],
      ["F", []],
    ]);
  });

  it("keeps a part whose parent is missing at the top level", () => {
    expect(nestParts([part("Orphan", 2)]).map((node) => node.part.name)).toEqual(["Orphan"]);
  });

  it("knows a STEP path by its extension in any case", () => {
    expect(["a.step", "b/c.STP", "d.Step"].every(isStepPath)).toBe(true);
    expect(["a.stl", "step", "a.step.txt"].some(isStepPath)).toBe(false);
  });
});
