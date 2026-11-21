import assert from "node:assert/strict";
import console from "node:console";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import process from "node:process";
import test from "node:test";
import ScoreReporter from "./score.ts";
import { STEPS } from "./steps.mjs";

// Drives the reporter the way Playwright does: onBegin with the twelve tests, onTestEnd
// for each, then onEnd. Each case sets how step 3 ends; the other steps pass unmarked.

const ENTERED = { type: "entered" };
const WALKED = { type: "walked" };
const STEP_ERROR = "Error: This step's test is not written past here. Still to check: a printer.";

/**
 * A reporter that writes its score file to a new temporary folder, prints nothing, and
 * adds nothing to a CI job summary.
 */
function isolatedReporter(t) {
  const dir = mkdtempSync(join(tmpdir(), "daily-loop-score-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  t.mock.method(console, "log", () => {});
  const summary = process.env.GITHUB_STEP_SUMMARY;
  delete process.env.GITHUB_STEP_SUMMARY;
  t.after(() => {
    if (summary !== undefined) process.env.GITHUB_STEP_SUMMARY = summary;
  });
  const scoreFile = join(dir, "daily-loop-score.json");
  return { reporter: new ScoreReporter({ scoreFile }), read: () => JSON.parse(readFileSync(scoreFile, "utf8")) };
}

/** Runs one scored run and returns what onEnd answers and the score file it wrote. */
async function score(t, step3) {
  const { reporter, read } = isolatedReporter(t);

  const runs = STEPS.map((title, index) => {
    const number = index + 1;
    const ticket = number === 3 ? step3.ticket : null;
    const test = {
      title: `${number}. ${title}`,
      expectedStatus: ticket ? "failed" : "passed",
      annotations: ticket ? [{ type: "ticket", description: ticket }] : [],
    };
    const result =
      number === 3
        ? { status: step3.status, annotations: step3.annotations, error: step3.error }
        : { status: "passed", annotations: [ENTERED, WALKED] };
    return { test, result };
  });
  reporter.onBegin({}, { allTests: () => runs.map((run) => run.test) });
  for (const { test, result } of runs) reporter.onTestEnd(test, result);
  const answer = await reporter.onEnd({ status: "passed" });
  return { answer, file: read() };
}

const cases = [
  {
    name: "an unmarked step that passes is pass",
    step3: { ticket: null, status: "passed", annotations: [ENTERED, WALKED] },
    state: "pass",
    score: 12,
    run: "passed",
  },
  {
    name: "a marked step that fails in its body is expected_fail",
    step3: { ticket: "GRE-1531", status: "failed", annotations: [ENTERED], error: { message: STEP_ERROR } },
    state: "expected_fail",
    score: 11,
    run: "passed",
  },
  {
    name: "a marked step that passes is unexpected_pass and fails the run",
    step3: { ticket: "GRE-1531", status: "passed", annotations: [ENTERED, WALKED] },
    state: "unexpected_pass",
    score: 11,
    run: "failed",
  },
  {
    name: "an unmarked step that fails is fail and fails the run",
    step3: { ticket: null, status: "failed", annotations: [ENTERED], error: { message: STEP_ERROR } },
    state: "fail",
    score: 11,
    run: "failed",
  },
  {
    name: "a marked step stopped by the harness is fail and fails the run",
    step3: {
      ticket: "GRE-1531",
      status: "failed",
      annotations: [ENTERED],
      error: { message: "Error: Harness: pp-server is not ready. Timeout" },
    },
    state: "fail",
    score: 11,
    run: "failed",
  },
  {
    name: "a marked step that fails before its body starts is fail and fails the run",
    step3: {
      ticket: "GRE-1531",
      status: "failed",
      annotations: [],
      error: { message: "Error: browser.newContext: Target page, context or browser has been closed" },
    },
    state: "fail",
    score: 11,
    run: "failed",
  },
  {
    name: "a step reported passed whose body never ran is fail and fails the run",
    step3: { ticket: null, status: "passed", annotations: [] },
    state: "fail",
    score: 11,
    run: "failed",
  },
  {
    name: "a marked step that times out is fail and fails the run",
    step3: { ticket: "GRE-1531", status: "timedOut", annotations: [ENTERED], error: { message: "Test timeout" } },
    state: "fail",
    score: 11,
    run: "failed",
  },
];

for (const c of cases) {
  test(c.name, async (t) => {
    const { answer, file } = await score(t, c.step3);
    const step3 = file.steps[2];
    assert.deepEqual(step3, { number: 3, title: STEPS[2], state: c.state, ticket: c.step3.ticket });
    assert.equal(file.score, c.score);
    assert.equal(file.steps.length, 12);
    assert.deepEqual(answer, { status: c.run });
  });
}

test("a step with no result is fail", async (t) => {
  const { reporter, read } = isolatedReporter(t);
  reporter.onBegin({}, { allTests: () => [] });
  assert.deepEqual(await reporter.onEnd({ status: "passed" }), { status: "failed" });
  const file = read();
  assert.deepEqual(new Set(file.steps.map((step) => step.state)), new Set(["fail"]));
  assert.equal(file.score, 0);
});
