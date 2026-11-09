// A fake Rust core for the browser checks: GET /health with every Module listed, the
// Builds, task feed and workflow routes over `count` generated Builds, the farm read
// over a fixed fleet, two printer Checkoff results waiting on Today, each Build's
// History and the farm log. Bodies follow
// the generated client schemas. There is no /ws/printers, so screens show the stream as
// down.
const MODULES = ["farm", "spoolman", "orders", "bambu", "assembly_tracking"];

const printer = (id, name, kind, state, extra = {}) => ({
  id,
  name,
  model: name,
  integration: kind ? { id: `int-${id}`, kind } : null,
  state,
  reason: null,
  last_heard_at: "2026-11-05T19:26:00Z",
  progress_percent: null,
  loaded_filaments: [{ slot: 1, filament_color_id: null, label: "" }],
  ...extra,
});
const PRINTERS = [
  printer("voron", "Voron 2.4", "moonraker", { execution: "printing", bed_clear: "needs_clearing", display: "printing" }, { progress_percent: 42 }),
  printer("trident", "Trident", "moonraker", { execution: "idle", bed_clear: "needs_clearing", display: "bed_to_clear" }),
  printer("micron", "Micron", "moonraker", { execution: "idle", bed_clear: "clear", display: "ready" }),
  printer("annex", "Annex K3", "moonraker", { execution: "unreachable", bed_clear: "unknown", display: "unreachable" }, { reason: "unreachable" }),
  printer("h2d", "Bambu H2D", "bambu", { execution: "manual", bed_clear: "unknown", display: "manual" }, { reason: "monitoring_not_supported", last_heard_at: null }),
  printer("h2d-spare", "Bambu H2D (spare)", null, { execution: "not_set_up", bed_clear: "unknown", display: "not_set_up" }, { reason: "not_set_up", last_heard_at: null }),
];

const at = (minute) => `2026-11-05T19:${String(minute).padStart(2, "0")}:00.000Z`;
const FARM_UNDO = { undoable: true, reason: null, detail: null };
const farmRefused = (reason, detail = null) => ({ undoable: false, reason, detail });
const farmEntry = (kind, id, minute, summary, extra) => ({
  id: `${kind}:${id}`,
  kind,
  at: at(minute),
  summary,
  build_ids: [1],
  printer_ids: ["voron"],
  units_confirmed: kind === "checkoff_burst" ? 3 : null,
  units_rejected: kind === "checkoff_burst" ? 0 : null,
  undoes: null,
  send_state: kind === "send" ? "done" : null,
  ...extra,
});
const FARM_LOG = [
  farmEntry("checkoff_burst", "4", 25, "Verified 1 bed on Voron 2.4", { undo: FARM_UNDO }),
  farmEntry("send", "q-2", 20, "Sent plate 2 to Voron 2.4", { undo: farmRefused("printer_command") }),
  farmEntry("checkoff_burst_undo", "3", 15, "Undid the burst on Trident", {
    undo: farmRefused("undo_entry"),
    printer_ids: ["trident"],
    undoes: "checkoff_burst:2",
  }),
  farmEntry("checkoff_burst", "2", 10, "Verified 2 beds on Trident", {
    undo: farmRefused("already_undone"),
    printer_ids: ["trident"],
  }),
  farmEntry("checkoff_burst", "1", 5, "Verified 1 bed on Micron", {
    undo: farmRefused("state_changed", "Checkoff changed since this burst: a later change on its Parts depends on it"),
    printer_ids: ["micron"],
  }),
];

function history(b) {
  const entry = (type, id, minute, summary, extra = {}) => ({
    id: `${type}:${id}`,
    type,
    at: at(minute),
    actor: "user",
    summary,
    link: { kind: type, id },
    ...extra,
  });
  return {
    plan_id: b.id,
    entries: [
      entry("decision", "3", 30, "Print the brackets in PETG", { actor: "assistant" }),
      entry("checkoff_burst", "manual-5a1f", 27, "Checked off 2 units, assembled 1", { undo: { undoable: true } }),
      entry("checkoff_burst", "7", 25, "4 units checked off", {
        undo: { undoable: false, reason: "no_undo_token", detail: "No undo is recorded for this Checkoff burst" },
      }),
      entry("send", "q-2", 20, "Sent plate 2 to Voron 2.4", { undo: { undoable: false, reason: "printer_command" } }),
      entry("snapshot", "2", 12, "Before accepting revision 2"),
      entry("revision", "2", 10, "Accepted revision 2"),
      entry("revision", "1", 1, "Accepted revision 1"),
    ],
    next_cursor: null,
  };
}

function build(id) {
  const total = 10 + (id % 90);
  return {
    id,
    name: `Fixture Build ${String(id).padStart(5, "0")}`,
    order_number: id % 3 === 0 ? `SO-${1000 + id}` : null,
    special_request: null,
    part_count: 3 + (id % 12),
    accepted_progress: { kind: "ready", total_units: total, remaining_units: total - (id % total) },
    build_stale: false,
    freshness: { status: "current", accepted_input_set_id: id, accepted_at: "2026-10-02T00:05:00Z" },
    archived_at: null,
    last_used_at: "2026-10-03T12:00:00Z",
    customer: null,
    due_date: null,
  };
}

const checkoffResult = (id, printer, units) => ({
  id,
  profile_id: 1,
  printer_id: printer,
  integration_id: "fixture-moonraker",
  filename: `${id}.gcode`,
  state: "awaiting_verify",
  host_outcome: "success",
  created_at: "2026-10-03T12:00:00Z",
  completed_at: "2026-10-03T13:00:00Z",
  applied_at: null,
  unit_count: units.length,
  units,
  resolved_unit_count: 0,
  resolved_units: [],
});

const CHECKOFF = [
  checkoffResult("fixture-plate-a", "fixture-voron", [
    { part_id: 1, unit_index: 0, part_filename: "fixture_bracket.stl" },
    { part_id: 1, unit_index: 1, part_filename: "fixture_bracket.stl" },
    { part_id: 2, unit_index: 0, part_filename: null },
  ]),
  checkoffResult("fixture-plate-b", "fixture-trident", [{ part_id: 3, unit_index: 4, part_filename: "fixture_gear.stl" }]),
];

// The Checkoff tab's units: two Parts across two Plates, one unit on no Plate.
const fixtureToken = (n) => `ppu_${String(n).padStart(32, "0")}`;
const fixturePlate = (ordinal, printer) => ({ plate_id: `plate_${String(ordinal).padStart(32, "0")}`, ordinal, printer_name: printer });
const checkoffUnit = (n, index, printed, plate) => ({
  token: fixtureToken(n),
  unit_index: index,
  object_name: `fixture unit ${n}`,
  printed,
  assembled: false,
  plate,
});
const checkoffPart = (id, filename, units) => ({
  part_id: id,
  filename,
  relative_path: filename,
  source_layer: "base",
  role: "part",
  quantity: units.length,
  printed_count: units.filter((u) => u.printed).length,
  assembled_count: 0,
  units,
});
const checkoffUnits = (id) => ({
  profile_id: id,
  basis: {
    profile_id: id,
    plan_version: 1,
    plan_revision_id: 1,
    plan_revision_digest: "sha256:fixture",
    required_unit_mapping_digest: "sha256:fixture",
  },
  plate_revision_id: 1,
  parts: [
    checkoffPart(1, "fixture_bracket.stl", [
      checkoffUnit(1, 0, true, fixturePlate(1, "Voron 2.4")),
      checkoffUnit(2, 1, false, fixturePlate(1, "Voron 2.4")),
      checkoffUnit(3, 2, false, fixturePlate(2, "Trident")),
    ]),
    checkoffPart(2, "fixture_gear.stl", [
      checkoffUnit(4, 0, false, fixturePlate(1, "Voron 2.4")),
      checkoffUnit(5, 1, false, null),
    ]),
  ],
});

const nextAction = (id) => ({
  kind: "verify_prints",
  label: "Verify prints",
  reason: `${1 + (id % 4)} prints are waiting for verification`,
  stage_id: "checkoff",
  target: { plan_id: id, tab: "checkoff", draft_id: null },
  draft_id: null,
  issue_count: null,
  item_count: null,
  unit_count: null,
});

function workflow(found) {
  const stage = (id, group, taskCount) => ({
    id,
    group,
    label: id,
    status: { kind: taskCount ? "needs_attention" : "ready", summary: "", task_count: taskCount ?? null, active_count: null },
  });
  const { total_units, remaining_units } = found.accepted_progress;
  return {
    build: { id: found.id, name: found.name },
    sources: { kind: "ready", attached_count: 2 },
    accepted_plan: { kind: "ready", plan_version: 1, revision_id: 1, total_units, remaining_units },
    working_plan: { kind: "none" },
    stock: { kind: "not_ready" },
    stages: [
      stage("sources", "prepare"),
      stage("plan", "prepare"),
      stage("production", "make"),
      stage("checkoff", "make", 1 + (found.id % 4)),
    ],
    next_action: nextAction(found.id),
    active_work: {
      awaiting_verification: 1 + (found.id % 4),
      failed_jobs: 0,
      failed_verifications: 0,
      printing_jobs: 0,
      queued_jobs: 0,
      remaining_units,
      sending_jobs: 0,
      total_units,
    },
  };
}

const DIGEST = "a".repeat(64);
const BASE = { revision_id: 1, plan_version: 1 };
const DRAFT = { draft_id: 9, state: "open", lifecycle_version: 2, snapshot_digest: DIGEST, base: BASE };
const draftPart = (id, filename, included, override) => ({
  draft_part_id: id,
  base_revision_part_id: id,
  part_key: filename,
  filename,
  relative_path: `stl/${filename}`,
  source_layer: "base:Fixture Source 01",
  role: "primary",
  quantity_inferred: 2,
  quantity_override: override,
  quantity_effective: override ?? 2,
  included,
});
const header = (found) => ({ name: found.name, order_number: found.order_number, customer: null, due_date: null });

/** The Plan tab's reads for Build `found`: one open draft that renames it and changes a Part. */
function planRead(found, rest) {
  if (rest === "/drafts") return { profile_id: found.id, drafts: [DRAFT] };
  if (rest === "/drafts/header") return { build: header(found), base: BASE };
  if (rest === "/drafts/9") {
    return {
      profile_id: found.id,
      draft: DRAFT,
      parts: [draftPart(11, "fixture_bracket.stl", true, null), draftPart(12, "fixture_gear.stl", false, 4)],
      diff: { base_is_current: true, added: [], removed: [], changed: [] },
      reconciliation: { kind: "ready", reused_units: 5, new_units: 0, surplus_units: 0 },
    };
  }
  if (rest === "/drafts/9/header") {
    return {
      draft_id: 9,
      build: header(found),
      draft: {
        header: { ...header(found), name: `${found.name} v2`, customer: "Fixture customer", due_date: "2026-12-01" },
        header_version: 1,
        updated_at: "2026-11-07T00:00:00.000Z",
      },
    };
  }
  if (rest !== "/drafts/9/accept-preview") return undefined;
  return {
    kind: "changed",
    revision: { draft_id: 9, expected_snapshot_digest: DIGEST, expected_lifecycle_version: 2, expected_base: BASE, expected_header_version: 1 },
    snapshot: {
      saved_before_accept: true,
      detail: "Print Partner saves a snapshot of the accepted Plan before it accepts this draft.",
    },
    changes: {
      fields: [{ draft_part_id: 12, filename: "fixture_gear.stl", field: "quantity_override", old: null, new: { kind: "integer", value: 4 } }],
      header: [
        { field: "name", old: found.name, new: `${found.name} v2` },
        { field: "customer", old: null, new: "Fixture customer" },
        { field: "due_date", old: null, new: "2026-12-01" },
      ],
      inputs: [],
      parts_added: [],
      parts_removed: [],
      required_units: {
        added: [],
        removed: [],
        printed_affected: [{ token: `ppu_${"0".repeat(31)}1`, filename: "fixture_gear.stl", unit_index: 0, printed: true, assembled: false }],
      },
      plates: { cleared: [{ plate_id: "fixture-plate-a", ordinal: 1, printer_name: "Voron 2.4", unit_count: 3 }] },
      stock: { kind: "off" },
      duplicate_orders: [{ profile_id: 2, name: "Synthetic Build 00002", order_number: "PO-0002" }],
      production: null,
      blocked_by: [],
    },
  };
}

/** A `core` handler for serveDist. Orders is on so the Order column is measured too. */
export function fakeCore(count, { orders = "on" } = {}) {
  const builds = Array.from({ length: count }, (_, i) => build(i + 1));
  // Every other Build has an open task, as the projection's feed would list it.
  const tasks = builds
    .filter((b) => b.id % 2 === 1)
    .map((b) => ({
      plan_id: b.id,
      build_name: b.name,
      due_date: null,
      attention: b.id % 3 === 0,
      rank_reason: "Prints await verification",
      next_action: nextAction(b.id),
    }));
  const health = {
    ok: true,
    authenticated: true,
    authentication_required: true,
    capabilities: [],
    degraded_detail: null,
    discord_oauth_configured: false,
    github_oauth_configured: false,
    multi_user: false,
    owner_mapping_required: false,
    registration_open: false,
    single_user_auth: true,
    single_user_setup_required: false,
    modules: MODULES.map((module) =>
      (module === "orders" && orders === "on") || module === "farm"
        ? { module, state: "on" }
        : { module, state: "off", reason: "default_off" },
    ),
  };
  return (pathname, request) => {
    if (pathname === "/health") {
      return (request.headers.referer ?? "").includes("core=down") ? { status: 503, body: {} } : { status: 200, body: health };
    }
    if (pathname === "/api/v2/plans") return { status: 200, body: { profiles: builds } };
    if (pathname === "/api/v2/tasks") return { status: 200, body: { tasks, unavailable: [] } };
    if (pathname === "/api/v2/printer-checkoff") return { status: 200, body: { results: CHECKOFF } };
    if (pathname === "/api/v2/printers") return { status: 200, body: { printers: PRINTERS } };
    if (pathname === "/api/v2/farm-log") return { status: 200, body: { entries: FARM_LOG, next_cursor: null } };
    const plan = /^\/api\/v2\/plans\/(\d+)(\/drafts.*)$/.exec(pathname);
    const planned = plan && builds[Number(plan[1]) - 1] && planRead(builds[Number(plan[1]) - 1], plan[2]);
    if (planned) return { status: 200, body: planned };
    const match = /^\/api\/v2\/plans\/(\d+)(\/layers|\/workflow|\/checkoff\/units|\/history)?$/.exec(pathname);
    const found = match && builds[Number(match[1]) - 1];
    if (!found) return undefined;
    if (!match[2]) return { status: 200, body: found };
    if (match[2] === "/workflow") return { status: 200, body: workflow(found) };
    if (match[2] === "/checkoff/units") return { status: 200, body: checkoffUnits(found.id) };
    if (match[2] === "/history") return { status: 200, body: history(found) };
    return {
      status: 200,
      body: {
        profile_id: found.id,
        layers: [
          { id: 1, layer_order: 0, layer_type: "base", project_id: 1, project_name: "Fixture Source 01" },
          { id: 2, layer_order: 1, layer_type: "addon", project_id: 2, project_name: "Fixture Source 02" },
        ],
      },
    };
  };
}
