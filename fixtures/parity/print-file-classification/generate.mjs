// Writes the print-file classification goldens from Node's classifier.
// Run from web/ (needs fflate from web/node_modules):
//   node --conditions=development --import tsx ../fixtures/parity/print-file-classification/generate.mjs          # write
//   node --conditions=development --import tsx ../fixtures/parity/print-file-classification/generate.mjs --check  # verify, no writes
// rust/crates/pp-domain/tests/print_file_parity.rs replays cases.json.
import assert from "node:assert/strict";
import { readFileSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "../../..");
const classifierSource = "web/apps/server/src/lib/print-file-classification.ts";
const fixturesSource = "web/apps/server/src/test/print-file-fixtures.ts";
const out = join(here, "cases.json");
const check = process.argv.includes("--check");

const { classifyPrintFileBytes, printFileNextAction, printFileRejectionMessage, THREE_MF_ARCHIVE_LIMITS } =
  await import(pathToFileURL(join(root, classifierSource)).href);
const { bgcode, hostileZip, MODEL_XML } = await import(pathToFileURL(join(root, fixturesSource)).href);
const { strToU8, zipSync } = await import(
  pathToFileURL(createRequire(join(root, "web/apps/server/package.json")).resolve("fflate")).href
);

// The fixture helper's zipSync stamps the current local time into every entry,
// so the goldens build their own with a fixed time in UTC.
process.env.TZ = "UTC";
const threeMf = (entries) =>
  zipSync(Object.fromEntries(Object.entries(entries).map(([name, body]) => [name, strToU8(body)])), {
    mtime: new Date("2020-01-01T00:00:00Z"),
  });
const slicedThreeMf = () =>
  threeMf({
    "[Content_Types].xml": "<Types/>",
    "3D/3dmodel.model": MODEL_XML,
    "Metadata/project_settings.config": "{}",
    "Metadata/plate_1.gcode": "G28\nG1 X10 Y10 E1\n",
  });
const slicerProjectThreeMf = () =>
  threeMf({
    "[Content_Types].xml": "<Types/>",
    "3D/3dmodel.model": MODEL_XML,
    "Metadata/Slic3r_PE.config": "; layer_height = 0.2\n",
  });

const text = (value) => new TextEncoder().encode(value);
const bytes = (values) => new Uint8Array(values);
const limits = THREE_MF_ARCHIVE_LIMITS;
const bgcodeType = (type) =>
  bytes([0x47, 0x43, 0x44, 0x45, 1, 0, 0, 0, 0, 0, type, 0, 0, 0, 4, 0, 0, 0, 0, 0, 0x61, 0x3d, 0x31, 0x0a]);
const checksummed = bytes([
  0x47, 0x43, 0x44, 0x45, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 4, 0, 0, 0, 0, 0, 0x61, 0x3d, 0x31, 0x0a, 0xb4, 0xd1,
  0xf1, 0x51, 5, 0, 0, 0, 8, 0, 0, 0, 0, 0, 1, 0, 1, 0, 0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0xb0,
  0x19, 0xb3, 0x9f, 1, 0, 1, 0, 4, 0, 0, 0, 12, 0, 0, 0, 0, 0, 0x78, 0x9c, 0x73, 0x37, 0xb2, 0xe0, 2, 0, 2,
  0x30, 0, 0xbc, 0xeb, 0x94, 0xe7, 0xb9,
]);
const badVersion = bgcode([{ payload: 4 }]);
new DataView(badVersion.buffer).setUint32(4, 99, true);
const deep = Array.from({ length: limits.maxNestingDepth }, () => "d").join("/");
const perEntry = limits.maxEntryUncompressedBytes;
const sliced = slicedThreeMf();

// A ZIP64 archive: the classic end record saturates its count and offset, and
// a locator points at the ZIP64 end record that holds the real ones.
function zip64(names, { entryCount = names.length, recordOffset } = {}) {
  const plain = hostileZip(names.map((name) => ({ name })));
  const directory = plain.subarray(0, plain.length - 22);
  const record = directory.length;
  const out = new Uint8Array(record + 56 + 20 + 22);
  out.set(directory, 0);
  const view = new DataView(out.buffer);
  view.setUint32(record, 0x06064b50, true);
  view.setBigUint64(record + 32, BigInt(entryCount), true);
  view.setBigUint64(record + 48, 30n, true);
  view.setUint32(record + 56, 0x07064b50, true);
  view.setBigUint64(record + 64, BigInt(recordOffset ?? record), true);
  view.setUint32(record + 76, 0x06054b50, true);
  view.setUint16(record + 86, 0xffff, true);
  view.setUint32(record + 92, 0xffffffff, true);
  return out;
}

// Mirrors print-file-classification.test.ts, then adds edges the tests skip.
const cases = [
  ["3mf-slicer-project", threeMf({
    "[Content_Types].xml": "<Types/>",
    "3D/3dmodel.model": MODEL_XML,
    "Metadata/Slic3r_PE.config": "; prusaslicer_config = begin\nlayer_height = 0.2\n",
    "Metadata/Slic3r_PE_model.config": "<config/>",
    "Metadata/thumbnail.png": "not really a png",
  })],
  ["3mf-model-package", threeMf({ "[Content_Types].xml": "<Types/>", "3D/3dmodel.model": MODEL_XML, "_rels/.rels": "<Relationships/>" })],
  ["3mf-toolpath-with-slicer-state", sliced],
  ["3mf-slicer-project-fixture", slicerProjectThreeMf()],
  ["3mf-unsupported", threeMf({ "[Content_Types].xml": "<Types/>", "notes/readme.txt": "hello" })],
  ["3mf-uppercase-toolpath", threeMf({ "3D/3dmodel.model": MODEL_XML, "METADATA/PLATE_1.GCODE": "G28\n" })],
  ["3mf-ini-slicer-state", threeMf({ "3D/Objects/part.model": MODEL_XML, "Metadata/printer.ini": "x=1" })],
  ["3mf-directory-entry-only", hostileZip([{ name: "3D/3dmodel.model/" }])],
  ["3mf-unicode-name", hostileZip([{ name: "3D/Σ模型.model" }])],
  ["3mf-invalid-utf8-name", (() => {
    const zip = hostileZip([{ name: "3D/aa.model" }]);
    zip[30 + 46 + 3] = 0xff;
    return zip;
  })()],
  ["zip-entry-limit", hostileZip([{ name: "3D/3dmodel.model" }], { declaredEntryCount: limits.maxEntries + 1 })],
  ["zip-entry-too-large", hostileZip([{ name: "3D/3dmodel.model", uncompressedSize: perEntry + 1 }])],
  ["zip-total-too-large", hostileZip(
    Array.from({ length: Math.floor(limits.maxTotalUncompressedBytes / perEntry) + 1 }, (_unused, index) => ({
      name: `3D/part-${index}.model`,
      uncompressedSize: perEntry,
    })),
  )],
  ...["../../etc/passwd", "3D/../../escape.model", "/absolute/3dmodel.model", "C:/windows/3dmodel.model", "3D\\3dmodel.model", "3D/./x.model", "3D/a\u0000.model"].map(
    (name, index) => [`zip-unsafe-name-${index}`, hostileZip([{ name }])],
  ),
  ["zip-long-name", hostileZip([{ name: `3D/${"a".repeat(limits.maxEntryNameBytes)}.model` }])],
  ["zip-symlink", hostileZip([{ name: "3D/3dmodel.model", externalAttributes: 0xa1ff0000 }])],
  ["zip-nesting", hostileZip([{ name: `${deep}/3dmodel.model` }])],
  ["zip-nesting-at-cap", hostileZip([{ name: `${deep.slice(2)}/3dmodel.model` }])],
  ["zip-no-central-directory", bytes([0x50, 0x4b, 0x03, 0x04, 0, 0, 0, 0])],
  ["zip64-model-package", zip64(["3D/3dmodel.model", "_rels/.rels"])],
  ["zip64-entry-limit", zip64(["3D/3dmodel.model"], { entryCount: limits.maxEntries + 1 })],
  ["zip64-record-out-of-range", zip64(["3D/3dmodel.model"], { recordOffset: 10_000 })],
  ["zip64-record-past-safe-integer", zip64(["3D/3dmodel.model"], { recordOffset: 2 ** 60 })],
  ...[30, 60, 100, sliced.length - 23, sliced.length - 22, sliced.length - 1].map((end) => [
    `3mf-truncated-at-${end}`,
    sliced.subarray(0, end),
  ]),
  ["gcode-long-header", text(`; generated by a slicer\n${"; thumbnail data\n".repeat(500)}G28 ; home\nG1 X10 Y10 E1\n`)],
  ["gcode-indented-lowercase", text("; c\r\n  m104 S200\r\n")],
  ["gcode-comment-only", text("; only comments\n; nothing to run\n")],
  ["gcode-letter-without-digit", text("G\nMX\n")],
  ["gcode-trailing-command-letter", text("; x\nG")],
  ["gcode-control-byte", text("G28\n\u0001\n")],
  ["gcode-nul", bytes([0x47, 0x31, 0x00])],
  ["gcode-high-bytes", bytes([0x47, 0x31, 0x20, 0xc3, 0xa9, 0x0a])],
  ["binary-noise", bytes([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a])],
  ["empty", bytes([])],
  ["bgcode-two-blocks", bgcode([{ payload: 16 }, { payload: 4 }])],
  ["bgcode-truncated", bgcode([{ payload: 16 }]).slice(0, 20)],
  ["bgcode-bad-version", badVersion],
  ["bgcode-header-only", bgcode([])],
  ["bgcode-short-header", bytes([0x47, 0x43, 0x44, 0x45, 1, 0])],
  ...[0, 1, 2, 3, 4].map((type) => [`bgcode-type-${type}`, bgcodeType(type)]),
  ["bgcode-thumbnail", bytes([0x47, 0x43, 0x44, 0x45, 1, 0, 0, 0, 0, 0, 5, 0, 0, 0, 8, 0, 0, 0, 0, 0, 1, 0, 1, 0, 0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])],
  ["bgcode-compressed", bytes([0x47, 0x43, 0x44, 0x45, 1, 0, 0, 0, 1, 0, 1, 0, 1, 0, 4, 0, 0, 0, 12, 0, 0, 0, 0, 0, 0x78, 0x9c, 0x73, 0x37, 0xb2, 0xe0, 2, 0, 2, 0x30, 0, 0xbc, 0xeb, 0x94, 0xe7, 0xb9])],
  ...[0, 1, 2, 3, 4, 5].map((type) => [
    `bgcode-missing-parameters-${type}`,
    bytes([0x47, 0x43, 0x44, 0x45, 1, 0, 0, 0, 0, 0, type, 0, 0, 0, 0, 0, 0, 0]),
  ]),
  ["bgcode-checksummed-chain", checksummed],
  ...[9, 17, 18, 19, 23, 27, 35, 36, 41, 49, 53, 61, 65, 67, 79, 83].map((end) => [
    `bgcode-checksummed-truncated-at-${end}`,
    checksummed.subarray(0, end),
  ]),
  ["bgcode-unknown-type", bytes([0x47, 0x43, 0x44, 0x45, 1, 0, 0, 0, 0, 0, 6, 0, 0, 0, 4, 0, 0, 0, 0, 0, 0, 0])],
  ["bgcode-bad-checksum-type", bytes([0x47, 0x43, 0x44, 0x45, 1, 0, 0, 0, 2, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0])],
];

const golden = {
  _generated: "@generated by fixtures/parity/print-file-classification/generate.mjs",
  source: [classifierSource, fixturesSource],
  cases: cases.map(([id, input]) => {
    const result = classifyPrintFileBytes(input);
    const expected =
      result.outcome === "classified"
        ? { ...result, next_action: printFileNextAction(result.classification) }
        : { ...result, message: printFileRejectionMessage(result.reason) };
    return { id, bytes: Buffer.from(input).toString("base64"), expected };
  }),
};
assert.equal(new Set(golden.cases.map((entry) => entry.id)).size, golden.cases.length);
// One case per line keeps the golden reviewable and its diffs small.
const body = `{"_generated": ${JSON.stringify(golden._generated)},\n"source": ${JSON.stringify(golden.source)},\n"cases": [\n${golden.cases
  .map((entry) => JSON.stringify(entry))
  .join(",\n")}\n]}\n`;
if (check) {
  assert.equal(readFileSync(out, "utf8"), body, "cases.json is stale; rerun without --check");
  console.log(`print-file-classification: ${golden.cases.length} cases match`);
} else {
  writeFileSync(out, body);
  console.log(`print-file-classification: wrote ${golden.cases.length} cases`);
}
