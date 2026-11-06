// A fake Rust core for the browser checks: GET /health with every Module listed, the
// Builds, task feed and workflow routes over `count` generated Builds, the farm read
// over a fixed fleet, and two printer Checkoff results waiting on Today. Bodies follow
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
    const match = /^\/api\/v2\/plans\/(\d+)(\/layers|\/workflow|\/checkoff\/units)?$/.exec(pathname);
    const found = match && builds[Number(match[1]) - 1];
    if (!found) return undefined;
    if (!match[2]) return { status: 200, body: found };
    if (match[2] === "/workflow") return { status: 200, body: workflow(found) };
    if (match[2] === "/checkoff/units") return { status: 200, body: checkoffUnits(found.id) };
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
