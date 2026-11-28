// Generates the kit bundle parity golden (GRE-425) from Node's real
// export-kit.ts and kit-bundle-share.ts. Run from the repo root after
// `npm --prefix web ci`:
//   node fixtures/parity/kit-bundle/generate.mjs          # write
//   node fixtures/parity/kit-bundle/generate.mjs --check  # verify, no writes
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { createRequire } from "node:module";
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "../../..");
const sources = ["web/apps/server/src/services/export-kit.ts", "web/apps/server/src/services/kit-bundle-share.ts"];
const output = join(here, "kit-bundle.json");
const MAX = 16 * 1024 * 1024;
const MTIME = new Date("2026-01-01T00:00:00Z");

const kit = (extra) => JSON.stringify({ format: "print-partner-kit", version: 3, ...extra });
// Same padding as export-kit.test.ts; the Rust test builds it the same way.
const padded = (total) => {
  const prefix = Buffer.from('{"format":"print-partner-kit","version":3,"padding":"');
  const suffix = Buffer.from('"}');
  return Buffer.concat([prefix, Buffer.alloc(total - prefix.length - suffix.length, 0x61), suffix]);
};
// Rewrite the uncompressed size in the local and central headers.
const forgeSize = (archive, size) => {
  const forged = Buffer.from(archive);
  forged.writeUInt32LE(size, forged.indexOf(Buffer.from([0x50, 0x4b, 0x03, 0x04])) + 22);
  forged.writeUInt32LE(size, forged.lastIndexOf(Buffer.from([0x50, 0x4b, 0x01, 0x02])) + 24);
  return forged;
};

const recipe = {
  profile: { id: 7, name: "Voron 2.4   r2", orderNumber: null },
  layers: [
    { layer_order: 0, layer_type: "base", project: { name: "Voron", url: "https://github.com/VoronDesign/Voron-2", branch: "main", source_kind: "github", source_type: "git", role: "base", category: "Printer kits", import_rules: ["STLs/"] } },
    { layer_order: 1, layer_type: "addon", project: null, weight: 1.5, huge: 1e21, tiny: 1e-7, "10": "ten", "2": "two" },
  ],
  sources: [{ name: "Voron", url: "https://github.com/VoronDesign/Voron-2", branch: "main", source_kind: "github", source_type: "git", role: "base", category: "Printer kits", import_rules: [], tag: "v2.4r2", manifest_community_slug: "voron-2-4" }],
  kitManifest: { version: 1, groups: [], notes: "tab\there, ctrl \u0001, quote \" and \\ slash", empty: {} },
  workingParts: [
    { matchKey: "base:STLs/a.stl", relativePath: "STLs/a.stl", filename: "a.stl", sourceLayer: "base", status: "matched", role: "printed", filamentColorId: "primary", filamentCustomHex: null, quantityInferred: 2, quantityOverride: null, quantityEffective: 2, included: true, notes: "", geometrySame: null, requirement: null, optionGroupId: null, manifestSource: null },
    { matchKey: "addon:b.stl", relativePath: "b.stl", filename: "b é😀.stl", sourceLayer: "addon", status: "new", role: "accent", filamentColorId: null, filamentCustomHex: "#FF0000", quantityInferred: 0, quantityOverride: 4, quantityEffective: 4, included: false, notes: "line\nbreak", geometrySame: true, requirement: "required", optionGroupId: "g1", manifestSource: "community" },
  ],
};

const acceptedParts = [
  {
    ...recipe.workingParts[1],
    partKey: "accepted:base/selector.stl",
    relativePath: "base/selector.stl",
    filename: "selector.stl",
    sourceLayer: "base",
    included: true,
    quantityEffective: 3,
    quantityOverride: 3,
    notes: "Accepted before the working edit",
    units: [{ completed: false }, { completed: true }, { completed: false }],
  },
  {
    ...recipe.workingParts[0],
    partKey: "accepted:addon/cover.stl",
    relativePath: "addon/cover.stl",
    filename: "cover.stl",
    sourceLayer: "addon",
    quantityEffective: 1,
    units: [{ completed: true }],
  },
].map(({ matchKey, ...part }) => part);

const acceptedCases = [
  { id: "accepted_basis", parts: acceptedParts },
  { id: "accepted_basis_without_units", parts: [{
    ...acceptedParts[0], quantityInferred: 0, quantityOverride: null, quantityEffective: 0, units: [],
  }] },
  { id: "no_accepted_basis", parts: null },
];

// Each parse input is { id, filename, hex } or a { recipe } both sides build.
async function parseInputs(fflate) {
  const zip = (files, level = 6) => Buffer.from(fflate.zipSync(Object.fromEntries(
    Object.entries(files).map(([name, text]) => [name, [Buffer.from(text), { level, mtime: MTIME }]]),
  )));
  const stored = Buffer.from(fflate.zipSync({ "kit.json": [Buffer.from(kit({})), { level: 0, mtime: MTIME }] }));
  const badCrc = Buffer.from(stored);
  badCrc[badCrc.indexOf(Buffer.from('"version"'))] ^= 0x20;
  const bytes = [
    ["json_v3", "kit.json", Buffer.from(kit({ profile: { name: "A" } }))],
    ["json_v1", "kit.json", Buffer.from(kit({ version: 1 }))],
    ["json_v2_no_filename", null, Buffer.from(kit({ version: 2 }))],
    ["version_string", "kit.json", Buffer.from(kit({ version: " 3 " }))],
    ["version_hex", "kit.json", Buffer.from(kit({ version: "0x2" }))],
    ["version_exponent", "kit.json", Buffer.from(kit({ version: "3e0" }))],
    ["version_true", "kit.json", Buffer.from(kit({ version: true }))],
    ["version_array", "kit.json", Buffer.from(kit({ version: [2] }))],
    ["version_float", "kit.json", Buffer.from(kit({ version: 2.0000000000000001 }))],
    ["version_null", "kit.json", Buffer.from(kit({ version: null }))],
    ["version_missing", "kit.json", Buffer.from('{"format":"print-partner-kit"}')],
    ["version_4", "kit.json", Buffer.from(kit({ version: 4 }))],
    ["version_half", "kit.json", Buffer.from(kit({ version: 2.5 }))],
    ["version_infinity", "kit.json", Buffer.from(kit({ version: "Infinity" }))],
    ["version_object", "kit.json", Buffer.from(kit({ version: {} }))],
    ["version_inf_word", "kit.json", Buffer.from(kit({ version: "inf" }))],
    ["format_other", "kit.json", Buffer.from(kit({ format: "other" }))],
    ["format_case", "kit.json", Buffer.from(kit({ format: "Print-Partner-Kit" }))],
    ["array", "kit.json", Buffer.from("[]")],
    ["string", "kit.json", Buffer.from('"print-partner-kit"')],
    ["number", "kit.json", Buffer.from("3")],
    ["null", "kit.json", Buffer.from("null")],
    ["syntax", "kit.json", Buffer.from('{"format":')],
    ["empty", "kit.json", Buffer.alloc(0)],
    ["bom", "kit.json", Buffer.concat([Buffer.from([0xef, 0xbb, 0xbf]), Buffer.from(kit({}))])],
    ["trailing_comma", "kit.json", Buffer.from('{"format":"print-partner-kit","version":3,}')],
    ["key_order_and_numbers", "kit.json", Buffer.from('{"format":"print-partner-kit","version":3,"b":1,"10":2,"2":3,"01":4,"4294967295":5,"4294967294":6,"big":12345678901234567890,"f":1.50,"neg0":-0,"exp":1E21,"small":5e-7,"dup":1,"dup":{"z":1,"0":2}}')],
    ["invalid_utf8", "kit.json", Buffer.concat([
      Buffer.from('{"format":"print-partner-kit","version":3,"name":"'),
      Buffer.from([0x61, 0xff, 0x62, 0xc3, 0x63, 0xe2, 0x82, 0x64, 0xf0, 0x9f, 0x98, 0x65, 0xed, 0xa0, 0x80, 0x66, 0xc0, 0x80, 0x67]),
      Buffer.from('"}'),
    ])],
    ["version_overflow", "kit.json", Buffer.from('{"format":"print-partner-kit","version":1e999}')],
    ["overflowing_numbers", "kit.json", Buffer.from('{"format":"print-partner-kit","version":3,"layers":[{"w":1e999,"v":-2E+400,"s":"1e999","u":1e-999}]}')],
    ["overflow_in_invalid_json", "kit.json", Buffer.from('{"format":"print-partner-kit","version":3,"w":01e999}')],
    ["escapes", "kit.json", Buffer.from('{"format":"print-partner-kit","version":3,"s":"\\u0041\\n\\u2028\\ud83d\\ude00\\/"}')],
    ["zip", "Bundle.print-partner-kit.zip", zip({ "kit.json": kit({ profile: { name: "Z" } }) })],
    ["zip_by_magic", "upload.bin", zip({ "kit.json": kit({}) })],
    ["zip_no_filename", null, zip({ "kit.json": kit({}) })],
    ["zip_extra_entries", "a.zip", zip({ "readme.txt": "hi", "kit.json": kit({ version: 1 }), "models/a.stl": "solid" })],
    ["zip_missing_kit_json", "a.zip", zip({ "other.json": kit({}) })],
    ["zip_nested_kit_json", "a.zip", zip({ "dir/kit.json": kit({}) })],
    ["zip_case_kit_json", "a.zip", zip({ "KIT.JSON": kit({}) })],
    ["zip_null", "a.zip", zip({ "kit.json": "null" })],
    ["zip_not_kit", "a.zip", zip({ "kit.json": "{}" })],
    ["zip_syntax", "a.zip", zip({ "kit.json": "{" })],
    ["zip_stored", "a.zip", stored],
    ["zip_bad_crc", "a.zip", badCrc],
    ["zip_name_but_json", "kit.zip", Buffer.from(kit({}))],
    ["kit_name_but_json", "Kit.PRINT-PARTNER-KIT.json", Buffer.from(kit({}))],
    ["pk_prefix_not_zip", "kit.json", Buffer.from("PK not a zip")],
    ["zip_declared_oversize", "a.zip", forgeSize(zip({ "kit.json": padded(128).toString() }, 9), MAX + 1)],
    ["zip_deflated_understated", "a.zip", forgeSize(zip({ "kit.json": padded(MAX + 1).toString() }, 9), 128)],
    ["zip_deflated_at_limit", "a.zip", zip({ "kit.json": padded(MAX).toString() }, 9)],
    ["zip_deflated_overstated", "a.zip", forgeSize(zip({ "kit.json": kit({}) }, 9), 1000)],
    ["zip_stored_overstated", "a.zip", forgeSize(stored, 1000)],
    ["zip_stored_understated_small", "a.zip", forgeSize(stored, 5)],

  ];
  return [
    ...bytes.map(([id, filename, buf]) => ({ id, filename, buf })),
    { id: "json_at_limit", filename: "kit.json", recipe: { padded: MAX } },
    { id: "json_over_limit", filename: "kit.json", recipe: { padded: MAX + 1 } },
    { id: "zip_stored_understated", filename: "a.zip", recipe: { padded: MAX + 1, stored_declared: 128 } },
    { id: "zip_stored_bad_crc_oversize", filename: "a.zip", recipe: { padded: MAX + 1, stored_declared: 128, flip_version: true } },
  ].map((input) => {
    if (!input.recipe) return input;
    const { padded: total, stored_declared: declared, flip_version: flip } = input.recipe;
    if (!declared) return { ...input, buf: padded(total) };
    const buf = forgeSize(Buffer.from(fflate.zipSync({ "kit.json": [padded(total), { level: 0, mtime: MTIME }] })), declared);
    // Flipping a stored byte breaks the entry's CRC.
    if (flip) buf[buf.indexOf(Buffer.from('"version"'))] ^= 0x20;
    return { ...input, buf };
  });
}

const sourceRefCases = [
  { id: "merge_by_url_and_name", data: {
    sources: [
      { name: "Voron", url: " HTTPS://GitHub.com/VoronDesign/Voron-2 ", branch: "", tag: "  v1 ", role: "unassigned", category: "unassigned", import_rules: ["STLs/"] },
      { name: "Local kit", url: "", role: "ADDON", import_rules: ["a/"] },
      { name: "", url: "" },
      { name: "  ", url: "\t" },
      null, 5, "str", [1, 2],
    ],
    layers: [
      { layer_type: "addon", project: { name: "Voron dup", url: "https://github.com/vorondesign/voron-2", branch: "dev", tag: "v2", role: "base", import_rules: ["STLs/", "extra/"], manifest_community_slug: "voron" } },
      { layer_type: "base", project: { name: "LOCAL KIT", url: "" } },
      { layer_type: 7, project: { name: "Seven", url: "u7" } },
      { layer_type: "", project: { name: "Blank layer", url: "u8" } },
      { layer_type: null, project: { name: "Null layer", url: "u9" } },
      { project: [1] }, { project: null }, 5, "layer", [],
    ],
  } },
  { id: "odd_values", data: { sources: [
    { name: 123, url: true, branch: null, tag: 5, source_kind: null, source_type: "", role: "Printer", import_rules: [1, null, "", "a/", [1, [2, null]], {}, false, 1e21] },
    { name: "Category given", url: "u2", role: "  ", category: " Tools " },
    { name: "Unicode ﻿", url: "﻿u3\u0085", role: "﻿Base　", manifest_community_slug: " oracle-kit " },
    { name: "Blank slug", url: "u4", manifest_community_slug: "  " },
    { name: "Object name", url: { a: 1 }, branch: ["x", "y"], import_rules: "not-array" },
  ] } },
  { id: "invalid_slug", data: { sources: [{ name: "A", url: "u", manifest_community_slug: "Bad Slug" }] } },
  { id: "reserved_slug", data: { sources: [{ name: "A", url: "u", manifest_community_slug: "registry-index" }] } },
  { id: "non_string_slug", data: { sources: [{ name: "A", url: "u", manifest_community_slug: 5 }] } },
  { id: "slug_without_name_is_ignored", data: { sources: [{ manifest_community_slug: "Bad Slug" }] } },
  { id: "sources_null", data: { sources: null, layers: null } },
  { id: "sources_missing", data: {} },
  { id: "sources_string", data: { sources: "abc", layers: "abc" } },
  { id: "sources_object", data: { sources: { name: "A" } } },
  { id: "sources_number", data: { sources: 0 } },
  { id: "layers_false", data: { layers: false } },
  { id: "layer_null", data: { layers: [null] } },
];

const projectRows = [
  { id: "full", row: { name: "Voron", url: "https://x/voron", branch: "dev", tag: "v1", sourceKind: "github", sourceType: "git", role: "base", importedPaths: '["a", "b.STL", "\\\\c\\\\", "  ", 5, "//d/", " e "]', metadataJson: '{"category":" Toolheads "}', manifestCommunitySlug: " voron " } },
  { id: "defaults", row: { name: "Local", url: "", branch: null, tag: null, sourceKind: null, sourceType: null, role: null, importedPaths: null, metadataJson: null, manifestCommunitySlug: null } },
  { id: "empty_tag_and_reserved_slug", row: { name: "A", url: "u", branch: "", tag: "", sourceKind: "", sourceType: "", role: "", importedPaths: "", metadataJson: "", manifestCommunitySlug: "registry-index" } },
  { id: "bad_slug", row: { name: "A", url: "u", branch: null, tag: null, sourceKind: null, sourceType: null, role: "ADDON", importedPaths: "not json", metadataJson: "[1]", manifestCommunitySlug: "Bad Slug" } },
  { id: "category_null", row: { name: "A", url: "u", branch: null, tag: null, sourceKind: null, sourceType: null, role: "base", importedPaths: "{}", metadataJson: '{"category":null}', manifestCommunitySlug: "kit-catalog" } },
  { id: "category_blank", row: { name: "A", url: "u", branch: null, tag: null, sourceKind: null, sourceType: null, role: "addon", importedPaths: " [] ", metadataJson: '{"category":"   "}', manifestCommunitySlug: "" } },
  { id: "metadata_not_json", row: { name: "A", url: "u", branch: null, tag: null, sourceKind: null, sourceType: null, role: " Base ", importedPaths: null, metadataJson: "not json", manifestCommunitySlug: null } },
  { id: "metadata_number_category", row: { name: "A", url: "u", branch: null, tag: null, sourceKind: null, sourceType: null, role: "base", importedPaths: null, metadataJson: '{"category":5}', manifestCommunitySlug: null } },
  ...["constructor", "__proto__", "__defineGetter__", "toString", "valueOf"].map((role) => ({
    id: `role_${role.toLowerCase()}`,
    row: { name: "A", url: "u", branch: null, tag: null, sourceKind: null, sourceType: null, role, importedPaths: null, metadataJson: null, manifestCommunitySlug: null },
  })),
];

if (process.argv.includes("--probe")) {
  const exportKit = await import(pathToFileURL(join(root, sources[0])).href);
  const share = await import(pathToFileURL(join(root, sources[1])).href);
  const serverRequire = createRequire(join(root, "web/apps/server/package.json"));
  const fflate = serverRequire("fflate");
  const refused = (error) => {
    const exact = [exportKit.KIT_JSON_TOO_LARGE_DETAIL, "Not a Print Partner kit file", "Unsupported kit version",
      "Missing kit.json in kit archive", "Cannot read properties of null (reading 'format')"];
    const kind = error instanceof exportKit.KitJsonTooLargeError ? "too_large"
      : error.name === "InvalidCommunityManifestSlugError" ? "invalid_slug"
        : exact.includes(error.message) ? "message"
          : error instanceof SyntaxError ? "syntax"
            : error instanceof TypeError ? "type"
              : "archive";
    return { kind, name: error.name, message: error.message };
  };
  const run = (fn) => { try { return { ok: fn() }; } catch (error) { return { error: refused(error) }; } };

  const parse = (await parseInputs(fflate)).map(({ id, filename, buf, recipe }) => {
    const result = run(() => exportKit.parseKitBundleBuffer(buf, filename ?? undefined));
    const text = result.ok && JSON.stringify(result.ok);
    if (text && text.length > 1024 * 1024) result.ok = { sha256: createHash("sha256").update(text).digest("hex") };
    return { id, filename, ...(recipe ? { recipe } : { hex: buf.toString("hex") }), result };
  });

  const sourceRefs = sourceRefCases.map(({ id, data }) => ({ id, data, result: run(() => {
    const refs = share.collectKitBundleSourceRefs(data);
    return {
      refs,
      unmatched: refs.map(share.kitUnmatchedSourceFromRef),
      patches: refs.map(share.kitMatchedSourcePatch),
      exports: refs.map(share.kitSourceRefToExportRecord),
    };
  }) }));

  const projects = projectRows.map(({ id, row }) => {
    const ref = share.kitSourceRefFromProject(row);
    return { id, row, export: share.kitLayerProjectExportRecord(ref), patch: share.kitMatchedSourcePatch(ref) };
  });

  const exportedAt = "2026-10-30T01:02:03.004Z";
  const buildBundle = (mode) => {
    const data = exportKit.buildKitBundleData({ mode, exportedAt });
    const kitJson = JSON.stringify(data, null, 2);
    const zipped = Buffer.from(fflate.zipSync({ "kit.json": [new TextEncoder().encode(kitJson), { mtime: MTIME }] }));
    return { kit_json: kitJson, zip_hex: zipped.toString("hex") };
  };
  const editable = buildBundle({ kind: "editable", recipe });
  const roundTrip = exportKit.parseKitBundleBuffer(Buffer.from(editable.zip_hex, "hex"), "x.print-partner-kit.zip");
  const build = { recipe, exported_at: exportedAt, ...editable,
    round_trip_equal: JSON.stringify(roundTrip, null, 2) === editable.kit_json };

  const acceptedBuilds = acceptedCases.map(({ id, parts }) => {
    const accepted = parts === null ? null : { parts };
    return { id, parts, ...buildBundle({ kind: "accepted_progress", recipe, accepted }) };
  });

  process.stdout.write(JSON.stringify({ max_kit_json_bytes: exportKit.MAX_KIT_JSON_BYTES, parse, source_refs: sourceRefs, projects, build, accepted_builds: acceptedBuilds }));
} else {
  const probe = execFileSync(process.execPath, [
    "--conditions=development", "--import", pathToFileURL(join(root, "web/node_modules/tsx/dist/esm/index.mjs")).href,
    fileURLToPath(import.meta.url), "--probe",
  ], { cwd: join(root, "web/apps/server"), encoding: "utf8", env: { ...process.env, TZ: "UTC" }, maxBuffer: 256 * 1024 * 1024 });
  const golden = JSON.parse(probe);
  const header = JSON.stringify({ generated: "@generated by fixtures/parity/kit-bundle/generate.mjs. Do not edit.", sources });
  const list = (items) => `[\n${items.map((item) => JSON.stringify(item)).join(",\n")}\n]`;
  const text = `${header.slice(0, -1)},\n"max_kit_json_bytes":${golden.max_kit_json_bytes},\n"parse":${list(golden.parse)},\n"source_refs":${list(golden.source_refs)},\n"projects":${list(golden.projects)},\n"build":${JSON.stringify(golden.build)},\n"accepted_builds":${list(golden.accepted_builds)}}\n`;
  if (process.argv.includes("--check")) {
    if (readFileSync(output, "utf8") !== text) throw new Error("kit-bundle.json is out of date");
  } else {
    writeFileSync(output, text);
  }
  console.log(JSON.stringify({ parse: golden.parse.length, source_refs: golden.source_refs.length, projects: golden.projects.length, check: process.argv.includes("--check") }));
}
