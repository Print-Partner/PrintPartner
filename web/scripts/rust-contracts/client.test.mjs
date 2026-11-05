// Round-trip tests for @print-partner/rust-client against Rust-serialized
// fixtures. `client.mjs --check` passes the exported document in
// PP_RUST_CLIENT_DOCUMENT; run standalone, the test exports it with cargo.
import process from "node:process";
import assert from "node:assert/strict";
import test from "node:test";
import { cpSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { compare, exportDocument, generate, packageSource, verifyIntegrity } from "./client.mjs";
import * as client from "../../packages/rust-client/src/index.ts";

const { dtoSchemas, routePath, routes } = client;

const document = JSON.parse(readFileSync(process.env.PP_RUST_CLIENT_DOCUMENT ?? exportDocument(), "utf8"));
const requestDtos = new Set(document.routes.map((route) => route.request).filter(Boolean));
const UNSAFE = Number.MAX_SAFE_INTEGER + 1;

/** Every [path, value] in a JSON value, objects and leaves alike. */
function* nodes(value, path = []) {
  yield [path, value];
  if (value && typeof value === "object") for (const [key, child] of Object.entries(value)) yield* nodes(child, [...path, key]);
}
function replaced(value, path, next) {
  if (path.length === 0) return next(value);
  const copy = Array.isArray(value) ? [...value] : { ...value };
  const result = replaced(value[path[0]], path.slice(1), next);
  if (result === undefined) delete copy[path[0]];
  else copy[path[0]] = result;
  return copy;
}
const wrongType = (value) => (value === null ? false : typeof value === "string" ? 1 : typeof value === "number" ? String(value) : typeof value === "boolean" ? "true" : Array.isArray(value) ? {} : []);

test("the package is the generator's output for this Rust contract", () => {
  compare(generate(document));
});

test("routes cover Builds, layers, v2 drafts and health, all generated from pp-api", () => {
  assert.deepEqual(Object.keys(routes), document.routes.map((route) => route.name));
  const covered = Object.values(routes).map((route) => `${route.method} ${route.path}`);
  for (const required of ["GET /api/v2/plans", "GET /api/v2/plans/{id}", "GET /api/v2/plans/{id}/layers", "GET /api/v2/plans/{id}/drafts", "GET /api/v2/plans/{id}/drafts/{draft_id}", "PATCH /api/v2/plans/{id}/drafts/{draft_id}/parts", "POST /api/v2/plans/{id}/drafts/{draft_id}/abandon", "GET /health"]) assert(covered.includes(required), required);
  assert.equal(routes.readHealth.auth, "none");
  assert(Object.values(routes).filter((route) => route.path !== "/health").every((route) => route.auth === "credential"));
  assert.equal(routePath("editDraftParts", { id: 7, draft_id: Number.MAX_SAFE_INTEGER }), "/api/v2/plans/7/drafts/9007199254740991/parts");
  assert.throws(() => routePath("readBuild", { id: UNSAFE }), RangeError);
  assert.throws(() => routePath("readBuild", { id: 0 }), RangeError);
});

test("every DTO has accepted Rust fixtures", () => {
  assert.deepEqual(Object.keys(dtoSchemas).sort(), Object.keys(document.dtos).sort());
  for (const name of Object.keys(dtoSchemas)) assert(document.fixtures.some((fixture) => fixture.dto === name && fixture.rust_accepts), name);
});

test("descriptor tables hold Rust's rows, parse with their DTO and cover the key enum", () => {
  assert(document.descriptors.length > 0);
  for (const table of document.descriptors) {
    const generated = client[table.name];
    assert.deepEqual(Object.values(generated), table.values, table.name);
    const schema = dtoSchemas[table.dto];
    for (const [key, row] of Object.entries(generated)) {
      assert.deepEqual(schema.parse(row), row, `${table.name}.${key}`);
      assert.equal(row[table.key], key);
    }
    assert.deepEqual(Object.keys(generated), schema.shape[table.key].options);
  }
});

test("a descriptor table missing a key value is refused", () => {
  const changed = JSON.parse(JSON.stringify(document));
  changed.descriptors[0].values.pop();
  assert.throws(() => generate(changed), /must list every/);
});

// Members Rust omits when empty: RFC 9457 problem members, and Health's `modules`,
// which Node and the other Rust routers do not send.
const optionalMembers = { Problem: new Set(["code", "detail", "errors"]), Health: new Set(["modules"]) };

for (const fixture of document.fixtures) {
  test(`${fixture.dto} ${fixture.name}: agrees with Rust and round-trips`, () => {
    const schema = dtoSchemas[fixture.dto];
    const parsed = schema.safeParse(fixture.value);
    assert.equal(parsed.success, fixture.rust_accepts, JSON.stringify(parsed.error?.issues));
    if (!fixture.rust_accepts) return;
    // Unicode text, nulls and safe-integer extremes come back unchanged.
    assert.deepEqual(parsed.data, fixture.value);
    const strict = requestDtos.has(fixture.dto);
    for (const [path, value] of nodes(fixture.value)) {
      const where = `${fixture.dto} ${fixture.name} at ${path.join(".") || "(root)"}`;
      const rejects = (next, why) => assert.equal(schema.safeParse(replaced(fixture.value, path, next)).success, false, `${where}: ${why}`);
      if (value && typeof value === "object" && !Array.isArray(value)) {
        // Requests reject unknown keys like serde's deny_unknown_fields; responses strip them.
        const extended = schema.safeParse(replaced(fixture.value, path, (object) => ({ ...object, unknown_key: 1 })));
        if (strict) assert.equal(extended.success, false, `${where}: request must reject unknown keys`);
        else assert.deepEqual(extended.data, fixture.value, `${where}: response must strip unknown keys`);
      }
      const optional = path.length === 1 && optionalMembers[fixture.dto]?.has(path[0]);
      if (path.length > 0 && !/^\d+$/.test(path.at(-1)) && !optional) {
        // Every covered field is required: omitted is never the same as null.
        rejects(() => undefined, "omitted field accepted");
      }
      if (path.length > 0) rejects(wrongType, "wrong type accepted");
      if (Number.isInteger(value)) {
        rejects(() => UNSAFE, "unsafe integer accepted");
        rejects((number) => number + 0.5, "fraction accepted");
      }
    }
  });
}

test("hand edits and stray files fail the check without cargo", () => {
  const copy = mkdtempSync(join(tmpdir(), "pp-rust-client-edit-"));
  try {
    cpSync(packageSource, copy, { recursive: true });
    verifyIntegrity(copy);
    const path = join(copy, "schemas.generated.ts");
    writeFileSync(path, readFileSync(path, "utf8").replace("z.string()", "z.any()"));
    assert.throws(() => verifyIntegrity(copy), /hand edit detected/);
    cpSync(packageSource, copy, { recursive: true });
    writeFileSync(join(copy, "extra.ts"), "export {};\n");
    assert.throws(() => verifyIntegrity(copy), /only generated files/);
  } finally {
    rmSync(copy, { recursive: true, force: true });
  }
});

test("a changed Rust schema is drift", () => {
  const changed = JSON.parse(JSON.stringify(document));
  changed.dtos.Health.required = changed.dtos.Health.required.filter((key) => key !== "degraded_detail");
  assert.throws(() => compare(generate(changed)), /drift from Rust/);
  const unsupported = JSON.parse(JSON.stringify(document));
  unsupported.dtos.Health.properties.ok.format = "float";
  assert.throws(() => generate(unsupported), /Unsupported/);
});
