import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { copyFileSync, cpSync, mkdirSync, mkdtempSync, readFileSync, renameSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import process from 'node:process';
import { fileURLToPath, URL } from 'node:url';
import test from 'node:test';
import YAML from 'yaml';

const cli = fileURLToPath(new URL('./check-ci-tool-pins.mjs', import.meta.url));
const action = 'taiki-e/install-action@ab6895351a88b908e96a4697860d4ffea5e57dbe';
const workflow = step => `jobs:\n  probe:\n    steps:\n${step}\n`;
const installer = input => workflow(`      - uses: ${action}\n        with:\n          tool: ${input}`);
const summary = (declarations, tools, errors) => `install_declarations=${declarations} tool_specs=${tools} errors=${errors}\n`;

function run(t, source, args = [], filename = 'fixture.yaml') {
  const directory = mkdtempSync(join(tmpdir(), 'ci-tool-pins-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  if (source !== undefined && !/^desktop-contracts\.ya?ml$/i.test(filename)) {
    writeFileSync(join(directory, 'desktop-contracts.yml'), desktop());
  }
  const file = join(directory, filename);
  if (source !== undefined) writeFileSync(file, source);
  const result = spawnSync(process.execPath, [cli, directory, ...args], { encoding: 'utf8' });
  assert.equal(result.error, undefined);
  return { ...result, file, directory };
}

const setupNode = 'actions/setup-node@949feb2413d6458794dcd2491c4babbbce0c15c1';
const matrixInput = '${{ matrix.node }}';
const desktop = (matrix = { node: ['22.23.3', '24.21.0'] }, steps = [
  { uses: setupNode, with: { 'node-version': matrixInput } },
]) => JSON.stringify({ jobs: { parity: { strategy: { matrix }, steps } } });

for (const [name, source] of [
  ['exact patches', desktop()],
  ['future patch bump', desktop({ node: ['22.23.4', '24.21.1'] })],
  ['compact expression', desktop(undefined, [{ uses: setupNode, with: { 'node-version': '${{matrix.node}}' } }])],
  ['anchored matrix', `nodes: &nodes ['22.23.3', '24.21.0']\njobs:\n  parity:\n    strategy:\n      matrix: {node: *nodes}\n    steps:\n      - uses: ${setupNode}\n        with: {node-version: '${matrixInput}'}`],
]) {
  test(`accept desktop Node ${name}`, t => {
    const result = run(t, source, [], 'desktop-contracts.yml');
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stderr, '');
    assert.equal(result.stdout, summary(0, 0, 0));
  });
}

for (const value of ['22', '24', '22.23', '22.x', 'lts/*', 'latest', 'node', '^22.23.3', '>=22', 'v22.23.3', '22.23.3-rc.1', '22.23.3+build', '022.23.3', '', '${{ inputs.node }}', 22, 22.23, null, {}]) {
  test(`reject desktop Node version ${JSON.stringify(value)}`, t => {
    const result = run(t, desktop({ node: [value, '24.21.0'] }), [], 'desktop-contracts.yml');
    assert.equal(result.status, 1, result.stdout);
    assert.equal(result.stderr, `${result.file} jobs.parity.strategy.matrix.node[0]: UNPINNED_NODE: ${JSON.stringify(value)}; expected major.minor.patch string\n`);
    assert.equal(result.stdout, summary(0, 0, 1));
  });
}

for (const [name, source, diagnostic] of [
  ['empty matrix', desktop({ node: [] }), 'INVALID_NODE_MATRIX'],
  ['scalar node', desktop({ node: '22.23.3' }), 'INVALID_NODE_MATRIX'],
  ['missing node', desktop({}), 'INVALID_NODE_MATRIX'],
  ['dynamic matrix', desktop('${{ fromJSON(inputs.matrix) }}'), 'INVALID_NODE_MATRIX'],
  ['duplicate version', desktop({ node: ['22.23.3', '22.23.3'] }), 'INVALID_NODE_MATRIX'],
  ['include override', desktop({ node: ['22.23.3'], include: [{ node: '24' }] }), 'INVALID_NODE_MATRIX'],
  ['exclude override', desktop({ node: ['22.23.3'], exclude: [] }), 'INVALID_NODE_MATRIX'],
  ['extra axis', desktop({ node: ['22.23.3'], os: ['ubuntu-latest'] }), 'INVALID_NODE_MATRIX'],
  ['missing parity', 'jobs: {other: {steps: []}}', 'INVALID_NODE_MATRIX'],
  ['missing strategy', `jobs: {parity: {steps: [{uses: '${setupNode}'}]}}`, 'INVALID_NODE_MATRIX'],
  ['missing setup', desktop(undefined, []), 'INVALID_NODE_SETUP'],
  ['duplicate setup', desktop(undefined, [{ uses: setupNode }, { uses: setupNode }]), 'INVALID_NODE_SETUP'],
  ...['24', '${{ matrix.other }}', '${{ inputs.node }}', null].map(value => [
    `bypassed matrix ${value}`, desktop(undefined, [{ uses: setupNode, with: { 'node-version': value } }]), 'INVALID_NODE_SETUP',
  ]),
  ['version file', desktop(undefined, [{ uses: setupNode, with: { 'node-version': matrixInput, 'node-version-file': '.nvmrc' } }]), 'INVALID_NODE_SETUP'],
  ['missing inputs', desktop(undefined, [{ uses: setupNode }]), 'INVALID_NODE_SETUP'],
  ['anchored floating matrix', `nodes: &nodes ['22', '24.21.0']\njobs: {parity: {strategy: {matrix: {node: *nodes}}, steps: [{uses: '${setupNode}', with: {node-version: '${matrixInput}'}}]}}`, 'UNPINNED_NODE'],
]) {
  test(`reject desktop Node ${name}`, t => {
    const result = run(t, source, [], 'desktop-contracts.yml');
    assert.equal(result.status, 1, result.stdout);
    assert.match(result.stderr, new RegExp(`: ${diagnostic}:`));
  });
}

test('Node policy leaves unrelated workflows unchanged', t => {
  const result = run(t, desktop({ node: ['22', '24'] }));
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stdout, summary(0, 0, 0));
});

for (const filename of ['desktop-contracts.yaml']) {
  test(`reject floating Node pins after rename to ${filename}`, t => {
    const result = run(t, desktop({ node: ['22', '24'] }), [], filename);
    assert.equal(result.status, 1, result.stdout);
    assert.equal(result.stdout, summary(0, 0, 2));
    assert.match(result.stderr, /UNPINNED_NODE: "22"/);
    assert.match(result.stderr, /UNPINNED_NODE: "24"/);
  });

  test(`accept exact Node pins after rename to ${filename}`, t => {
    const result = run(t, desktop(), [], filename);
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stdout, summary(0, 0, 0));
    assert.equal(result.stderr, '');
  });
}

for (const replacement of [null, 'renamed-contracts.yml', 'desktop-contracts.yml.disabled', 'desktop-contracts.YML', 'DESKTOP-CONTRACTS.YAML']) {
  test(`reject repository workflows after desktop contracts ${replacement ?? 'removal'}`, t => {
    const directory = mkdtempSync(join(tmpdir(), 'ci-workflow-presence-'));
    t.after(() => rmSync(directory, { recursive: true, force: true }));
    cpSync(fileURLToPath(new URL('../../.github/workflows', import.meta.url)), directory, { recursive: true });
    const original = join(directory, 'desktop-contracts.yml');
    if (replacement === null) rmSync(original);
    else renameSync(original, join(directory, replacement));
    const result = spawnSync(process.execPath, [cli, directory], { encoding: 'utf8' });
    assert.equal(result.error, undefined);
    assert.equal(result.status, 1, result.stdout);
    assert.equal(result.stderr, `${directory} $: MISSING_DESKTOP_CONTRACTS: expected one desktop-contracts.yml or desktop-contracts.yaml workflow\n`);
    assert.match(result.stdout, / errors=1\n$/);
  });
}

test('reject symlink replacing desktop contracts', t => {
  const directory = mkdtempSync(join(tmpdir(), 'ci-workflow-symlink-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const workflows = join(directory, 'workflows');
  mkdirSync(workflows);
  writeFileSync(join(workflows, 'unrelated.yml'), workflow('      - run: echo ok'));
  writeFileSync(join(directory, 'contracts.yml'), desktop());
  symlinkSync('../contracts.yml', join(workflows, 'desktop-contracts.yml'));
  const result = spawnSync(process.execPath, [cli, workflows], { encoding: 'utf8' });
  assert.equal(result.error, undefined);
  assert.equal(result.status, 1, result.stdout);
  assert.equal(result.stderr, `${workflows} $: MISSING_DESKTOP_CONTRACTS: expected one desktop-contracts.yml or desktop-contracts.yaml workflow\n`);
  assert.equal(result.stdout, summary(0, 0, 1));
});

for (const tracked of [false, true, 'symlink']) {
  test(`${tracked === true ? 'accept tracked' : `reject ${tracked === false ? 'untracked' : 'tracked symlink'}`} desktop contracts in default repository scan`, t => {
    const directory = mkdtempSync(join(tmpdir(), 'ci-workflow-tracked-'));
    t.after(() => rmSync(directory, { recursive: true, force: true }));
    mkdirSync(join(directory, 'web/scripts'), { recursive: true });
    mkdirSync(join(directory, '.github/workflows'), { recursive: true });
    copyFileSync(cli, join(directory, 'web/scripts/check-ci-tool-pins.mjs'));
    symlinkSync(fileURLToPath(new URL('../node_modules', import.meta.url)), join(directory, 'web/node_modules'));
    writeFileSync(join(directory, '.github/workflows/web-ci.yml'), JSON.stringify(webCI));
    for (const name of ['integration-smoke', 'pr-evidence', 'desktop-e2e', 'desktop-webkit', 'desktop-runtime', 'desktop-native-macos']) {
      copyFileSync(fileURLToPath(new URL(`../../.github/workflows/${name}.yml`, import.meta.url)), join(directory, `.github/workflows/${name}.yml`));
    }
    const contracts = join(directory, '.github/workflows/desktop-contracts.yml');
    if (tracked === 'symlink') symlinkSync('../../contracts.yml', contracts);
    else writeFileSync(contracts, desktop());
    for (const args of [
      ['init', '-q'],
      ['add', '.github/workflows/web-ci.yml', '.github/workflows/integration-smoke.yml', '.github/workflows/pr-evidence.yml', '.github/workflows/desktop-e2e.yml', '.github/workflows/desktop-webkit.yml', '.github/workflows/desktop-runtime.yml', '.github/workflows/desktop-native-macos.yml'],
      ['add', tracked ? '.github/workflows/desktop-contracts.yml' : 'web/scripts/check-ci-tool-pins.mjs'],
      ['-c', 'user.name=CI test', '-c', 'user.email=ci@example.invalid', 'commit', '-qm', 'fixture'],
    ]) {
      const git = spawnSync('git', args, { cwd: directory, encoding: 'utf8' });
      assert.equal(git.status, 0, git.stderr);
    }
    if (tracked === 'symlink') {
      rmSync(contracts);
      writeFileSync(contracts, desktop());
    }
    const result = spawnSync(process.execPath, [join(directory, 'web/scripts/check-ci-tool-pins.mjs')], { encoding: 'utf8' });
    assert.equal(result.error, undefined);
    assert.equal(result.status, tracked === true ? 0 : 1, result.stdout);
    assert.equal(result.stderr, tracked === true ? '' : `${join(directory, '.github/workflows')} $: UNTRACKED_DESKTOP_CONTRACTS: expected exactly one regular desktop contracts workflow in HEAD\n`);
    assert.equal(result.stdout, summary(1, 1, tracked === true ? 0 : 1));
  });
}

test('reject multiple desktop contracts filename variants', t => {
  const directory = mkdtempSync(join(tmpdir(), 'ci-workflow-duplicates-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  writeFileSync(join(directory, 'desktop-contracts.yml'), desktop());
  writeFileSync(join(directory, 'desktop-contracts.yaml'), desktop());
  const result = spawnSync(process.execPath, [cli, directory], { encoding: 'utf8' });
  assert.equal(result.error, undefined);
  assert.equal(result.status, 1, result.stdout);
  assert.equal(result.stderr, `${directory} $: DUPLICATE_DESKTOP_CONTRACTS: expected one desktop contracts workflow; found desktop-contracts.yaml, desktop-contracts.yml\n`);
  assert.equal(result.stdout, summary(0, 0, 1));
});

test('reject unrelated workflow without desktop contracts', t => {
  const result = run(t);
  writeFileSync(join(result.directory, 'unrelated.yml'), workflow('      - run: echo ok'));
  const missing = spawnSync(process.execPath, [cli, result.directory], { encoding: 'utf8' });
  assert.equal(missing.error, undefined);
  assert.equal(missing.status, 1, missing.stdout);
  assert.equal(missing.stderr, `${result.directory} $: MISSING_DESKTOP_CONTRACTS: expected one desktop-contracts.yml or desktop-contracts.yaml workflow\n`);
  assert.equal(missing.stdout, summary(0, 0, 1));
});

for (const [name, source, declarations, tools] of [
  ['current releases', installer('nextest@0.9.148, sccache@0.18.0 ast-grep@0.50.0 cargo-machete@0.9.2'), 1, 4],
  ['quoted flow keys and reordered inputs', `"jobs": {probe: {"steps": [{with: {"tool": "nextest@0.9.148"}, "uses": '${action}'}]}}`, 1, 1],
  ['folded whitespace list', installer('>\n            nextest@0.9.148\n            sccache@0.18.0'), 1, 2],
  ['CRLF and no trailing newline', installer("'nextest@0.9.148' # exact release").trimEnd().replaceAll('\n', '\r\n'), 1, 1],
  ['complete steps anchor', `jobs:\n  first:\n    steps: &steps\n      - uses: ${action}\n        with: &inputs\n          tool: &tool nextest@0.9.148\n  second:\n    steps: *steps\n  third:\n    steps:\n      - uses: ${action}\n        with: *inputs\n      - uses: ${action}\n        with: {tool: *tool}\n`, 3, 3],
  ['unrelated tool inputs and shell text', workflow(`      - uses: other/action@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n        with: {tool: nextest}\n      - run: |\n          echo 'uses: ${action}'\n          echo 'tool: nextest'`), 0, 0],
  ['reusable workflow job', 'jobs: {probe: {uses: owner/repo/.github/workflows/reuse.yml@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa}}', 0, 0],
]) {
  test(name, t => {
    const result = run(t, source);
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stderr, '');
    assert.equal(result.stdout, summary(declarations, tools, 0));
  });
}

for (const token of ['nextest', 'sccache', 'nextest@latest', 'nextest@main', 'nextest@0.9', 'nextest@^0.9.148', 'nextest@>=0.9.148', 'nextest@0.9.148-beta', 'nextest@0.9.148+build', 'nextest@00.9.148']) {
  test(`reject floating release ${token}`, t => {
    const result = run(t, installer(JSON.stringify(token)));
    assert.equal(result.status, 1);
    assert.equal(result.stdout, summary(1, 0, 1));
    assert.equal(result.stderr, `${result.file} jobs.probe.steps[0].with.tool: UNPINNED_TOOL: ${JSON.stringify(token)}; expected name@major.minor.patch\n`);
  });
}

for (const [name, source, diagnostic, path = 'jobs.probe.steps[0].with.tool'] of [
  ['dynamic tool', installer("'nextest@${{ matrix.version }}'"), 'INVALID_TOOL'],
  ['dynamic action', workflow("      - uses: '${{ matrix.action }}'"), 'INVALID_USES', 'jobs.probe.steps[0].uses'],
  ['missing with', workflow(`      - uses: ${action}`), 'INVALID_TOOL'],
  ['missing tool', workflow(`      - uses: ${action}\n        with: {args: test}`), 'INVALID_TOOL'],
  ['empty tool', installer("''"), 'INVALID_TOOL'],
  ['null tool', installer('null'), 'INVALID_TOOL'],
  ['numeric tool', installer('123'), 'INVALID_TOOL'],
  ['sequence tool', installer('[nextest@0.9.148]'), 'INVALID_TOOL'],
  ['mapping tool', installer('{nextest: 0.9.148}'), 'INVALID_TOOL'],
  ['mixed comma list', installer('nextest@0.9.148, sccache'), 'UNPINNED_TOOL'],
  ['mixed whitespace list', installer('nextest@0.9.148 sccache'), 'UNPINNED_TOOL'],
  ['empty comma entry', installer('nextest@0.9.148,'), 'UNPINNED_TOOL'],
  ['duplicate tool names', installer('nextest@0.9.148 nextest@0.9.147'), 'DUPLICATE_TOOL'],
  ['flow floating input', `jobs: {probe: {steps: [{uses: '${action}', with: {'tool': nextest}}]}}`, 'UNPINNED_TOOL'],
  ['anchored floating inputs', `inputs: &inputs {tool: nextest}\njobs: {probe: {steps: [{uses: '${action}', with: *inputs}]}}`, 'UNPINNED_TOOL'],
  ['anchored floating steps', `jobs: {probe: {steps: &steps [{uses: '${action}', with: {tool: nextest}}]}, second: {steps: *steps}}`, 'UNPINNED_TOOL'],
  ['anchored floating scalar', `tool: &tool sccache\njobs: {probe: {steps: [{uses: '${action}', with: {tool: *tool}}]}}`, 'UNPINNED_TOOL'],
  ['adjacent unrelated tool', workflow(`      - uses: ${action}\n      - uses: other/action@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n        with: {tool: nextest@0.9.148}`), 'INVALID_TOOL'],
  ['sub-action', workflow(`      - uses: taiki-e/install-action/nextest@v2`), 'INVALID_INSTALLER', 'jobs.probe.steps[0].uses'],
  ['missing installer ref', workflow('      - uses: taiki-e/install-action'), 'INVALID_INSTALLER', 'jobs.probe.steps[0].uses'],
  ['duplicate tool key', installer('nextest@0.9.148\n          tool: nextest'), 'INVALID_YAML', '$'],
  ['duplicate uses key', workflow(`      - uses: ${action}\n        uses: other/action@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa`), 'INVALID_YAML', '$'],
  ['duplicate with key', workflow(`      - uses: ${action}\n        with: {tool: nextest}\n        with: {tool: nextest@0.9.148}`), 'INVALID_YAML', '$'],
  ['duplicate steps key', 'jobs: {probe: {steps: [], steps: []}}', 'INVALID_YAML', '$'],
  ['duplicate jobs key', 'jobs: {}\njobs: {}', 'INVALID_YAML', '$'],
  ['malformed YAML', 'jobs: [', 'INVALID_YAML', '$'],
  ['multiple documents', 'jobs: {}\n---\njobs: {}', 'INVALID_YAML', '$'],
  ['merge key', `inputs: &inputs {tool: nextest}\njobs: {probe: {steps: [{uses: '${action}', with: {<<: *inputs}}]}}`, 'UNSUPPORTED_YAML', '$'],
  ['custom tag', installer('!unknown nextest'), 'INVALID_YAML', '$'],
  ['non-string mapping key', 'jobs: {123: {steps: []}}', 'UNSUPPORTED_YAML', '$'],
  ['unresolved alias', 'jobs: *missing', 'READ_OR_PARSE_ERROR', '$'],
  ['alias expansion limit', `step: &step {uses: '${action}', with: {tool: nextest@0.9.148}}\njobs: {probe: {steps: [${Array(101).fill('*step').join(', ')}]}}`, 'READ_OR_PARSE_ERROR', '$'],
  ['null jobs', 'jobs: null', 'INVALID_SHAPE', 'jobs'],
  ['sequence jobs', 'jobs: []', 'INVALID_SHAPE', 'jobs'],
  ['invalid job', 'jobs: {probe: null}', 'INVALID_SHAPE', 'jobs.probe'],
  ['dynamic steps', "jobs: {probe: {steps: '${{ matrix.steps }}'}}", 'INVALID_SHAPE', 'jobs.probe.steps'],
  ['mapping steps', `jobs: {probe: {steps: {uses: '${action}', with: {tool: nextest}}}}`, 'INVALID_SHAPE', 'jobs.probe.steps'],
  ['set-tagged step', `jobs: {probe: {steps: [!!set {uses: null, with: null}]}}`, 'INVALID_SHAPE', 'jobs.probe.steps[0]'],
  ['scalar step', 'jobs: {probe: {steps: [nextest]}}', 'INVALID_SHAPE', 'jobs.probe.steps[0]'],
  ['job uses and steps', 'jobs: {probe: {uses: owner/reuse@main, steps: []}}', 'INVALID_SHAPE', 'jobs.probe'],
]) {
  test(`reject ${name}`, t => {
    const result = run(t, source);
    assert.equal(result.status, 1, result.stdout);
    assert.ok(result.stderr.includes(`${result.file} ${path}: ${diagnostic}:`), result.stderr);
    assert.match(result.stdout, / errors=[1-9]\d*\n$/);
  });
}

test('empty directory', t => {
  const result = run(t);
  assert.equal(result.status, 1);
  assert.equal(result.stdout, summary(0, 0, 2));
  assert.equal(result.stderr, `${result.directory} $: NO_WORKFLOWS: no .yml or .yaml workflow files found\n${result.directory} $: MISSING_DESKTOP_CONTRACTS: expected one desktop-contracts.yml or desktop-contracts.yaml workflow\n`);
});

test('missing directory', t => {
  const result = run(t);
  rmSync(result.directory, { recursive: true });
  const missing = spawnSync(process.execPath, [cli, result.directory], { encoding: 'utf8' });
  assert.equal(missing.status, 1);
  assert.match(missing.stderr, /READ_ERROR: ENOENT/);
  assert.equal(missing.stdout, summary(0, 0, 1));
});

test('extra arguments fail usage', t => {
  const result = run(t, installer('nextest'), ['extra']);
  assert.equal(result.status, 2);
  assert.equal(result.stdout, '');
  assert.equal(result.stderr, 'Usage: node web/scripts/check-ci-tool-pins.mjs [workflow-directory | --event-sha <commit-sha>]\n');
});

test('missing parser gives setup instruction', t => {
  const directory = mkdtempSync(join(tmpdir(), 'ci-tool-parser-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  mkdirSync(join(directory, 'web/scripts'), { recursive: true });
  const isolated = join(directory, 'web/scripts/check-ci-tool-pins.mjs');
  copyFileSync(cli, isolated);
  const result = spawnSync(process.execPath, [isolated], { encoding: 'utf8', env: { ...process.env, NODE_PATH: '' } });
  assert.equal(result.status, 2);
  assert.equal(result.stdout, '');
  assert.equal(result.stderr, 'MISSING_PARSER: run npm ci --ignore-scripts in web/ before checking CI tool pins\n');
});

test('mixed list reports only the floating token', t => {
  const result = run(t, installer('nextest@0.9.148, sccache'));
  assert.equal(result.status, 1);
  assert.equal(result.stdout, summary(1, 1, 1));
  assert.equal(result.stderr, `${result.file} jobs.probe.steps[0].with.tool: UNPINNED_TOOL: "sccache"; expected name@major.minor.patch\n`);
});

const checkout = 'actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1';

for (const [name, source] of [
  ['quoted uses key', workflow(`      - "uses": actions/checkout@v7`)],
  ['flow step', 'jobs: {probe: {steps: [{uses: actions/checkout@v7}]}}'],
  ['anchored step', 'step: &checkout {uses: actions/checkout@v7}\njobs: {probe: {steps: [*checkout]}}'],
  ['anchored scalar', 'ref: &ref actions/checkout@v7\njobs: {probe: {steps: [{uses: *ref}]}}'],
  ['anchored steps', 'jobs: {probe: {steps: &steps [{uses: actions/checkout@v7}]}, second: {steps: *steps}}'],
  ['reusable workflow', 'jobs: {probe: {uses: owner/repo/.github/workflows/ci.yml@main}}'],
  ['sub-action tag', workflow('      - uses: owner/repo/path@v1')],
  ['short SHA', workflow('      - uses: actions/checkout@3d3c42e')],
  ['missing ref', workflow('      - uses: actions/checkout')],
  ['extra ref separator', workflow(`      - uses: owner/repo@tag@${'a'.repeat(40)}`)],
  ['ref suffix', workflow(`      - uses: ${checkout}suffix`)],
  ['image tag', workflow('      - uses: docker://alpine:3.22')],
  ['Docker Git-style SHA', workflow(`      - uses: docker://alpine@${'a'.repeat(40)}`)],
  ['short Docker digest', workflow(`      - uses: docker://alpine@sha256:${'a'.repeat(63)}`)],
  ['nonhex Docker digest', workflow(`      - uses: docker://alpine@sha256:${'g'.repeat(64)}`)],
  ['long Docker digest', workflow(`      - uses: docker://alpine@sha256:${'a'.repeat(65)}`)],
  ['missing Docker image', workflow(`      - uses: docker://@sha256:${'a'.repeat(64)}`)],
  ['bare SHA', workflow(`      - uses: ${'a'.repeat(40)}`)],
]) {
  test(`reject unpinned action in ${name}`, t => {
    const result = run(t, source);
    assert.equal(result.status, 1, result.stdout);
    assert.match(result.stderr, /UNPINNED_ACTION:/);
    assert.match(result.stdout, / errors=[1-9]\d*\n$/);
  });
}

for (const [name, source] of [
  ['quoted uses key', workflow(`      - "uses": '${checkout}'`)],
  ['flow step', `jobs: {probe: {steps: [{uses: '${checkout}'}]}}`],
  ['anchored step', `step: &checkout {uses: '${checkout}'}\njobs: {probe: {steps: [*checkout]}}`],
  ['reusable workflow', `jobs: {probe: {uses: owner/repo/.github/workflows/ci.yml@${'a'.repeat(40)}}}`],
  ['sub-action SHA', workflow(`      - uses: owner/repo/path@${'A'.repeat(40)}`)],
  ['Docker digest', workflow(`      - uses: docker://alpine@sha256:${'a'.repeat(64)}`)],
  ['tagged image with digest', workflow(`      - uses: docker://ghcr.io/owner/image:tag@sha256:${'a'.repeat(64)}`)],
  ['local action', workflow('      - uses: ./local/action')],
  ['local reusable workflow', 'jobs: {probe: {uses: ./.github/workflows/ci.yml}}'],
  ['shell example', workflow(`      - run: |\n          echo 'uses: actions/checkout@v7'`)],
]) {
  test(`accept immutable or local action in ${name}`, t => {
    const result = run(t, source);
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stderr, '');
    assert.equal(result.stdout, summary(0, 0, 0));
  });
}

const webCI = { jobs: Object.fromEntries(['web', 'web-build'].map(name => [name, {
  steps: [
    { uses: setupNode, with: { 'node-version': '22.23.3' } },
    { run: 'test "$(node --version)" = "v22.23.3"' },
  ],
}])) };

for (const filename of ['web-ci.yml', 'web-ci.yaml']) {
  test(`accept exact matching Web CI pins in ${filename}`, t => {
    const result = run(t, JSON.stringify(webCI), [], filename);
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stdout, summary(0, 0, 0));
  });
}

for (const name of ['web', 'web-build']) {
  for (const version of ['22', '22.23', '22.x', 'lts/*', '^22.23.3', 'v22.23.3', '22.23.3-rc.1', '022.23.3', '24.21.0', '1.0.0', '${{ inputs.node }}', 22, null]) {
    test(`reject Web CI ${name} Node ${JSON.stringify(version)}`, t => {
      const source = JSON.parse(JSON.stringify(webCI));
      source.jobs[name].steps[0].with['node-version'] = version;
      const result = run(t, JSON.stringify(source), [], 'web-ci.yml');
      assert.equal(result.status, 1, result.stdout);
      assert.match(result.stderr, new RegExp(`jobs.${name}: UNPINNED_WEB_NODE:`));
    });
  }
  for (const [label, mutate, diagnostic] of [
    ['missing lane', source => delete source.jobs[name], 'INVALID_WEB_NODE_SETUP'],
    ['conditional lane', source => { source.jobs[name].if = false; }, 'INVALID_WEB_NODE_SETUP'],
    ['tolerated lane failure', source => { source.jobs[name]['continue-on-error'] = true; }, 'INVALID_WEB_NODE_SETUP'],
    ['dependent lane', source => { source.jobs[name].needs = ['skipped']; }, 'INVALID_WEB_NODE_SETUP'],
    ['matrix override', source => { source.jobs[name].strategy = { matrix: { node: ['22'] } }; }, 'INVALID_WEB_NODE_SETUP'],
    ['duplicate setup', source => source.jobs[name].steps.push(source.jobs[name].steps[0]), 'INVALID_WEB_NODE_SETUP'],
    ['missing setup', source => source.jobs[name].steps.shift(), 'INVALID_WEB_NODE_SETUP'],
    ['version file', source => { source.jobs[name].steps[0].with['node-version-file'] = '.nvmrc'; }, 'UNPINNED_WEB_NODE'],
    ['conditional setup', source => { source.jobs[name].steps[0].if = 'false'; }, 'UNPINNED_WEB_NODE'],
    ['tolerated setup failure', source => { source.jobs[name].steps[0]['continue-on-error'] = true; }, 'UNPINNED_WEB_NODE'],
    ['missing runtime check', source => source.jobs[name].steps.pop(), 'MISSING_WEB_NODE_VERIFICATION'],
    ['mismatched runtime check', source => { source.jobs[name].steps[1].run = 'test "$(node --version)" = "v24.21.0"'; }, 'MISSING_WEB_NODE_VERIFICATION'],
    ['conditional runtime check', source => { source.jobs[name].steps[1].if = 'false'; }, 'MISSING_WEB_NODE_VERIFICATION'],
    ['tolerated runtime failure', source => { source.jobs[name].steps[1]['continue-on-error'] = true; }, 'MISSING_WEB_NODE_VERIFICATION'],
    ['install before setup', source => source.jobs[name].steps.unshift({ run: 'npm ci --ignore-scripts' }), 'EARLY_WEB_NODE_COMMAND'],
    ['lifecycle before setup', source => source.jobs[name].steps.unshift({ run: 'npm rebuild' }), 'EARLY_WEB_NODE_COMMAND'],
    ['node before setup', source => source.jobs[name].steps.unshift({ run: 'node scripts/probe.mjs' }), 'EARLY_WEB_NODE_COMMAND'],
    ['late runtime check', source => source.jobs[name].steps.splice(1, 0, { run: 'npm ci' }), 'MISSING_WEB_NODE_VERIFICATION'],
  ]) {
    test(`reject Web CI ${name} ${label}`, t => {
      const source = JSON.parse(JSON.stringify(webCI));
      mutate(source);
      const result = run(t, JSON.stringify(source), [], 'web-ci.yml');
      assert.equal(result.status, 1, result.stdout);
      assert.match(result.stderr, new RegExp(`: ${diagnostic}:`));
    });
  }
}

for (const name of ['web', 'web-build']) {
  test(`accept Web CI ${name} with exact changes dependency and selection`, t => {
    const source = structuredClone(webCI);
    source.jobs[name].needs = ['changes'];
    source.jobs[name].if = "(github.event_name != 'pull_request' || !github.event.pull_request.draft) && (!cancelled() && (needs.changes.result != 'success' || needs.changes.outputs.web != 'false'))";
    const result = run(t, JSON.stringify(source), [], 'web-ci.yml');
    assert.equal(result.status, 0, result.stderr);
  });
  for (const [label, needs, condition] of [
    ['missing dependency', undefined, "web"],
    ['extra dependency', ['changes', 'skipped'], "web"],
    ['wrong output', ['changes'], "ui"],
  ]) {
    test(`reject Web CI ${name} selection with ${label}`, t => {
      const source = structuredClone(webCI);
      if (needs) source.jobs[name].needs = needs;
      source.jobs[name].if = `(github.event_name != 'pull_request' || !github.event.pull_request.draft) && (!cancelled() && (needs.changes.result != 'success' || needs.changes.outputs.${condition} != 'false'))`;
      const result = run(t, JSON.stringify(source), [], 'web-ci.yml');
      assert.equal(result.status, 1, result.stdout);
      assert.match(result.stderr, /INVALID_WEB_NODE_SETUP/);
    });
  }
}

test('reject Web CI lane patch mismatch despite matching runtime checks', t => {
  const source = JSON.parse(JSON.stringify(webCI));
  source.jobs['web-build'].steps[0].with['node-version'] = '22.23.4';
  source.jobs['web-build'].steps[1].run = 'test "$(node --version)" = "v22.23.4"';
  const result = run(t, JSON.stringify(source), [], 'web-ci.yml');
  assert.equal(result.status, 1, result.stdout);
  assert.match(result.stderr, /MISMATCHED_WEB_NODE: expected 22.23.3, received 22.23.4/);
});

test('accept coordinated future Web CI patch bump', t => {
  const result = run(t, JSON.stringify(webCI).replaceAll('22.23.3', '22.23.4'), [], 'web-ci.yml');
  assert.equal(result.status, 0, result.stderr);
});

test('repository Web CI has exact matching Node pins and runtime checks', t => {
  const source = readFileSync(fileURLToPath(new URL('../../.github/workflows/web-ci.yml', import.meta.url)), 'utf8');
  const result = run(t, source, [], 'web-ci.yml');
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stdout, summary(0, 0, 0));
});

for (const [filename, name, code, optional, major = 22] of [
  ['integration-smoke.yml', 'postgres', 'INTEGRATION', true],
  ['pr-evidence.yml', 'lint-pr-body', 'EVIDENCE', false],
  ['desktop-e2e.yml', 'linux-smoke', 'E2E', false],
  ['desktop-webkit.yml', 'webkit-stl', 'WEBKIT', false, 24],
]) {
  const source = () => ({ jobs: { [name]: {
    ...(optional ? { if: "github.event_name != 'workflow_dispatch' || inputs.postgres" }
      : ['E2E', 'WEBKIT'].includes(code) ? { if: "github.event_name != 'pull_request' || contains(github.event.pull_request.labels.*.name, 'ci:desktop-smoke')" } : {}),
    steps: JSON.parse(JSON.stringify(webCI.jobs.web.steps).replaceAll('22.23.3', `${major}.23.3`)),
  } } });
  for (const extension of ['yml', 'yaml', 'YML']) {
    test(`accept exact Node pin in ${filename} as ${extension}`, t => {
      const result = run(t, JSON.stringify(source()), [], filename.replace('yml', extension));
      assert.equal(result.status, 0, result.stderr);
      assert.equal(result.stdout, summary(0, 0, 0));
    });
  }
  test(`accept draft guard in ${filename}`, t => {
    const data = source();
    const guard = "github.event_name != 'pull_request' || !github.event.pull_request.draft";
    data.jobs[name].if = data.jobs[name].if ? `(${guard}) && (${data.jobs[name].if})` : guard;
    const result = run(t, JSON.stringify(data), [], filename);
    assert.equal(result.status, 0, result.stderr);
  });
  for (const version of ['22', '22.23', '22.x', 'lts/*', '^22.23.3', 'v22.23.3', '22.23.3-rc.1', '022.23.3', ...(major === 22 ? ['24.21.0'] : ['22.23.3', '24', '24.21', '24.x', '024.21.0']), '${{ inputs.node }}', 22, null]) {
    test(`reject ${filename} Node ${JSON.stringify(version)}`, t => {
      const data = source();
      data.jobs[name].steps[0].with['node-version'] = version;
      const result = run(t, JSON.stringify(data), [], filename);
      assert.equal(result.status, 1, result.stdout);
      assert.match(result.stderr, new RegExp(`UNPINNED_${code}_NODE:`));
    });
  }
  for (const [label, mutate, diagnostic] of [
    ['missing lane', data => delete data.jobs[name], 'INVALID_SHAPE'],
    ['renamed lane', data => { data.jobs.other = data.jobs[name]; delete data.jobs[name]; }, `INVALID_${code}_NODE_SETUP`],
    ['matrix override', data => { data.jobs[name].strategy = { matrix: { node: ['22'] } }; }, `INVALID_${code}_NODE_SETUP`],
    ['tolerated lane', data => { data.jobs[name]['continue-on-error'] = true; }, `INVALID_${code}_NODE_SETUP`],
    ...((optional || ['E2E', 'WEBKIT'].includes(code)) ? [false, true, 'false', '${{ false }}', 'inputs.postgres', undefined].map(condition => [
      `unexpected smoke condition ${condition}`, data => {
        if (condition === undefined) delete data.jobs[name].if;
        else data.jobs[name].if = condition;
      }, `INVALID_${code}_NODE_SETUP`,
    ]) : []),
    ['dependent lane', data => { data.jobs[name].needs = ['skipped']; }, `INVALID_${code}_NODE_SETUP`],
    ...(!optional ? [['conditional lane', data => { data.jobs[name].if = false; }, `INVALID_${code}_NODE_SETUP`]] : []),
    ['missing setup', data => data.jobs[name].steps.shift(), `INVALID_${code}_NODE_SETUP`],
    ['duplicate setup', data => data.jobs[name].steps.push(data.jobs[name].steps[0]), `INVALID_${code}_NODE_SETUP`],
    ['version file', data => { data.jobs[name].steps[0].with['node-version-file'] = '.nvmrc'; }, `UNPINNED_${code}_NODE`],
    ['conditional setup', data => { data.jobs[name].steps[0].if = false; }, `UNPINNED_${code}_NODE`],
    ['tolerated setup', data => { data.jobs[name].steps[0]['continue-on-error'] = true; }, `UNPINNED_${code}_NODE`],
    ['missing assertion', data => data.jobs[name].steps.pop(), `MISSING_${code}_NODE_VERIFICATION`],
    ['mismatched assertion', data => { data.jobs[name].steps[1].run = 'test "$(node --version)" = "v22.23.4"'; }, `MISSING_${code}_NODE_VERIFICATION`],
    ['conditional assertion', data => { data.jobs[name].steps[1].if = false; }, `MISSING_${code}_NODE_VERIFICATION`],
    ['tolerated assertion', data => { data.jobs[name].steps[1]['continue-on-error'] = true; }, `MISSING_${code}_NODE_VERIFICATION`],
    ['early install', data => data.jobs[name].steps.unshift({ run: 'npm ci' }), `EARLY_${code}_NODE_COMMAND`],
    ['late assertion', data => data.jobs[name].steps.splice(1, 0, { run: 'npm ci' }), `MISSING_${code}_NODE_VERIFICATION`],
  ]) {
    test(`reject ${filename} ${label}`, t => {
      const data = source();
      mutate(data);
      const result = run(t, JSON.stringify(data), [], filename);
      assert.equal(result.status, 1, result.stdout);
      assert.match(result.stderr, new RegExp(`${diagnostic}:`));
    });
  }
  test(`accept future ${filename} patch with matching assertion`, t => {
    const result = run(t, JSON.stringify(source()).replaceAll(`${major}.23.3`, `${major}.23.4`), [], filename);
    assert.equal(result.status, 0, result.stderr);
  });
  test(`repository ${filename} has exact Node pin and runtime assertion`, t => {
    const data = readFileSync(fileURLToPath(new URL(`../../.github/workflows/${filename}`, import.meta.url)), 'utf8');
    const result = run(t, data, [], filename);
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stdout, summary(0, 0, 0));
  });
}

for (const filename of ['desktop-runtime.yml', 'desktop-native-macos.yml']) {
  const source = readFileSync(fileURLToPath(new URL(`../../.github/workflows/${filename}`, import.meta.url)), 'utf8');
  test(`repository ${filename} has exact patches and immediate runtime assertions`, t => {
    const result = run(t, source, [], filename);
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stderr, '');
  });
  for (const [label, mutate, diagnostic] of [
    ['floating Node 24', text => text.replaceAll("node-version: '24.21.0'", "node-version: '24'"), 'UNPINNED_DESKTOP_NODE'],
    ['floating Node 22', text => text.replaceAll("node-version: '22.23.3'", "node-version: '22.x'"), 'UNPINNED_DESKTOP_NODE'],
    ['wrong major', text => text.replaceAll("node-version: '24.21.0'", "node-version: '22.23.3'"), 'UNPINNED_DESKTOP_NODE'],
    ['missing assertions', text => text.replaceAll('run: test "$(node --version)" = "v24.21.0"', 'run: echo unchecked'), 'MISSING_DESKTOP_NODE_VERIFICATION'],
    ['mismatched assertion', text => text.replace('run: test "$(node --version)" = "v24.21.0"', 'run: test "$(node --version)" = "v24.21.1"'), 'MISSING_DESKTOP_NODE_VERIFICATION'],
    ['version file', text => text.replace("node-version: '24.21.0'", "node-version: '24.21.0'\n          node-version-file: .nvmrc"), 'UNPINNED_DESKTOP_NODE'],
    ['tolerated setup', text => text.replace('        with:\n          node-version:', '        continue-on-error: true\n        with:\n          node-version:'), 'UNPINNED_DESKTOP_NODE'],
    ['missing setup', text => text.replace('uses: actions/setup-node@', 'uses: example/other@'), 'INVALID_DESKTOP_NODE_SETUP'],
    ['late assertion', text => text.replace('      - name: Verify Node runtime', '      - run: npm ci\n      - name: Verify Node runtime'), 'MISSING_DESKTOP_NODE_VERIFICATION'],
  ]) {
    test(`reject ${filename} ${label}`, t => {
      const result = run(t, mutate(source), [], filename);
      assert.equal(result.status, 1, result.stdout);
      assert.match(result.stderr, new RegExp(`${diagnostic}:`));
    });
  }
  test(`accept coordinated future ${filename} patch bump`, t => {
    const result = run(t, source.replaceAll('24.21.0', '24.21.1').replaceAll('22.23.3', '22.23.4'), [], filename);
    assert.equal(result.status, 0, result.stderr);
  });
  for (const extension of ['yaml', 'YML']) {
    test(`reject ${filename} floating pins after ${extension} rename`, t => {
      const result = run(t, source.replaceAll("node-version: '24.21.0'", "node-version: '24.x'"), [], filename.replace('yml', extension));
      assert.equal(result.status, 1, result.stdout);
      assert.match(result.stderr, /UNPINNED_DESKTOP_NODE:/);
    });
  }
}

for (const filename of ['desktop-runtime.yml', 'desktop-native-macos.yml']) {
  const original = YAML.parse(readFileSync(fileURLToPath(new URL(`../../.github/workflows/${filename}`, import.meta.url)), 'utf8'));
  for (const [lane, job] of Object.entries(original.jobs)) {
    for (const [index, step] of (job.steps ?? []).entries()) {
      if (!step.uses?.startsWith('actions/setup-node@')) continue;
      for (const [label, mutate, diagnostic] of [
        ['floating pin', steps => { steps[index].with['node-version'] = 'lts/*'; }, 'UNPINNED_DESKTOP_NODE'],
        ['missing assertion', steps => steps.splice(index + 1, 1), 'MISSING_DESKTOP_NODE_VERIFICATION'],
        ['tolerated assertion', steps => { steps[index + 1]['continue-on-error'] = true; }, 'MISSING_DESKTOP_NODE_VERIFICATION'],
        ['changed assertion condition', steps => { steps[index + 1].if = false; }, 'MISSING_DESKTOP_NODE_VERIFICATION'],
        ['changed setup condition', steps => { steps[index].if = false; }, 'UNPINNED_DESKTOP_NODE'],
      ]) {
        test(`reject ${filename} ${lane} setup ${index} ${label}`, t => {
          const data = JSON.parse(JSON.stringify(original));
          mutate(data.jobs[lane].steps);
          const result = run(t, JSON.stringify(data), [], filename);
          assert.equal(result.status, 1, result.stdout);
          assert.match(result.stderr, new RegExp(`${diagnostic}:`));
        });
      }
    }
  }
}

for (const [filename, lane] of [['desktop-runtime.yml', 'source-artifacts'], ['desktop-native-macos.yml', 'linux-precheck']]) {
  const original = YAML.parse(readFileSync(fileURLToPath(new URL(`../../.github/workflows/${filename}`, import.meta.url)), 'utf8'));
  for (const [label, mutate, diagnostic] of [
    ['missing lane', data => delete data.jobs[lane], 'INVALID_DESKTOP_NODE_SETUP'],
    ['duplicate setup', data => {
      const steps = data.jobs[lane].steps;
      steps.push(steps.find(step => step.uses?.startsWith('actions/setup-node@')));
    }, 'INVALID_DESKTOP_NODE_SETUP'],
    ['inconsistent patch', data => {
      const steps = data.jobs[lane].steps;
      const index = steps.findIndex(step => step.uses?.startsWith('actions/setup-node@'));
      steps[index].with['node-version'] = '24.21.1';
      steps[index + 1].run = 'test "$(node --version)" = "v24.21.1"';
    }, 'MISMATCHED_DESKTOP_NODE'],
  ]) {
    test(`reject ${filename} ${label}`, t => {
      const data = JSON.parse(JSON.stringify(original));
      mutate(data);
      const result = run(t, JSON.stringify(data), [], filename);
      assert.equal(result.status, 1, result.stdout);
      assert.match(result.stderr, new RegExp(`${diagnostic}:`));
    });
  }
}
