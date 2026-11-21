import { appendFileSync, mkdirSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import process from "node:process";
import { stripVTControlCharacters } from "node:util";
import type { FullResult, Reporter, Suite, TestCase, TestResult } from "@playwright/test/reporter";
import { STEPS } from "./steps.mjs";

type State = "pass" | "expected_fail" | "fail" | "unexpected_pass";

/** Where the score lands locally. The workflow uploads this file as `daily-loop-score`. */
const SCORE_FILE = resolve(
  import.meta.dirname,
  "../../test-results/daily-loop/daily-loop-score.json",
);

const numberOf = (test: TestCase) => Number(/^(\d+)\. /.exec(test.title)?.[1]);

/**
 * Scores the run: a step is `pass` only when it passed with no marker. A marked step
 * that passes (`unexpected_pass`) fails the run. So does `fail`: an unmarked step that
 * fails, and any step that did not run, timed out or stopped on a harness failure.
 */
export default class ScoreReporter implements Reporter {
  private tickets = new Map<number, string | null>();
  private states = new Map<number, State>();
  private reasons = new Map<number, string>();

  onBegin(_config: unknown, suite: Suite) {
    for (const test of suite.allTests()) {
      const ticket = test.annotations.find((a) => a.type === "ticket")?.description;
      this.tickets.set(numberOf(test), ticket ?? null);
    }
  }

  onTestEnd(test: TestCase, result: TestResult) {
    // Where the step stops: the failure's first line and the control it waited for.
    const lines = stripVTControlCharacters(result.error?.message ?? "").split("\n");
    const target = lines.find((line) => /waiting for |^Locator: /.test(line))?.trim();
    if (lines[0]) this.reasons.set(numberOf(test), [lines[0], target].filter(Boolean).join(" "));
    // A marker excuses the step's own failure, not a harness that did not get that far.
    const harness = /Harness: |browserType\.launch/.test(lines[0]);
    const marked = test.expectedStatus === "failed";
    // The spec adds `walked` when a body runs to its end; a pass without it did not run.
    const walked = [...test.annotations, ...result.annotations].some((a) => a.type === "walked");
    let state: State = "fail";
    if (result.status === "passed" && walked) state = marked ? "unexpected_pass" : "pass";
    else if (result.status === "failed" && marked && !harness) state = "expected_fail";
    this.states.set(numberOf(test), state);
  }

  onEnd(result: FullResult) {
    const steps = STEPS.map((title, index) => ({
      number: index + 1,
      title,
      state: this.states.get(index + 1) ?? "fail",
      ticket: this.tickets.get(index + 1) ?? null,
    }));
    const score = steps.filter((step) => step.state === "pass").length;
    const total = `Daily loop: ${score} of ${steps.length}`;
    const commit = process.env.PP_DAILY_LOOP_COMMIT ?? "";
    mkdirSync(dirname(SCORE_FILE), { recursive: true });
    writeFileSync(SCORE_FILE, `${JSON.stringify({ commit, score, steps }, null, 2)}\n`);

    console.log("");
    for (const step of steps) {
      const state = `${step.state.replace("_", "-")}${step.ticket ? ` ${step.ticket}` : ""}`;
      console.log(`Step ${String(step.number).padStart(2)}  ${state.padEnd(24)} ${step.title}`);
      if (this.reasons.has(step.number)) console.log(`         ${this.reasons.get(step.number)}`);
    }
    console.log(total);
    if (process.env.GITHUB_STEP_SUMMARY) {
      const rows = steps.map((s) => `| ${s.number} | ${s.title} | ${s.state} | ${s.ticket ?? ""} |`);
      const table = ["| # | Step | State | Ticket |", "| --- | --- | --- | --- |", ...rows];
      appendFileSync(
        process.env.GITHUB_STEP_SUMMARY,
        [`### ${total}`, "", `Commit \`${commit}\``, "", ...table, ""].join("\n"),
      );
    }
    const strict = steps.every((step) => step.state === "pass" || step.state === "expected_fail");
    return { status: strict ? result.status : ("failed" as const) };
  }
}
