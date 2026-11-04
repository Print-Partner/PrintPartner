// A fake Rust core for the browser checks: GET /health with every Module listed, and the
// Builds, task feed and workflow routes over `count` generated Builds. Bodies follow the generated client schemas.
const MODULES = ["farm", "spoolman", "orders", "bambu", "assembly_tracking"];

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

const nextAction = (id) => ({
  kind: "verify_prints",
  label: "Verify prints",
  reason: `${1 + (id % 4)} prints are waiting for verification`,
  stage_id: "checkoff",
  target: { plan_id: id, tab: "checkoff" },
});

function workflow(found) {
  const stage = (id, group, taskCount) => ({
    id,
    group,
    label: id,
    status: { kind: taskCount ? "needs_attention" : "ready", summary: "", task_count: taskCount },
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
      module === "orders" && orders === "on"
        ? { module, state: "on" }
        : { module, state: "off", reason: "default_off" },
    ),
  };
  return (pathname, request) => {
    if (pathname === "/health") {
      return (request.headers.referer ?? "").includes("core=down") ? { status: 503, body: {} } : { status: 200, body: health };
    }
    if (pathname === "/api/v2/plans") return { status: 200, body: { profiles: builds } };
    if (pathname === "/api/v2/tasks") return { status: 200, body: { tasks } };
    const match = /^\/api\/v2\/plans\/(\d+)(\/layers|\/workflow)?$/.exec(pathname);
    const found = match && builds[Number(match[1]) - 1];
    if (!found) return undefined;
    if (!match[2]) return { status: 200, body: found };
    if (match[2] === "/workflow") return { status: 200, body: workflow(found) };
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
