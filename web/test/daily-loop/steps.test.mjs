import assert from "node:assert/strict";
import test from "node:test";
import { STEPS, documentedSteps } from "./steps.mjs";

test("the twelve test titles are the twelve steps in docs/product/daily-loop.md", () => {
  assert.deepEqual(
    documentedSteps(),
    STEPS.map((title, index) => ({ number: index + 1, title })),
  );
  assert.equal(STEPS.length, 12);
});

test("a changed, added or removed step in the table is seen", () => {
  const row = (number, title) => `| ${number} | ${title} | Done text |\n`;
  const table = STEPS.map((title, index) => row(index + 1, title)).join("");
  assert.equal(documentedSteps(table).length, 12);
  assert.equal(documentedSteps(table + row(13, "Prints a label")).length, 13);
  assert.notDeepEqual(documentedSteps(table.replace("Imports", "Restores")), documentedSteps(table));
});
