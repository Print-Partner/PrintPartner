// From web/: node --conditions=development --import tsx ../fixtures/parity/printer-checkoff-verify/generate.mjs [--check]
// Replays printer Checkoff verify and dismiss through the Node services on a disposable copy of
// a recorded fixture with synthetic links. No printer, webhook or Spoolman call happens here.
import assert from 'node:assert/strict';
import { copyFileSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { DatabaseSync } from 'node:sqlite';
import { normalizer } from '../checkoff-normalizer.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, '../../..');
const server = join(root, 'web/apps/server');
const { createSelfHostPorts } = await import(join(server, 'src/adapters/self-host/index.ts'));
const { verifyPrinterCheckoff, dismissHostFailedLink } = await import(join(server, 'src/services/printer-checkoff-verify.ts'));
const { AcceptedPlanOperationalIntegrityError } = await import(join(server, 'src/db/accepted-plan-operational.ts'));

const database = 'rust/crates/pp-storage/tests/fixtures/required-units/unchanged.db';
const queries = {
  progress: 'SELECT part_id, unit_index, completed, assembled FROM print_progress ORDER BY part_id, unit_index',
  units: 'SELECT token, profile_id, object_name FROM required_units ORDER BY token',
  jobs: 'SELECT id, tenant_id, profile_id, host_integration_id, printer_id, material, filename, status, filament_consumed_g, at, completed_at, link_id FROM print_jobs ORDER BY rowid',
  parts: 'SELECT id, tenant_id, job_id, at, profile_id, part_id, unit_index, result, reason, note, host_integration_id, filename, match_key, role, filament_display, link_id FROM print_job_parts ORDER BY rowid',
  links: "SELECT tenant_id, value FROM app_settings WHERE key = 'printer.checkoff_links' ORDER BY tenant_id",
};

const base = { integration_id: 'sim-moonraker', printer_id: 'sim-1', host_name: 'Simulated Voron', created_at: '2020-01-01T00:00:00.000Z' };
// Synthetic links, including stored shapes Node normalizes on its next write: a string part id,
// padded text, a legacy applied state, an unknown field, a lone surrogate and an entry that cannot load.
const links = [
  { ...base, id: 'link-verify', profile_id: 1, filename: 'plate-a.gcode', state: 'awaiting_verify', host_name: '  Simulated Voron  ',
    units: [{ part_id: '1', unit_index: 1, object_name: ' bracket__ppu_00000000000000000000000000000002 ' }, { part_id: 1, unit_index: 2 }, { part_id: 3, unit_index: 0 }, { part_id: 3, unit_index: 1 }],
    imported_inventory: { extras: [{ name: 'spare clip', kind: 'object', checkoff: { result: 'pending' } }] },
    remote_identity: { size_bytes: 1234.4, sha256: 'a'.repeat(64), ignored: true }, last_progress: 99.5, units_marked: 1, legacy_field: 'dropped' },
  { ...base, id: 'link-stale-file', profile_id: 1, filename: 'plate-a.gcode', state: 'awaiting_verify', units: [{ part_id: 1, unit_index: 1 }] },
  // A lone surrogate, as Node's .slice(0, n) leaves one; JSON.stringify stores it escaped.
  { ...base, id: 'link-watching', profile_id: 1, filename: 'plate-b.gcode', state: 'watching', saw_active: 1, host_name: 'Simulated Voron \uD83D', units: [{ part_id: 1, unit_index: 2 }] },
  { ...base, id: 'link-host-failed', profile_id: 1, filename: 'plate-c.gcode', state: 'host_failed', units: [{ part_id: 3, unit_index: 0 }] },
  { ...base, id: 'link-legacy-applied', profile_id: 1, filename: 'plate-d.gcode', state: 'applied', applied_at: '2020-01-02T00:00:00.000Z', units: [] },
  { ...base, id: 'link-burst', profile_id: 1, filename: 'plate-f.gcode', state: 'awaiting_verify', units: [{ part_id: 2, unit_index: 0 }, { part_id: 3, unit_index: 1 }] },
  { ...base, id: 'link-unmapped', profile_id: 1, filename: 'plate-e.gcode', state: 'awaiting_verify', units: [{ part_id: 9, unit_index: 0 }] },
  { ...base, id: '', profile_id: 1, filename: 'unloadable.gcode', state: 'watching', units: [] },
];
const foreign = [{ ...base, id: 'link-foreign', profile_id: 1, filename: 'foreign.gcode', state: 'awaiting_verify', units: [{ part_id: 1, unit_index: 1 }] }];

const confirm = (part_id, unit_index, extra = {}) => ({ part_id, unit_index, result: 'confirmed', ...extra });
const reject = (part_id, unit_index, reason, extra = {}) => ({ part_id, unit_index, result: 'rejected', reason, ...extra });
// The long note splits a surrogate pair at 500 UTF-16 units, so Node stores a lone surrogate
// escape that every later step has to load.
const calls = [
  { op: 'verify', body: { link_id: 'link-verify', decisions: [confirm(1, 2)] } },
  { op: 'verify', body: { link_id: 'link-verify', decisions: [confirm(1, 1, { note: '  first layer clean  ', reason: '' }), reject(3, 0, 'warping', { note: '' })] } },
  { op: 'verify', body: { link_id: 'link-verify', decisions: [confirm(1, 1, { note: '  first layer clean  ', reason: '' }), reject(3, 0, 'warping', { note: '' })] } },
  { op: 'reopen' },
  { op: 'verify', body: { link_id: 'link-stale-file', decisions: [confirm(1, 1)] } },
  { op: 'verify', body: { link_id: 'link-watching', decisions: [confirm(1, 2)] } },
  { op: 'verify', body: { link_id: 'link-unmapped', decisions: [confirm(9, 0)] } },
  { op: 'verify', body: { link_id: 'link-foreign', decisions: [confirm(1, 1)] } },
  { op: 'verify', body: { link_id: ' ', decisions: [confirm(1, 2)] } },
  { op: 'verify', body: { link_id: 'link-verify', decisions: [reject(1, 2, 'not-a-reason')] } },
  { op: 'verify', body: { link_id: 'link-verify', decisions: [confirm(1, 2)], additional_decisions: [{ index: 3, result: 'confirmed' }] } },
  { op: 'verify', body: { link_id: 'link-verify', decisions: [confirm(1, 2), reject(3, 1, 'layer_shift', { note: 'x'.repeat(499) + '\u{1F600}' + 'x'.repeat(100) })], additional_decisions: [{ index: 0, result: 'rejected', reason: 'other', note: ' bent ' }] } },
  { op: 'dismiss', body: { link_id: 'link-host-failed' } },
  { op: 'dismiss', body: { link_id: 'link-host-failed' } },
  { op: 'dismiss', body: { link_id: 'missing' } },
  // The route reads String(link_id).trim(): an array joins, so these find a link or none.
  { op: 'verify', body: { link_id: [' link-stale-file '], decisions: [confirm(1, 1)] } },
  { op: 'dismiss', body: { link_id: [] } },
  { op: 'dismiss', body: { link_id: 'link-host-failed\u0085' } },
  // Node writes Part 2 and then refuses Part 3, committing half of the burst. Kept last so the
  // Rust replay, which writes nothing here, matches every earlier step exactly.
  { op: 'verify', body: { link_id: 'link-burst', decisions: [confirm(2, 0), confirm(3, 1)] }, partialInNode: true },
];

function receipt(call, repo) {
  const linkId = String(call.body.link_id ?? '').trim();
  if (!linkId) return { status: 400, body: { detail: 'link_id is required' } };
  let result;
  try {
    result = call.op === 'verify'
      ? verifyPrinterCheckoff(repo, linkId, call.body.decisions, call.body.additional_decisions)
      : dismissHostFailedLink(repo, linkId);
  } catch (error) {
    const detail = error instanceof AcceptedPlanOperationalIntegrityError ? 'Accepted Plan data is inconsistent' : 'Internal Server Error';
    return { status: 500, body: { detail } };
  }
  if ('error' in result) return { status: result.status, body: { detail: result.error } };
  return { status: 200, body: JSON.parse(JSON.stringify(call.op === 'verify' ? result : { link: result })) };
}


const directory = mkdtempSync(join(tmpdir(), 'pp-printer-checkoff-verify-'));
copyFileSync(join(root, database), join(directory, 'print-partner.db'));
let raw = new DatabaseSync(join(directory, 'print-partner.db'));
let ports;
const normalize = normalizer();
const setup = [
  'UPDATE print_progress SET completed=1, assembled=1 WHERE part_id=1 AND unit_index=0',
  "INSERT INTO app_settings (tenant_id, key, value) VALUES ('default', 'printer.checkoff_links', " + "'" + JSON.stringify(links).replaceAll("'", "''") + "')",
  "INSERT INTO app_settings (tenant_id, key, value) VALUES ('foreign', 'printer.checkoff_links', " + "'" + JSON.stringify(foreign).replaceAll("'", "''") + "')",
];
const read = () => {
  const rows = Object.fromEntries(Object.entries(queries).map(([name, sql]) => [name, raw.prepare(sql).all().map((row) => ({ ...row }))]));
  rows.links = rows.links.map((row) => ({ tenant_id: row.tenant_id, value: JSON.parse(row.value) }));
  return normalize(rows);
};
const output = [];
let reopenedAt = null;
let before;
try {
  for (const sql of setup) raw.exec(sql);
  before = read();
  raw.close();
  ports = createSelfHostPorts(directory);
  await ports.db.connect();
  raw = new DatabaseSync(join(directory, 'print-partner.db'));
  for (const call of calls) {
    if (call.op === 'reopen') {
      reopenedAt = output.length;
      raw.close();
      await ports.db.close();
      ports = createSelfHostPorts(directory);
      await ports.db.connect();
      raw = new DatabaseSync(join(directory, 'print-partner.db'));
      continue;
    }
    const previous = output.at(-1)?.rows ?? before;
    const result = normalize(receipt(call, ports.repository));
    const rows = read();
    if (result.status !== 200 && !call.partialInNode) {
      for (const name of Object.keys(queries)) assert.deepEqual(rows[name], previous[name], 'refusal keeps ' + name);
    }
    if (call.partialInNode) {
      assert.equal(result.status, 400);
      assert.notDeepEqual(rows.progress, previous.progress, 'Node keeps the first Part of a refused burst');
    }
    assert.deepEqual(rows.units, before.units, 'Required units never change');
    output.push({ op: call.op, body: call.body, ...(call.partialInNode ? { partialInNode: true } : {}), receipt: result, rows });
  }
} finally {
  raw.close();
  await ports?.db.close();
  rmSync(directory, { recursive: true, force: true });
}
assert.notEqual(reopenedAt, null);

const path = join(here, 'node.json');
const line = (value) => JSON.stringify(value);
const bytes = '{"_generated": "@generated by fixtures/parity/printer-checkoff-verify/generate.mjs",\n"database": ' + line(database) +
  ',\n"queries": ' + line(queries) + ',\n"setup": ' + line(setup) + ',\n"reopenedAt": ' + reopenedAt + ',\n"before": ' + line(before) +
  ',\n"steps": [\n' + output.map(line).join(',\n') + '\n]}\n';
if (process.argv.includes('--check')) {
  assert.equal(readFileSync(path, 'utf8'), bytes, 'printer Checkoff verify fixtures are stale');
  console.log('node.json matches the Node printer Checkoff verify replay');
} else {
  writeFileSync(path, bytes);
  console.log('Wrote ' + output.length + ' printer Checkoff verify steps to node.json');
}
