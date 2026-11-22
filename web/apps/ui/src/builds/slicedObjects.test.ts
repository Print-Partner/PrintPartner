import { describe, expect, it } from "vitest";
import { part } from "./sendQueueFake.test-utils";
import { matchFilename, objectNames, proposeUnits, readObjectNames } from "./slicedObjects";

describe("objectNames", () => {
  it("reads EXCLUDE_OBJECT_DEFINE, M486, printing-object and Cura labels, each name once", () => {
    const text = [
      "EXCLUDE_OBJECT_DEFINE NAME=bracket.stl_id_0_copy_0 CENTER=10,10",
      "EXCLUDE_OBJECT_DEFINE NAME='lid part.stl' CENTER=20,20",
      'EXCLUDE_OBJECT_DEFINE NAME="Bracket.stl_id_0_copy_0"',
      'M486 S0 A"knob.stl"',
      "M486 A1",
      "M486 Aspacer.stl",
      "; printing object clip.stl id:3 copy 0",
      ";MESH:foot.stl",
      ";MESH:NONMESH",
    ].join("\n");
    expect(objectNames(text)).toEqual([
      "bracket.stl_id_0_copy_0",
      "lid part.stl",
      "knob.stl",
      "spacer.stl",
      "clip.stl",
      "foot.stl",
    ]);
  });

  it("prefers PrusaSlicer's objects_info, which lists every placed copy", () => {
    const text = [
      'M486 A"ignored.stl"',
      '; objects_info = {"objects":[{"name":"bracket.stl (Instance 1)"},{"name":"bracket.stl (Instance 1)"},{"name":" "}]}',
    ].join("\n");
    expect(objectNames(text)).toEqual(["bracket.stl (Instance 1)", "bracket.stl (Instance 1)"]);
  });

  it("finds no names in a file without object labels", () => {
    expect(objectNames("G28\nG1 X10 Y10\n")).toEqual([]);
  });

  it("reads the text runs of a .bgcode file", async () => {
    const label = new TextEncoder().encode("EXCLUDE_OBJECT_DEFINE NAME=bracket.stl\n");
    const bytes = new Uint8Array([0x47, 0x43, 0x44, 0x45, 0, 1, 2, ...label, 0, 0xff]);
    expect(await readObjectNames(new File([bytes], "plate.bgcode"))).toEqual(["bracket.stl"]);
  });
});

describe("matchFilename", () => {
  const library = ["bracket.stl", "frame_corner.stl", "z_tensioner_left.stl", "z_tensioner_right.stl"];

  it.each([
    ["bracket.stl_id_0_copy_1", "bracket.stl"],
    ["'bracket_stl__Instance_2_'", "bracket.stl"],
    ["bracket.stl (Instance 3)", "bracket.stl"],
    ["Bracket.STL", "bracket.stl"],
    ["frame_corner_02.stl", "frame_corner.stl"],
    ["z_tensionr_left.stl", "z_tensioner_left.stl"],
  ])("%s names %s", (name, filename) => {
    expect(matchFilename(name, library)).toBe(filename);
  });

  it.each([
    ["z_tensioner_rght.stl", library],
    ["frame_corner_1234", library],
    ["bracket.stl", ["kit-a/bracket.stl", "kit-b/bracket.stl"]],
    ["", library],
  ])("%s names no single file", (name, filenames) => {
    expect(matchFilename(name, filenames)).toBeNull();
  });
});

describe("proposeUnits", () => {
  it("gives each copy the next unit still to print, skipping printed ones", () => {
    const proposal = proposeUnits(
      ["bracket.stl_id_0_copy_0", "bracket.stl_id_0_copy_1", "bracket.stl_id_0_copy_2"],
      [part(11, "bracket.stl", 3, 1)],
    );
    expect(proposal.units).toEqual([
      { part_id: 11, unit_index: 1, object_name: "bracket.stl_id_0_copy_0" },
      { part_id: 11, unit_index: 2, object_name: "bracket.stl_id_0_copy_1" },
    ]);
    expect(proposal.perPart).toEqual([{ filename: "bracket.stl", count: 2 }]);
    expect(proposal.unmatched).toEqual(["bracket.stl_id_0_copy_2"]);
  });

  it("binds an export-style name to its own unit before file name matches take the rest", () => {
    const proposal = proposeUnits(["bracket.stl", "bracket_03"], [part(11, "bracket.stl", 3)]);
    expect(proposal.units.map((u) => u.unit_index)).toEqual([2, 0]);
  });

  it("links nothing for a name that matches no Part or two Parts", () => {
    const parts = [part(11, "bracket.stl", 1), part(12, "bracket.stl", 1), part(13, "lid.stl", 1)];
    const proposal = proposeUnits(["bracket.stl", "spoon.stl", "lid.stl"], parts);
    expect(proposal.units).toEqual([{ part_id: 13, unit_index: 0, object_name: "lid.stl" }]);
    expect(proposal.unmatched).toEqual(["bracket.stl", "spoon.stl"]);
  });

  it("sends the object name trimmed, at most 200 characters", () => {
    expect(proposeUnits(["lid_01 "], [part(13, "lid.stl", 1)]).units).toEqual([
      { part_id: 13, unit_index: 0, object_name: "lid_01" },
    ]);
    const filename = `${"a".repeat(250)}.stl`;
    const long = proposeUnits([`  ${filename}`], [part(14, filename, 1)]);
    expect(long.units[0]!.object_name).toBe(filename.slice(0, 200));
  });
});
