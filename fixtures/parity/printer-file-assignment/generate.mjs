// From web/: node --conditions=development --import tsx ../fixtures/parity/printer-file-assignment/generate.mjs [--check]
// Replays the create path of materializeAcceptedPrinterLink, and the completed patch the
// print-file assignment route applies after it, on a disposable copy of a recorded fixture.
// No printer, upload or file read happens here: the route's request parsing and file
// inspection are not part of this golden.
import assert from 'node:assert/strict';
import { copyFileSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { DatabaseSync } from 'node:sqlite';

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, '../../..');
const server = join(root, 'web/apps/server');
const { createSelfHostPorts } = await import(join(server, 'src/adapters/self-host/index.ts'));
const { updatePrinterCheckoffLink } = await import(join(server, 'src/services/printer-checkoff-store.ts'));

const database = 'rust/crates/pp-storage/tests/fixtures/required-units/unchanged.db';
const queries = {
  progress: 'SELECT part_id, unit_index, completed, assembled FROM print_progress ORDER BY part_id, unit_index',
  links: "SELECT tenant_id, value FROM app_settings WHERE key = 'printer.checkoff_links' ORDER BY tenant_id",
};
// One open link already watches open.gcode on sim-moonraker.
const links = [
  { id: 'link-open', profile_id: 1, integration_id: 'sim-moonraker', printer_id: 'sim-1', host_name: 'Simulated Voron', filename: 'open.gcode',
    state: 'watching', created_at: '2020-01-01T00:00:00.000Z', units: [{ part_id: 3, unit_index: 0 }] },
];
const setup = [
  'UPDATE print_progress SET completed=1 WHERE part_id=1 AND unit_index=0',
  "INSERT INTO app_settings (tenant_id, key, value) VALUES ('default', 'printer.checkoff_links', '" + JSON.stringify(links).replaceAll("'", "''") + "')",
];

// `materializeProblem` in routes/printer-checkoff.ts for the outcomes the create path returns.
const problems = {
  empty: 'Accepted Plan has no Required units',
  stale_plan_revision: 'The Accepted Plan moved on; reload the Build and choose the file again',
  no_match: 'That print file does not map to an incomplete Required unit in this Build',
  already_linked: 'That print file is already assigned',
};
const HOST_COMPLETED_PATCH = { state: 'awaiting_verify', host_outcome: 'success', saw_active: true, last_progress: 100 };

const name = (token) => 'bracket__ppu_0000000000000000000000000000000' + token;
const link = (filename, extra = {}) => ({ integrationId: 'sim-moonraker', printerId: 'sim-1', hostName: 'Simulated Voron', filename, started: false, ...extra });
const unit = (part_id, unit_index) => ({ part_id, unit_index });
const assign = (filename, fields = {}, linkExtra = {}) => ({
  profileId: 1, expectedPlanRevisionId: 1, objectNames: [], confirmedUnits: [], retainUnmatched: false, completed: false,
  ...fields, link: link(filename, linkExtra),
});
const steps = [
  assign('a.gcode', { objectNames: [name(2), 'gear.stl'], confirmedUnits: [unit(1, 1), unit(3, 1)] }),
  assign('A.GCODE', { confirmedUnits: [unit(1, 2)] }),
  assign('open.gcode', { confirmedUnits: [unit(1, 2)] }),
  assign('b.gcode', { expectedPlanRevisionId: 99, confirmedUnits: [unit(1, 2)] }),
  assign('c.gcode', { confirmedUnits: [unit(1, 0)] }),
  assign('d.gcode', { confirmedUnits: [unit(1, 2), unit(1, 2)] }),
  assign('e.gcode', { confirmedUnits: [unit(2, 0)] }),
  assign('f.gcode', { confirmedUnits: [unit(9, 0)] }),
  assign('g.gcode', { objectNames: ['mystery_a', 'mystery_b'], confirmedUnits: [unit(1, 2), unit(3, 1)],
    objectMappings: [{ object_index: 1, part_id: 1, unit_index: 2 }, { object_index: 0, part_id: 3, unit_index: 1 }] }),
  assign('h.gcode', { objectNames: [name(2), 'mystery'], confirmedUnits: [unit(1, 2)], objectMappings: [{ object_index: 0, part_id: 1, unit_index: 2 }] }),
  assign('i.gcode', { objectNames: ['mystery'], confirmedUnits: [unit(1, 2)], objectMappings: [{ object_index: 0, part_id: 3, unit_index: 1 }] }),
  assign('j.gcode', { objectNames: ['mystery', 'mystery'], confirmedUnits: [unit(1, 2)],
    objectMappings: [{ object_index: 0, part_id: 1, unit_index: 2 }, { object_index: 0, part_id: 1, unit_index: 2 }] }),
  assign('k.gcode', { objectNames: [name(2), name(2), name(1), 'unknown object', 'gear.stl', 'gear.stl', 'gear.stl'],
    confirmedUnits: [unit(1, 1)], objectMappings: [], retainUnmatched: true }),
  assign('l.gcode', { confirmedUnits: [], objectMappings: [], retainUnmatched: true }),
  assign('m.gcode', { confirmedUnits: [unit(3, 0)], completed: true }),
  assign('n.3mf', { objectNames: ['gear.stl'], confirmedUnits: [unit(3, 1)] }, {
    hostName: '   ', remotePath: '  gcodes/n.3mf  ', remoteIdentity: { size_bytes: 10, sha256: 'b'.repeat(64), modified_at: '2020-01-03T00:00:00.000Z' },
    classification: { format: '3mf', kind: 'toolpath_package' } }),
  assign('o.gcode', { objectNames: [name(3)], confirmedUnits: [unit(1, 2)], objectMappings: [] }),
  assign('p.gcode', { profileId: 2, confirmedUnits: [unit(1, 2)] }),
];

function receipt(command, repo) {
  const { completed, ...rest } = command;
  let materialized;
  try {
    materialized = repo.materializeAcceptedPrinterLink({ kind: 'create', ...rest });
  } catch {
    return { status: 500, body: { detail: 'Internal Server Error' } };
  }
  if (materialized.kind === 'accepted_state_unavailable') {
    const detail = materialized.reason === 'compatibility_dirty' ? 'Accepted Plan requires compatibility repair' : 'Accepted Plan operational state is not initialized';
    return { status: 409, body: { detail } };
  }
  if (materialized.kind !== 'created') return { status: 409, body: { detail: problems[materialized.kind] } };
  if (!completed) return { status: 200, body: JSON.parse(JSON.stringify({ link: materialized.link, attribution: materialized.attribution })) };
  const done = updatePrinterCheckoffLink(repo, materialized.link.id, { ...HOST_COMPLETED_PATCH, completed_at: new Date().toISOString() }, { requireState: 'watching' });
  assert.ok(done, 'the completed patch applies');
  return { status: 200, body: JSON.parse(JSON.stringify({ link: done, attribution: materialized.attribution })) };
}

// Generated ids and times differ per run. Seeded times use 2020, so any other ISO time is "now".
function normalizer() {
  const ids = new Map();
  const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
  const iso = /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}Z$/;
  const walk = (value) => {
    if (Array.isArray(value)) return value.map(walk);
    if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, walk(v)]));
    if (typeof value !== 'string') return value;
    if (uuid.test(value)) {
      if (!ids.has(value)) ids.set(value, '<uuid:' + ids.size + '>');
      return ids.get(value);
    }
    return iso.test(value) && !value.startsWith('2020-') ? '<now>' : value;
  };
  return walk;
}

const directory = mkdtempSync(join(tmpdir(), 'pp-printer-file-assignment-'));
copyFileSync(join(root, database), join(directory, 'print-partner.db'));
let raw = new DatabaseSync(join(directory, 'print-partner.db'));
let ports;
const normalize = normalizer();
const read = () => {
  const rows = Object.fromEntries(Object.entries(queries).map(([key, sql]) => [key, raw.prepare(sql).all().map((row) => ({ ...row }))]));
  rows.links = rows.links.map((row) => ({ tenant_id: row.tenant_id, value: JSON.parse(row.value) }));
  return normalize(rows);
};
const output = [];
let before;
try {
  for (const sql of setup) raw.exec(sql);
  before = read();
  raw.close();
  ports = createSelfHostPorts(directory);
  await ports.db.connect();
  raw = new DatabaseSync(join(directory, 'print-partner.db'));
  for (const command of steps) {
    const previous = output.at(-1)?.rows ?? before;
    const result = normalize(receipt(command, ports.repository));
    const rows = read();
    if (result.status !== 200) assert.deepEqual(rows, previous, 'a refusal writes nothing');
    assert.deepEqual(rows.progress, before.progress, 'an assignment checks nothing off');
    output.push({ command, receipt: result, rows });
  }
} finally {
  raw.close();
  await ports?.db.close();
  rmSync(directory, { recursive: true, force: true });
}
const kinds = new Set(output.map((step) => step.receipt.body.detail ?? 'created'));
assert.equal(kinds.size, 5, 'the corpus covers created and every refusal it can reach');

const path = join(here, 'node.json');
const line = (value) => JSON.stringify(value);
const bytes = '{"_generated": "@generated by fixtures/parity/printer-file-assignment/generate.mjs",\n"database": ' + line(database) +
  ',\n"queries": ' + line(queries) + ',\n"setup": ' + line(setup) + ',\n"before": ' + line(before) +
  ',\n"steps": [\n' + output.map(line).join(',\n') + '\n]}\n';
if (process.argv.includes('--check')) {
  assert.equal(readFileSync(path, 'utf8'), bytes, 'printer file assignment fixtures are stale');
  console.log('node.json matches the Node printer file assignment replay');
} else {
  writeFileSync(path, bytes);
  console.log('Wrote ' + output.length + ' printer file assignment steps to node.json');
}
