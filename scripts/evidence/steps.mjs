// Named setup steps for capture.mjs. Each takes `api` (safety.mjs makeApi), a Build ID
// and `{ root, coreOrigin }`, changes the disposable data through the Rust API, and
// returns a log line.
import { randomUUID } from "node:crypto";
import { createRequire } from "node:module";
import { resolve } from "node:path";
import { CaptureError, guarded } from "./safety.mjs";

const plan = (id) => `/api/v2/plans/${id}`;
const idempotent = () => ({ "Idempotency-Key": randomUUID() });

/** The Build's open draft, or null. */
async function openDraft(api, id) {
  const { drafts } = await api("GET", `${plan(id)}/drafts`);
  return drafts.find((draft) => draft.state === "open") ?? null;
}

/** Resolves every reconciliation conflict on the open draft by replacing the part. */
async function settleReconciliation(api, id) {
  const draft = await openDraft(api, id);
  if (!draft) return "no open draft";
  const workspace = await api("GET", `${plan(id)}/drafts/${draft.draft_id}`);
  if (workspace.reconciliation.kind === "ready") return "already settled";
  const decisions = workspace.reconciliation.conflicts.map((conflict) => ({
    kind: "replace",
    target_draft_part_id: conflict.target_draft_part_id,
  }));
  const settled = await api(
    "PUT",
    `${plan(id)}/drafts/${draft.draft_id}/reconciliation`,
    { decisions, expected_snapshot_digest: workspace.draft.snapshot_digest },
    idempotent(),
  );
  if (settled.reconciliation.kind !== "ready") throw new CaptureError(`Build ${id} still has conflicts`);
  return `settled ${decisions.length} conflict(s)`;
}

/**
 * Accepts the Build's open draft, which stores a pre-apply snapshot first. Without an
 * open draft it opens one by saving the current header unchanged. Accept stores a
 * snapshot only when the Build has an Accepted revision whose Source rows the core can
 * still read (no `legacy:` rows); the accept preview says so in `snapshot`, and the step
 * refuses before Accept when it would store none.
 */
async function snapshot(api, id) {
  let draft = await openDraft(api, id);
  if (!draft) {
    const { build, base } = await api("GET", `${plan(id)}/drafts/header`);
    await api(
      "PUT",
      `${plan(id)}/drafts/header`,
      { expected_base: base, expected_draft: null, expected_header_version: null, header: build },
      idempotent(),
    );
    draft = await openDraft(api, id);
    if (!draft) throw new CaptureError(`Build ${id}: saving the header opened no draft`);
  }
  await settleReconciliation(api, id);
  const preview = await api("GET", `${plan(id)}/drafts/${draft.draft_id}/accept-preview`);
  if (preview.snapshot?.saved_before_accept !== true) {
    // detail is one of two fixed pp-storage sentences; cap it to one short line anyway.
    const detail = String(preview.snapshot?.detail ?? "no snapshot note").replace(/\s+/g, " ").slice(0, 160);
    throw new CaptureError(`Build ${id}: Accept saves no snapshot (${detail})`);
  }
  const revision = preview.revision;
  await api(
    "POST",
    `${plan(id)}/drafts/${draft.draft_id}/apply`,
    {
      expected_snapshot_digest: revision.expected_snapshot_digest,
      expected_lifecycle_version: revision.expected_lifecycle_version,
      expected_base: revision.expected_base,
      expected_header_version: revision.expected_header_version,
      remap_checkoff_links: false,
    },
    idempotent(),
  );
  return `accepted draft ${draft.draft_id} (${preview.kind}); snapshot stored`;
}

/** Proposes an order already held by another synthetic Build; never accepts it. */
async function duplicateOrder(api, id) {
  const { profiles: builds } = await api("GET", "/api/v2/plans");
  const { build, base } = await api("GET", `${plan(id)}/drafts/header`);
  const orderKey = (order) => (order ?? "").trim().replace(/\s+/g, " ").toLowerCase();
  const holder = builds.find((other) => other.id !== id && !other.archived_at
    && orderKey(other.order_number) && orderKey(other.order_number) !== orderKey(build.order_number));
  if (!holder) throw new CaptureError("no synthetic Build holds a different order");
  const draft = await openDraft(api, id);
  const current = draft ? await api("GET", `${plan(id)}/drafts/${draft.draft_id}/header`) : null;
  await api("PUT", `${plan(id)}/drafts/header`, {
    expected_base: draft?.base ?? base,
    expected_draft: draft ? {
      draft_id: draft.draft_id, state: draft.state,
      lifecycle_version: draft.lifecycle_version, snapshot_digest: draft.snapshot_digest,
      base: draft.base,
    } : null,
    expected_header_version: current?.draft?.header_version ?? null,
    header: { ...build, order_number: holder.order_number },
  }, idempotent());
  await settleReconciliation(api, id);
  return "saved conflicting order to draft without accepting";
}

/** The official MCP TypeScript SDK, as the Node server installs it. */
function mcpSdk(root) {
  const load = createRequire(resolve(root, "web/apps/server/package.json"));
  return {
    Client: load("@modelcontextprotocol/sdk/client/index.js").Client,
    Transport: load("@modelcontextprotocol/sdk/client/streamableHttp.js").StreamableHTTPClientTransport,
  };
}

/** One tool call: its first text block, parsed, and whether the tool reported an error. */
async function callTool(client, name, args) {
  const result = await guarded(client.callTool({ name, arguments: args }), `MCP ${name}: call failed`);
  try {
    return { value: JSON.parse(result.content?.[0]?.text ?? ""), isError: result.isError === true };
  } catch {
    throw new CaptureError(`MCP ${name}: the result is not JSON`);
  }
}

/** The Build's layers as `type:Source` strings, in order. */
async function layersOf(api, id) {
  const body = await api("GET", `/api/v2/plans/${id}/layers`);
  const layers = Array.isArray(body) ? body : body?.layers ?? [];
  return layers.map((layer) => `${layer.layer_type}:${layer.project_name}`);
}

/** The planning brief in a build-planning read, or null. */
function briefOf(planning) {
  return planning?.planning?.brief ?? planning?.brief ?? null;
}

/** Stored state the receipt watches: layers, the planning brief and the Accepted Plan. */
async function stateOf(api, id) {
  const [layers, planning, header] = await Promise.all([
    layersOf(api, id),
    api("GET", `/api/v2/plans/${id}/build-planning`),
    api("GET", `/api/v2/plans/${id}/drafts/header`),
  ]);
  return {
    layers,
    requirements: JSON.stringify(briefOf(planning)?.requirements ?? null),
    // The Accepted Plan the drafts header is based on: its version and revision.
    accepted: JSON.stringify(header?.base ?? null),
  };
}

/** Fails unless `actual` equals `expected` field for field. */
function expectSame(label, actual, expected) {
  if (JSON.stringify(actual) !== JSON.stringify(expected)) throw new CaptureError(`${label}: stored state changed`);
}

/** The HTTP status that refused `promise`, or 0 if it succeeded. Never reads the error text. */
async function refusal(promise) {
  try {
    await promise;
    return 0;
  } catch (error) {
    return Number.isInteger(error?.code) ? error.code : -1;
  }
}

/**
 * Runtime receipt for MCP through the gateway (GRE-1333, with GRE-1386's calls). Uses the
 * official MCP SDK on `/mcp` and `/api/v1/mcp` with only an API key: no cookie and no
 * Origin, as an assistant connects. Checks status codes, result fields and stored state
 * through the Rust API, never message text. run.py seeds Build `id` with a planning brief
 * that has one open requirement. The key never reaches the output.
 */
async function mcpReceipt(api, id, { root, coreOrigin }) {
  const { Client, Transport } = mcpSdk(root);
  const lines = [];
  const connect = async (path, headers) => {
    const client = new Client({ name: "pp-evidence-mcp-receipt", version: "1.0.0" });
    const transport = new Transport(new URL(path, coreOrigin), { requestInit: { headers } });
    await client.connect(transport);
    return client;
  };
  const opened = [];
  const session = async (path, key) => {
    const client = await guarded(connect(path, { Authorization: `Bearer ${key}` }), `MCP initialize on ${path} failed`);
    opened.push(client);
    return client;
  };
  try {
    // Keyless and bad keys: refused before any tool runs.
    const keyless = await guarded(fetch(new URL("/mcp", coreOrigin), {
      method: "POST",
      headers: { "Content-Type": "application/json", Accept: "application/json, text/event-stream" },
      body: "{}",
    }), "keyless POST /mcp: transport error");
    lines.push(`keyless POST /mcp, no Origin -> ${keyless.status}`);
    if (keyless.status !== 403) throw new CaptureError("keyless POST /mcp was not refused with 403");
    for (const [label, headers] of [
      ["no key, exact Origin", { Origin: coreOrigin }],
      ["wrong key", { Authorization: `Bearer pp_${randomUUID().replaceAll("-", "")}` }],
    ]) {
      for (const path of ["/mcp", "/api/v1/mcp"]) {
        const status = await refusal(connect(path, headers));
        lines.push(`initialize ${path}, ${label} -> ${status}`);
        if (status !== 401) throw new CaptureError(`${label} on ${path} was not refused with 401`);
      }
    }

    const { key } = await api("POST", "/settings/api-keys", {});
    const viaMcp = await session("/mcp", key);
    const viaApi = await session("/api/v1/mcp", key);
    const server = viaMcp.getServerVersion();
    const { tools } = await guarded(viaMcp.listTools(), "MCP tools/list failed");
    const { tools: apiTools } = await guarded(viaApi.listTools(), "MCP tools/list on /api/v1/mcp failed");
    lines.push(`initialize /mcp and /api/v1/mcp with a key -> ${server?.name} ${server?.version}`);
    lines.push(`tools/list -> ${tools.length} tools on /mcp, ${apiTools.length} on /api/v1/mcp: ${tools.map((tool) => tool.name).join(", ")}`);
    const names = tools.map((tool) => tool.name);
    if (!names.length || JSON.stringify(names) !== JSON.stringify(apiTools.map((tool) => tool.name))) {
      throw new CaptureError("the two MCP paths list different tools");
    }
    for (const name of ["add_addon", "propose_update_build_brief", "confirm_apply", "get_plan_decisions",
      "list_pending_actions", "dismiss_proposed_action", "propose_create_build"]) {
      lines.push(`  ${name}: ${names.includes(name) ? "listed" : "not listed"}`);
      if (!names.includes(name)) throw new CaptureError(`${name} is not listed`);
    }

    // add_addon on /mcp and on /api/v1/mcp for the same unattached Source, then confirm both.
    const before = await stateOf(api, id);
    lines.push(`Build ${id} before: layers [${before.layers.join(", ")}]; accepted ${before.accepted}`);
    if (JSON.parse(before.accepted)?.revision_id == null) throw new CaptureError(`Build ${id} has no Accepted Plan revision`);
    const listed = await api("GET", "/api/v1/sources");
    const attached = new Set(before.layers.map((layer) => layer.split(":").slice(1).join(":")));
    const source = (Array.isArray(listed) ? listed : listed?.sources ?? [])
      .map((item) => item.name).sort().find((name) => !attached.has(name));
    if (!source) throw new CaptureError(`Build ${id}: every Source is already attached`);
    const proposals = [];
    for (const [path, client] of [["/mcp", viaMcp], ["/api/v1/mcp", viaApi]]) {
      const { value, isError } = await callTool(client, "add_addon", { plan_id: id, source_name: source });
      const action = value.action;
      const ok = !isError && value.status === "proposed" && action?.params?.source_name === source
        && Array.isArray(value.warnings) && Array.isArray(value.suggested_excludes);
      lines.push(`add_addon on ${path} {plan_id:${id}, source_name:"${source}"} -> status ${value.status}, action ${action?.id} for "${action?.params?.source_name}", warnings ${value.warnings?.length}, suggested_excludes ${value.suggested_excludes?.length}`);
      if (!ok) throw new CaptureError(`add_addon on ${path} did not propose ${source}`);
      expectSame(`add_addon on ${path}`, await stateOf(api, id), before);
      proposals.push([path, client, action.id]);
    }
    lines.push(`layers after both proposals: unchanged`);

    const [[, first, firstId], [secondPath, second, secondId]] = proposals;
    const confirmed = await callTool(first, "confirm_apply", { action_id: firstId });
    const resultLayers = (confirmed.value.result?.layers ?? []).map((layer) => `${layer.layer_type}:${layer.project_name}`);
    lines.push(`confirm_apply ${firstId} on /mcp -> ok ${confirmed.value.ok}; result layers [${resultLayers.join(", ")}]`);
    if (confirmed.isError || confirmed.value.ok !== true) throw new CaptureError(`confirm_apply ${firstId} was refused`);
    const added = await stateOf(api, id);
    lines.push(`GET /api/v2/plans/${id}/layers -> [${added.layers.join(", ")}]`);
    if (!added.layers.includes(`addon:${source}`)) throw new CaptureError(`Build ${id} layers do not show ${source}`);
    expectSame("accepted Plan after add_addon", added.accepted, before.accepted);
    const decisions = await callTool(first, "get_plan_decisions", { plan_id: id });
    const decisionList = Array.isArray(decisions.value) ? decisions.value : decisions.value.decisions ?? [];
    const applied = decisionList.filter((decision) => decision.kind === "applied_action"
      && decision.action_type === "add_addon" && decision.params?.source_name === source);
    lines.push(`get_plan_decisions -> ${decisionList.length} decisions, ${applied.length} applied add_addon for "${source}"`);
    if (applied.length !== 1) throw new CaptureError("get_plan_decisions does not list the applied add_addon");

    const again = await callTool(second, "confirm_apply", { action_id: secondId });
    // A refused apply puts the action back in its session's pending list; an unknown id would not.
    const stillPending = ((await callTool(second, "list_pending_actions", {})).value.actions ?? []).some((action) => action.id === secondId);
    lines.push(`confirm_apply ${secondId} on ${secondPath}, Source already attached -> ok ${again.value.ok}, isError ${again.isError}, still pending ${stillPending}`);
    if (again.value.ok !== false || again.isError !== true || !stillPending) {
      throw new CaptureError("confirming an already attached Source was not refused");
    }
    await callTool(second, "dismiss_proposed_action", { action_id: secondId });
    expectSame("second confirm", await stateOf(api, id), added);
    lines.push("layers after the refused confirm: unchanged");

    // propose_update_build_brief on /api/v1/mcp corrects the seeded open requirement.
    const planning = await api("GET", `/api/v2/plans/${id}/build-planning`);
    const requirements = briefOf(planning)?.requirements ?? [];
    const open = requirements.find((requirement) => requirement.status === "unverified");
    if (!open) throw new CaptureError(`Build ${id} has no open requirement in its planning brief`);
    const corrected = requirements.map((requirement) =>
      requirement === open ? { key: open.key, value: open.value, status: "satisfied" } : requirement);
    const brief = await callTool(viaApi, "propose_update_build_brief", { plan_id: id, requirements: corrected });
    lines.push(`propose_update_build_brief on /api/v1/mcp {requirement "${open.key}": unverified -> satisfied} -> status ${brief.value.status}, action ${brief.value.action?.id}`);
    if (brief.isError || brief.value.status !== "proposed" || !brief.value.action?.id) throw new CaptureError("propose_update_build_brief did not propose");
    expectSame("brief proposal", await stateOf(api, id), added);
    lines.push("build-planning after the proposal: unchanged");
    const briefConfirmed = await callTool(viaApi, "confirm_apply", { action_id: brief.value.action.id });
    const updated = await stateOf(api, id);
    const stored = JSON.parse(updated.requirements).find((requirement) => requirement.key === open.key);
    lines.push(`confirm_apply ${brief.value.action.id} on /api/v1/mcp -> ok ${briefConfirmed.value.ok}; GET /api/v2/plans/${id}/build-planning requirement "${open.key}" -> ${stored?.status}`);
    if (briefConfirmed.value.ok !== true || stored?.status !== "satisfied") throw new CaptureError("the brief correction was not stored");
    expectSame("accepted Plan after the brief", updated.accepted, before.accepted);

    const { profiles } = await api("GET", "/api/v2/plans");
    const other = profiles.map((profile) => profile.id).find((plan) => plan !== id);
    const otherBefore = await stateOf(api, other);
    if (otherBefore.requirements !== "null") throw new CaptureError(`Build ${other} already has a planning brief`);
    const refused = await callTool(viaMcp, "propose_update_build_brief", { plan_id: other, requirements: corrected });
    lines.push(`propose_update_build_brief for Build ${other}, which has no brief -> isError ${refused.isError}, action ${refused.value.action?.id ?? "none"}`);
    if (refused.isError !== true || refused.value.action?.id || refused.value.status === "proposed") {
      throw new CaptureError("a Build with no brief was not refused at propose");
    }
    expectSame(`Build ${other}`, await stateOf(api, other), otherBefore);

    // A revoked key: an open session and a new one are both refused, and nothing changes.
    const revocable = await api("POST", "/settings/api-keys", {});
    const doomed = await session("/mcp", revocable.key);
    await api("DELETE", `/settings/api-keys/${revocable.id}`);
    const settled = await stateOf(api, id);
    const revokedCall = await refusal(doomed.callTool({ name: "add_addon", arguments: { plan_id: id, source_name: source } }));
    // The MCP guard answers 401 before any tool runs, so a refused call cannot propose anything.
    lines.push(`revoked key (DELETE /settings/api-keys/{id}), open session: add_addon -> ${revokedCall}`);
    for (const path of ["/mcp", "/api/v1/mcp"]) {
      const status = await refusal(connect(path, { Authorization: `Bearer ${revocable.key}` }));
      lines.push(`revoked key, initialize ${path} -> ${status}`);
      if (status !== 401) throw new CaptureError(`a revoked key on ${path} was not refused with 401`);
    }
    if (revokedCall !== 401) throw new CaptureError("a revoked key's open session was not refused with 401");
    expectSame("after revoked calls", await stateOf(api, id), settled);
    // propose_create_build (GRE-1443, #1726): listed in the catalog; record what main serves.
    const plansBefore = (await api("GET", "/api/v2/plans")).profiles.length;
    const create = await callTool(viaMcp, "propose_create_build", { name: "Synthetic MCP receipt Build", request: "A synthetic Build for the MCP receipt." });
    const createActions = (await callTool(viaMcp, "list_pending_actions", {})).value.actions ?? [];
    lines.push(`propose_create_build -> isError ${create.isError}, status ${create.value.status ?? "none"}, action ${create.value.action?.id ?? "none"}`);
    if (create.value.action?.id) {
      await callTool(viaMcp, "dismiss_proposed_action", { action_id: create.value.action.id });
    }
    expectSame("Builds after propose_create_build", (await api("GET", "/api/v2/plans")).profiles.length, plansBefore);
    lines.push(`  pending after it: ${createActions.length}; Builds: ${plansBefore} (unchanged)`);
    for (const [path, client] of [["/mcp", viaMcp], ["/api/v1/mcp", viaApi]]) {
      const pending = (await callTool(client, "list_pending_actions", {})).value.actions ?? [];
      lines.push(`list_pending_actions on ${path} -> [${pending.map((action) => action.id).join(", ")}]`);
      if (pending.length) throw new CaptureError(`a pending action is left on ${path}`);
    }
    const final = await stateOf(api, id);
    expectSame("accepted Plan at the end", final.accepted, before.accepted);
    lines.push(`Build ${id} at the end: layers [${final.layers.join(", ")}]; accepted ${final.accepted} (unchanged)`);
    return lines.join("\n  ");
  } catch (error) {
    // The lines hold only statuses, ids and synthetic names, so a failed run still shows them.
    console.log(`capture: mcp-receipt before the failure:\n  ${lines.join("\n  ")}`);
    throw error;
  } finally {
    for (const client of opened) await client.close().catch(() => {});
  }
}

// Named setup steps. Each takes a Build ID; run.py checks names and arguments.
export const STEPS = {
  "settle-reconciliation": settleReconciliation,
  snapshot,
  "duplicate-order": duplicateOrder,
  "mcp-receipt": mcpReceipt,
};
