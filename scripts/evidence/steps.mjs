// Named setup steps for capture.mjs. Each takes `api` (safety.mjs makeApi) and a
// Build ID, changes the disposable data through the Rust API, and returns a log line.
import { randomUUID } from "node:crypto";
import { CaptureError } from "./safety.mjs";

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

// Named setup steps. Each takes a Build ID; run.py checks names and arguments.
export const STEPS = {
  "settle-reconciliation": settleReconciliation,
  snapshot,
};
