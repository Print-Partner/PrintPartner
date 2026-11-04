// A fake Rust core for the browser checks: GET /health with every Module listed, and the
// Builds routes over `count` generated Builds. Bodies follow the generated client schemas.
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

/** A `core` handler for serveDist. Orders is on so the Order column is measured too. */
export function fakeCore(count, { orders = "on" } = {}) {
  const builds = Array.from({ length: count }, (_, i) => build(i + 1));
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
    const match = /^\/api\/v2\/plans\/(\d+)(\/layers)?$/.exec(pathname);
    const found = match && builds[Number(match[1]) - 1];
    if (!found) return undefined;
    if (!match[2]) return { status: 200, body: found };
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
