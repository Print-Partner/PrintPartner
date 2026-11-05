// Generates the planning-http parity golden: Node's real app answering
// GET /plans/:id/build-planning and GET /plans/:id/decisions on the flat,
// /api/v1 and /api/v2 mounts, over seeded briefs, Sources, drafts and
// decisions. The seed (SQL rows and Source files, with `{data}` standing for
// the data directory) is written into node.json so Rust replays the same
// data. Run from the repo root after `npm --prefix web ci`:
//   node fixtures/parity/planning-http/generate.mjs          # write
//   node fixtures/parity/planning-http/generate.mjs --check  # verify, no writes
// Every stored value is literal, so the capture needs no labels.
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "../../..");
const output = join(here, "node.json");
const sources = [
  "web/apps/server/src/routes/plans.ts",
  "web/apps/server/src/services/build-planning.ts",
  "web/apps/server/src/db/repository.ts",
];
const AT = "2026-11-01T09:00:00.000Z";
const PLANS = ["Synced kept", "Synced changed", "Applied", "Abandoned", "Missing draft", "Claims", "No brief", "Invalid brief", "Foreign brief", "Corrupt draft"];

// Source trees. `link` entries are symlinks, which Node neither follows nor lists.
const FILES = [
  ["repos/1/frame/left.stl", "left v1"], ["repos/1/frame/right.stl", "right"],
  ["repos/1/electronics/board.stl", "board"], ["repos/1/README.md", "readme"],
  ["repos/1/moved/part.stl", "part"], ["repos/1/same.stl", "same"],
  ["repos/1/.git/HEAD", "ref: main"], ["repos/1/.printpartner-source-snapshot.json", "{}"],
  ["repos/2/frame/left.stl", "left v2"], ["repos/2/frame/right.stl", "right"],
  ["repos/2/README.md", "readme"], ["repos/2/renamed/part.stl", "part"],
  ["repos/2/toolhead/new.stl", "new"], ["repos/2/same.stl", "same"], ["repos/2/.git/config", "[core]"],
  ["repos/2/link.stl", { link: "same.stl" }],
  ["repos/3/x.stl", "x"],
];

const evidence = (id, source, extra = {}) => ({
  id, url: `printpartner:source:${source}`, normalized_url: `printpartner:source:${source}`,
  kind: "model_source", input_kind: "upload", source_id: source, ...extra,
});
const brief = (buildId, extra = {}) => ({
  version: 1, build_id: buildId, special_request: "Build a 350mm printer with a probe",
  requirements: [{ key: "size", value: "350", status: "satisfied" }],
  evidence: [], contributions: [], compatibility_findings: [], differences: [], resolutions: {},
  role_filaments: [{ role: "primary", inventory_kind: "catalog", color_hex: "#112233", substitution_confirmed: false }],
  checklist_items: [], draft_review_blockers: [], created_at: AT, updated_at: AT, ...extra,
});
const synced = [
  evidence("base", 1, { kind: "canonical_design", source_role: "structural_base", title: "Base" }),
  evidence("overlay", 2, { kind: "vendor_overlay", source_role: "overlay", title: "Overlay" }),
];
const longClaim = "x".repeat(150) + " 🧵 longer than the cut";

/** The briefs by plan id; plan 1's differences come from Node's own hydration. */
function briefs(keptDifferences) {
  const groups = [...new Set(keptDifferences.map((difference) => difference.group_id))];
  return {
    1: brief(1, {
      evidence: synced, differences: keptDifferences,
      resolutions: Object.fromEntries(groups.map((group) => [group, { resolution: "include_both", rationale: "both", resolved_at: AT }])),
      draft_id: 1, draft_source_revisions: { 1: "aaa111", 2: "bbb222" },
      checklist_items: [{ id: "c1", title: "Test fit", category: "test_fit", required: true, completed: true }],
    }),
    2: brief(2, {
      evidence: synced,
      differences: [
        { id: "stale", group_id: "base:overlay:frame", family: "frame", kind: "changed", source_a: "Base", source_b: "Overlay", path_a: "frame/left.stl", path_b: "frame/left.stl", detail: "old" },
        { id: "claim", group_id: "claims:old", family: "documentation_claims", kind: "contradictory", source_a: "a", source_b: "b", detail: "old claim" },
      ],
      resolutions: {
        "base:overlay:frame": { resolution: "choose_source_b", rationale: "newer", resolved_at: AT },
        gone: { resolution: "custom", rationale: "r", custom_resolution: "c", resolved_at: AT },
      },
      draft_id: 2, draft_source_revisions: { 1: "old" },
    }),
    3: brief(3, { requirements: [{ key: "size", value: "350", status: "user_waived", detail: "ok" }], draft_id: 3 }),
    4: brief(4, { draft_id: 4, draft_source_revisions: {} }),
    5: brief(5, {
      draft_id: 999,
      role_filaments: [{ role: "accent", requested_brand: "Brand", inventory_kind: "substitute", inventory_id: "9", color_hex: "#ABCDEF", substitution_confirmed: false }],
      checklist_items: [{ id: "c2", title: "Wire the probe", detail: "CAN", category: "wiring", required: true, completed: false }],
      draft_review_blockers: ["Review plates"],
    }),
    6: brief(6, {
      requirements: [
        { key: "project", value: "printer", status: "unverified" },
        { key: "transport", value: "can", status: "incompatible" },
        { key: "size", value: "350", status: "satisfied" },
      ],
      evidence: [
        evidence("pending", 3, { source_role: "addon", title: " Probe ", extract: " Uses CAN " }),
        evidence("rootless", 4, { source_role: "addon", title: "probe", extract: longClaim }),
        evidence("unknown", 77, { title: "PROBE", extract: "Uses CAN", sync_status: "failed" }),
        evidence("roleless", 3),
        { id: "page", url: "https://printables.com/model/1", normalized_url: "https://printables.com/model/1", kind: "model_source", input_kind: "model_page", upload_required: true },
        { id: "page2", url: "https://printables.com/model/2", normalized_url: "https://printables.com/model/2", kind: "model_source", input_kind: "model_page", upload_required: true },
        evidence("upload2", 3, { source_role: "addon", derived_from_evidence_id: "page2" }),
        { id: "docs", url: "https://example.com/docs", normalized_url: "https://example.com/docs", kind: "informational_evidence", sync_status: "synced", title: "Docs", extract: "  " },
      ],
      contributions: [
        { id: "k1", evidence_id: "pending", slot: "probe", responsibility: "printable_parts", path_scopes: ["probe/"], confidence: "medium", evidence_text: "t", status: "proposed" },
        { id: "k2", evidence_id: "pending", slot: "toolhead", responsibility: "hardware_constraint", path_scopes: [], confidence: "low", evidence_text: "t", status: "confirmed" },
      ],
      compatibility_findings: [
        { id: "f1", subject: "probe", status: "unverified", detail: "unknown mount", evidence_ids: ["pending"] },
        { id: "f2", subject: "board", status: "incompatible", detail: "too wide", evidence_ids: [] },
        { id: "f3", subject: "frame", status: "satisfied", detail: "fits", evidence_ids: [] },
      ],
    }),
    9: brief(1),
    10: brief(10, { draft_id: 5 }),
  };
}

function seedSql(keptDifferences, digest) {
  const project = "INSERT INTO projects (id, tenant_id, name, url, local_path, last_synced_at, last_commit_sha) VALUES (?, 'default', ?, ?, ?, ?, ?)";
  const sql = [
    [project, [1, "Base", "https://github.com/example/base", "{data}/repos/1", AT, "aaa111"]],
    [project, [2, "Overlay", "https://github.com/example/overlay", "{data}/repos/2", AT, "bbb222"]],
    [project, [3, "Pending", "https://github.com/example/pending", "{data}/repos/3", AT, null]],
    [project, [4, "Rootless", "https://github.com/example/rootless", null, AT, "ddd444"]],
    ["INSERT INTO plan_revisions (id, tenant_id, profile_id, revision_number, provenance_kind, digest_format, snapshot_digest, created_by, accepted_by, created_at, accepted_at) VALUES (1, 'default', 3, 1, 'legacy', 'plan-snapshot-v1', ?, 'user', 'user', ?, ?)", ["a".repeat(64), AT, AT]],
  ];
  const draft = (id, plan, state, revision) => [
    "INSERT INTO plan_drafts (id, tenant_id, profile_id, base_plan_version, state, lifecycle_version, digest_format, snapshot_digest, created_by, idempotency_key, created_at, consumed_revision_id, consumed_at) VALUES (?, 'default', ?, 0, ?, ?, 'plan-draft-v1', ?, 'user', ?, ?, ?, ?)",
    [id, plan, state, state === "open" ? 0 : 1, digest, `draft-${id}`, AT, revision, revision == null ? null : AT],
  ];
  sql.push(draft(1, 1, "open", null), draft(2, 2, "open", null), draft(3, 3, "consumed", 1), draft(4, 4, "abandoned", null));
  // Draft 5's digest does not match its rows, so Node's getPlanDraft refuses it.
  sql.push(draft(5, 10, "open", null).map((item, index) => index === 1 ? item.with(4, "c".repeat(64)) : item));
  for (const [plan, value] of Object.entries(briefs(keptDifferences))) {
    sql.push(["INSERT INTO app_settings (tenant_id, key, value) VALUES ('default', ?, ?)", [`build_planning.v1.${plan}`, JSON.stringify(value)]]);
  }
  sql.push(["INSERT INTO app_settings (tenant_id, key, value) VALUES ('default', 'build_planning.v1.8', '{not json')", []]);
  // Plan 1: 205 decisions, two per timestamp, so the latest 200 cut a tie.
  const params = ['{"part_id":4,"nested":{"list":[1,2.5,null,true,"🧵"]}}', "[1,2]", "not json", "{}"];
  const results = [null, "", '{"ok":true}', "[]", "5"];
  for (let index = 0; index < 205; index += 1) {
    const millis = String(Math.floor(index / 2)).padStart(3, "0");
    sql.push([
      "INSERT INTO plan_decisions (tenant_id, profile_id, created_at, actor, kind, action_type, params_json, label, summary, rationale, result_json) VALUES ('default', 1, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
      [`2026-10-01T09:00:00.${millis}Z`, index % 3 ? "assistant" : "user", index % 2 ? "applied" : "dismissed", index % 4 ? null : "set_quantity", params[index % 4], `Decision ${index}`, index % 5 ? "" : `Summary ${index}`, index % 6 ? null : "because", results[index % 5]],
    ]);
  }
  sql.push(
    ["INSERT INTO plan_decisions (tenant_id, profile_id, created_at, actor, kind, label) VALUES ('default', 2, ?, 'user', 'user_note', 'Mine')", [AT]],
    ["INSERT INTO plan_decisions (tenant_id, profile_id, created_at, actor, kind, label) VALUES ('other', 2, ?, 'user', 'user_note', 'Foreign')", [AT]],
  );
  return sql;
}

const REQUESTS = [
  ...PLANS.flatMap((name, index) => [
    [`planning ${name}`, ["", "/api/v1", "/api/v2"][index % 3] + `/plans/${index + 1}/build-planning`],
    [`decisions ${name}`, ["/api/v2", "", "/api/v1"][index % 3] + `/plans/${index + 1}/decisions`],
  ]),
  ["planning invalid draft_id", "/plans/1/build-planning?draft_id=abc"],
  ["planning valid draft_id", "/api/v2/plans/1/build-planning?draft_id=2"],
  ["planning draft_id without brief", "/plans/7/build-planning?draft_id=0"],
  ["planning invalid draft_id before corrupt draft", "/plans/10/build-planning?draft_id=abc"],
  ["planning missing plan", "/api/v2/plans/999999/build-planning"],
  ["decisions missing plan", "/plans/999999/decisions"],
  ["decisions hex id", "/plans/0x2/decisions"],
  ["decisions word id", "/api/v2/plans/abc/decisions"],
];

if (process.argv.includes("--probe")) {
  const load = (source) => import(pathToFileURL(join(root, source)).href);
  const require = createRequire(join(root, "web/apps/server/package.json"));
  const Database = require("better-sqlite3");
  const dataDir = mkdtempSync(join(tmpdir(), "pp-parity-planning-http-"));
  try {
    process.env.PRINT_PARTNER_DATA_DIR = dataDir;
    delete process.env.PRINT_PARTNER_API_KEY;
    const { createSelfHostPorts } = await load("web/apps/server/src/adapters/self-host/index.ts");
    const { buildApp } = await load("web/apps/server/src/app.ts");
    const { loadConfig } = await load("web/apps/server/src/config.ts");
    const { hydrateBuildPlanningBrief } = await load("web/apps/server/src/services/build-planning.ts");
    const { digestPlanDraft } = await load("web/apps/server/src/services/plan-drafts.ts");
    const ports = createSelfHostPorts(dataDir);
    await ports.db.connect();
    const app = await buildApp(loadConfig(), ports);
    await app.ready();

    for (const [index, name] of PLANS.entries()) {
      const created = await app.inject({ method: "POST", url: "/plans", payload: { name } });
      assert.equal(created.json().id, index + 1, name);
    }
    for (const [path, content] of FILES) {
      const absolute = join(dataDir, path);
      mkdirSync(dirname(absolute), { recursive: true });
      if (typeof content === "string") writeFileSync(absolute, content);
      else symlinkSync(content.link, absolute);
    }
    const store = { listSources: () => [1, 2].map((id) => ({ id, name: id === 1 ? "Base" : "Overlay", local_path: join(dataDir, `repos/${id}`), last_synced_at: AT, last_commit_sha: id === 1 ? "aaa111" : "bbb222" })) };
    const kept = hydrateBuildPlanningBrief(store, brief(1, { evidence: synced })).differences;
    assert.equal(kept.length, 4, "changed, renamed, removed and added");
    const sql = seedSql(kept, digestPlanDraft({ baseRevisionId: null, basePlanVersion: 0, inputs: [], parts: [] }));
    const writer = new Database(join(dataDir, "print-partner.db"));
    for (const [statement, values] of sql) writer.prepare(statement).run(...values.map((value) => typeof value === "string" ? value.replace("{data}", dataDir) : value));
    writer.close();

    const reader = new Database(join(dataDir, "print-partner.db"), { readonly: true });
    const tables = reader.prepare("SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name").all().map((row) => row.name);
    const graph = () => new Map(tables.map((table) => [table, JSON.stringify(reader.prepare(`SELECT * FROM "${table}"`).raw().all())]));
    const cases = [];
    for (const [id, path] of REQUESTS) {
      const before = graph();
      const response = await app.inject({ method: "GET", url: path });
      const after = graph();
      cases.push({ id, path, node: { status: response.statusCode, body: response.json() }, changed: tables.filter((table) => before.get(table) !== after.get(table)) });
    }
    reader.close();
    await app.close();
    await ports.db.close?.();
    const byId = Object.fromEntries(cases.map((item) => [item.id, item.node]));
    assert.equal(byId["planning Synced kept"].body.planning.readiness.ready, true);
    assert.equal(byId["planning Synced kept"].body.planning.planning_phase.kind, "draft");
    assert.equal(byId["planning Synced changed"].body.planning.planning_phase.kind, "preparing");
    assert.equal(byId["planning Applied"].body.planning.planning_phase.kind, "applied");
    assert.equal(byId["decisions Synced kept"].body.decisions.length, 200);
    assert(cases.every((item) => item.changed.length === 0), "reads change no table");
    writeFileSync(process.argv.at(-1), JSON.stringify({ files: FILES, sql, cases }));
  } finally {
    rmSync(dataDir, { recursive: true, force: true });
  }
  process.exit(0);
}

const tsx = join(root, "web/node_modules/tsx/dist/esm/index.mjs");
const scratch = mkdtempSync(join(tmpdir(), "pp-parity-planning-http-probe-"));
let probe;
try {
  const result = join(scratch, "probe.json");
  execFileSync(
    process.execPath,
    ["--conditions=development", "--import", pathToFileURL(tsx).href, fileURLToPath(import.meta.url), "--probe", result],
    { cwd: join(root, "web/apps/server"), stdio: ["ignore", "ignore", "inherit"] },
  );
  probe = JSON.parse(readFileSync(result, "utf8"));
} finally {
  rmSync(scratch, { recursive: true, force: true });
}
const lines = (items) => items.map((item) => `    ${JSON.stringify(item)}`).join(",\n");
const text = `{
  "generated": ${JSON.stringify("@generated by fixtures/parity/planning-http/generate.mjs. Do not edit.")},
  "sources": ${JSON.stringify(sources)},
  "files": [
${lines(probe.files)}
  ],
  "sql": [
${lines(probe.sql)}
  ],
  "cases": [
${lines(probe.cases)}
  ]
}
`;
const check = process.argv.includes("--check");
if (check) {
  assert.equal(readFileSync(output, "utf8"), text, "node.json is out of date");
} else {
  writeFileSync(output, text);
}
console.log(JSON.stringify({ cases: probe.cases.length, check }));
