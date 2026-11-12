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

/**
 * Runtime receipt for MCP through the gateway (GRE-1333). Creates an API key, checks that
 * the core refuses a keyless POST /mcp, then connects the official MCP SDK to /mcp with
 * only that key: no cookie and no Origin, as an assistant connects. Lists the tools,
 * proposes `add_addon` with the first Source the Build can take, and confirms it with
 * `confirm_apply`. The key never reaches the output.
 */
async function mcpReceipt(api, id, { root, coreOrigin }) {
  const { Client, Transport } = mcpSdk(root);
  const endpoint = new URL("/mcp", coreOrigin);
  const keyless = await guarded(
    fetch(endpoint, {
      method: "POST",
      headers: { "Content-Type": "application/json", Accept: "application/json, text/event-stream" },
      body: "{}",
    }),
    "keyless POST /mcp: transport error",
  );
  // The MCP guard's own refusal, not the gateway's "Invalid Origin".
  const refusal = await guarded(keyless.text(), "keyless POST /mcp: reading the body failed");
  if (keyless.status !== 403 || !refusal.includes("Invalid request origin")) {
    throw new CaptureError(`keyless POST /mcp answered ${keyless.status}, not the MCP guard's 403`);
  }
  const { key } = await api("POST", "/settings/api-keys", {});
  const client = new Client({ name: "pp-evidence-mcp-receipt", version: "1.0.0" });
  const transport = new Transport(endpoint, { requestInit: { headers: { Authorization: `Bearer ${key}` } } });
  await guarded(client.connect(transport), "MCP initialize failed");
  try {
    const server = client.getServerVersion();
    const { tools } = await guarded(client.listTools(), "MCP tools/list failed");
    const lines = [
      "keyless POST /mcp -> 403 Invalid request origin (MCP guard)",
      `initialize -> ${server?.name} ${server?.version}, session ${transport.sessionId ? "issued" : "none"}`,
      `tools/list -> ${tools.length} tools: ${tools.map((tool) => tool.name).join(", ")}`,
    ];
    const listed = await api("GET", "/api/v1/sources");
    const names = (Array.isArray(listed) ? listed : listed?.sources ?? []).map((source) => source.name).sort();
    let proposal;
    for (const source of names) {
      const { value, isError } = await callTool(client, "add_addon", { plan_id: id, source_name: source });
      lines.push(`add_addon ${JSON.stringify({ plan_id: id, source_name: source })} -> ${
        isError || !value.action?.id ? `refused: ${String(value.error ?? "no action").slice(0, 100)}` : `proposed ${value.action.id}`}`);
      if (!isError && value.action?.id) {
        proposal = value.action;
        break;
      }
    }
    if (!proposal) throw new CaptureError(`Build ${id}: no Source took an add_addon proposal`);
    const pending = await callTool(client, "list_pending_actions", {});
    const listedIds = (pending.value.actions ?? []).map((action) => action.id);
    lines.push(`list_pending_actions -> [${listedIds.join(", ")}]`);
    const confirmed = await callTool(client, "confirm_apply", { action_id: proposal.id });
    if (confirmed.isError || confirmed.value.ok !== true) throw new CaptureError(`confirm_apply ${proposal.id} was refused`);
    const layers = (confirmed.value.result?.layers ?? []).map((layer) => `${layer.layer_type} ${layer.project_name}`);
    lines.push(`confirm_apply ${proposal.id} -> ok; Build ${id} layers: ${layers.join(", ")}`);
    const after = await callTool(client, "list_pending_actions", {});
    lines.push(`list_pending_actions -> [${(after.value.actions ?? []).map((action) => action.id).join(", ")}]`);
    return lines.join("\n  ");
  } finally {
    await client.close().catch(() => {});
  }
}

// Named setup steps. Each takes a Build ID; run.py checks names and arguments.
export const STEPS = {
  "settle-reconciliation": settleReconciliation,
  snapshot,
  "duplicate-order": duplicateOrder,
  "mcp-receipt": mcpReceipt,
};
