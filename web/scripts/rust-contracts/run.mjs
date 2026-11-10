import process from "node:process";
import console from "node:console";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { readFileSync, readdirSync, mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { URL, fileURLToPath } from "node:url";
import { sha256, verifyFreshness } from "../desktop-contracts/freshness.mjs";
import { sourcePaths } from "./sources.mjs";

const repo = fileURLToPath(new URL("../../../", import.meta.url));
const web = join(repo, "web");
const mode = process.argv[2];
assert(["--write", "--check"].includes(mode), "Usage: run.mjs --write|--check [RECEIPTS]");
const receipts = process.argv[3] ?? mkdtempSync(join(tmpdir(), "pp-rust-contracts-"));
mkdirSync(receipts, {recursive: true});
const checks = [];
function run(name, executable, args, cwd = repo, expectedExit = 0) {
  const result = spawnSync(executable, args, {cwd, encoding: "utf8", maxBuffer: 64 * 1024 * 1024});
  writeFileSync(join(receipts, `${name}.log`), `${result.stdout ?? ""}${result.stderr ?? ""}`);
  checks.push({name, args, exit: result.status, expectedExit});
  writeFileSync(join(receipts, "checks.json"), JSON.stringify(checks, null, 2) + "\n");
  assert.equal(result.status, expectedExit, `${name}: ${result.stderr}`);
}
const sourceHashes = () => Object.fromEntries(sourcePaths(repo).map((path) => [join(repo, path), sha256(readFileSync(join(repo, path)))]));
let sources = mode === "--check" ? sourceHashes() : null;

const cargo = process.env.CARGO ?? "cargo";
const schema = join(web, "scripts/rust-contracts/schemas.json");
const output = join(web, "packages/contracts/src/rust-wire.generated.ts");
run("rust-export", cargo, ["run", "--locked", "--manifest-path", "rust/Cargo.toml", "-p", "pp-api-types", "--features", "codegen", "--bin", "export-rust-contracts", "--", mode, schema]);
run("zod-generation", process.execPath, ["scripts/rust-contracts/generate.mjs", mode, schema, output], web);
if (mode === "--check") {
  const planted = join(receipts, "planted-schema-drift.json");
  writeFileSync(planted, readFileSync(schema, "utf8") + "\n");
  run("native-schema-drift-control", cargo, ["run", "--locked", "--manifest-path", "rust/Cargo.toml", "-p", "pp-api-types", "--features", "codegen", "--bin", "export-rust-contracts", "--", "--check", planted], repo, 1);
  assert.match(readFileSync(join(receipts, "native-schema-drift-control.log"), "utf8"), /Rust contract schema drift/);
  run("old-t9-semantic-oracle", process.execPath, ["--conditions=development", "--import", "tsx", "--test", "scripts/desktop-contracts/semantics.test.mjs"], web);
  run("old-t9-autosave-oracle", process.execPath, ["--conditions=development", "--import", "tsx", "scripts/desktop-contracts/generate.mjs", "--check", receipts], web);
  const original = JSON.parse(readFileSync(join(receipts, "node-cases.json"), "utf8")).supplemental;
  const frozen = JSON.parse(readFileSync(join(web, "scripts/rust-contracts/supplemental.json"), "utf8")).cases;
  assert.equal(original.length, 50);
  assert.deepEqual(frozen, original.map(({name, parser, raw_input, input, outcome}) => ({name, parser, ...(raw_input ? {raw_input} : {input}), outcome})), "Shared T9 supplemental corpus drift");
  run("generated-parity-controls", process.execPath, ["--conditions=development", "--import", "tsx", "--test", "scripts/rust-contracts/semantics.test.mjs", "scripts/rust-contracts/receipts.test.mjs"], web);
  run("generated-typecheck", process.execPath, ["node_modules/typescript/bin/tsc", "--noEmit", "--strict", "--skipLibCheck", "--target", "es2022", "--lib", "es2022", "--module", "nodenext", "--moduleResolution", "nodenext", "packages/contracts/src/rust-wire.generated.ts"], web);
  run("native-independent-parity", cargo, ["test", "--locked", "--manifest-path", "rust/Cargo.toml", "-p", "pp-api-types", "--features", "codegen", "--test", "node_boundary_contracts", "--", "ticket_t_63_contracts"]);
  const nativeLog = readFileSync(join(receipts, "native-independent-parity.log"), "utf8");
  const counts = /test result: ok\. (\d+) passed; 0 failed/.exec(nativeLog);
  assert(counts && Number(counts[1]) >= 9, "Reverse native parity must execute every named test with nonzero coverage");
}
sources ??= sourceHashes();
const artifacts = Object.fromEntries(readdirSync(receipts).filter((name) => name !== "manifest.json").map((name) => [name, sha256(readFileSync(join(receipts, name)))]));
writeFileSync(join(receipts, "manifest.json"), JSON.stringify({status: "PASS", mode, sources, artifacts}, null, 2) + "\n");
verifyFreshness(receipts);
console.log(`PASS: Rust reverse-generation ${mode}; receipts ${receipts}`);
