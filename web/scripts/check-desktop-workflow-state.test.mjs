import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { chmodSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import process from 'node:process';
import test from 'node:test';
import { fileURLToPath, URL } from 'node:url';

const cli = fileURLToPath(new URL('./check-desktop-workflow-state.mjs', import.meta.url));
const path = '.github/workflows/desktop-contracts.yml';

function run(t, response, overrides = {}) {
  const directory = mkdtempSync(join(tmpdir(), 'workflow-state-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const gh = join(directory, 'gh');
  writeFileSync(gh, `#!/usr/bin/env node
const expected = ['api', '--hostname', 'github.com', 'repos/Print-Partner/PrintPartner/actions/workflows/desktop-contracts.yml'];
if (JSON.stringify(process.argv.slice(2)) !== JSON.stringify(expected)) process.exit(9);
if (process.env.TEST_API_ERROR) { console.error('fixture API error'); process.exit(1); }
console.log(process.env.TEST_API_RESPONSE);
`);
  chmodSync(gh, 0o755);
  return spawnSync(process.execPath, [cli], {
    encoding: 'utf8',
    env: { ...process.env, PATH: `${directory}:${process.env.PATH}`, GITHUB_REPOSITORY: 'Print-Partner/PrintPartner',
      GH_TOKEN: 'fixture-token', TEST_API_RESPONSE: response, TEST_API_ERROR: '', ...overrides },
  });
}

test('accepts active workflow at the canonical API path', t => {
  const result = run(t, JSON.stringify({ path, state: 'active' }));
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stdout, 'desktop-contracts.yml API state=active\n');
});

for (const state of ['disabled_manually', 'disabled_inactivity', 'disabled_fork', 'deleted', 'unknown', '', null]) {
  test(`rejects API workflow state ${JSON.stringify(state)}`, t => {
    const result = run(t, JSON.stringify({ path, state }));
    assert.equal(result.status, 1, result.stdout + result.stderr);
    assert.match(result.stderr, /INVALID_DESKTOP_WORKFLOW_STATE/);
    assert.equal(result.stdout, '');
  });
}

for (const response of ['not JSON', 'null', '[]', '{}', JSON.stringify({ state: 'active' }),
  JSON.stringify({ path: '.github/workflows/other.yml', state: 'active' })]) {
  test(`rejects invalid API response ${response}`, t => {
    const result = run(t, response);
    assert.equal(result.status, 1, result.stdout + result.stderr);
    assert.match(result.stderr, /INVALID_DESKTOP_WORKFLOW_STATE/);
  });
}

for (const [name, overrides] of [
  ['API error', { TEST_API_ERROR: '1' }],
  ['missing token', { GH_TOKEN: '' }],
  ['missing repository', { GITHUB_REPOSITORY: '' }],
  ['invalid repository', { GITHUB_REPOSITORY: '../other/repo' }],
]) {
  test(`fails closed for ${name}`, t => {
    const result = run(t, JSON.stringify({ path, state: 'active' }), overrides);
    assert.equal(result.status, 1, result.stdout + result.stderr);
    assert.match(result.stderr, /INVALID_DESKTOP_WORKFLOW_STATE/);
    assert.doesNotMatch(result.stdout + result.stderr, /fixture-token/);
  });
}
