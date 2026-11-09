// node --test scripts/evidence/steps.test.mjs (run.sh --self-test runs it too).
import assert from "node:assert/strict";
import test from "node:test";
import { CaptureError } from "./safety.mjs";
import { STEPS } from "./steps.mjs";

const NO_SNAPSHOT = "No snapshot is saved because this Build has no Accepted revision.";
const DRAFT = { draft_id: 68, state: "open", snapshot_digest: "a".repeat(64) };
const REVISION = {
  draft_id: 68,
  expected_base: { plan_version: 3, revision_id: 1 },
  expected_header_version: null,
  expected_lifecycle_version: 2,
  expected_snapshot_digest: "a".repeat(64),
};

/** A fake core: answers the reads the snapshot step makes and records every call. */
function fakeApi(snapshot) {
  const calls = [];
  const api = async (method, path) => {
    calls.push(`${method} ${path}`);
    if (path === "/api/v2/plans/2/drafts") return { drafts: [DRAFT] };
    if (path === "/api/v2/plans/2/drafts/68") return { draft: DRAFT, reconciliation: { kind: "ready" } };
    if (path === "/api/v2/plans/2/drafts/68/accept-preview") return { kind: "no_change", revision: REVISION, snapshot };
    if (path === "/api/v2/plans/2/drafts/68/apply") return {};
    throw new Error(`unexpected ${method} ${path}`);
  };
  return { api, calls };
}

test("snapshot refuses before Accept when Accept would save no snapshot", async () => {
  const { api, calls } = fakeApi({ saved_before_accept: false, detail: NO_SNAPSHOT });
  const error = await STEPS.snapshot(api, 2).catch((caught) => caught);
  assert.ok(error instanceof CaptureError);
  assert.equal(error.message, `Build 2: Accept saves no snapshot (${NO_SNAPSHOT})`);
  assert.ok(!calls.some((call) => call.endsWith("/apply")), "Accept must not run");
});

test("snapshot accepts and reports the snapshot when the preview says one is saved", async () => {
  const { api, calls } = fakeApi({ saved_before_accept: true, detail: "A snapshot is saved before Accept." });
  assert.equal(await STEPS.snapshot(api, 2), "accepted draft 68 (no_change); snapshot stored");
  assert.equal(calls.at(-1), "POST /api/v2/plans/2/drafts/68/apply");
});

test("a missing or long snapshot note still refuses with one short line", async () => {
  for (const snapshot of [undefined, { saved_before_accept: false, detail: `x\n${"y".repeat(500)}` }]) {
    const error = await STEPS.snapshot(fakeApi(snapshot).api, 2).catch((caught) => caught);
    assert.ok(error instanceof CaptureError);
    assert.doesNotMatch(error.message, /\n/);
    assert.ok(error.message.length < 220, error.message);
  }
});
