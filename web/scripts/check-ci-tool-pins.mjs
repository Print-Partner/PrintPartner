#!/usr/bin/env node
import console from 'node:console';
import { execFileSync } from 'node:child_process';
import { readdirSync, readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { basename, dirname, join, resolve } from 'node:path';
import process from 'node:process';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const eventMode = process.argv[2] === '--event-sha';
const eventSha = process.argv[3];
if (eventMode ? process.argv.length !== 4 : process.argv.length > 3) {
  console.error('Usage: node web/scripts/check-ci-tool-pins.mjs [workflow-directory | --event-sha <commit-sha>]');
  process.exit(2);
}
let yaml;
try {
  const require = createRequire(join(root, 'web/package.json'));
  yaml = require('yaml');
} catch {
  console.error('MISSING_PARSER: run npm ci --ignore-scripts in web/ before checking CI tool pins');
  process.exit(2);
}
const draftGuard = "github.event_name != 'pull_request' || !github.event.pull_request.draft";
const draftPrefix = `(${draftGuard}) && (`;
const desktopNodeLanes = [
  ['desktop-e2e', 'linux-smoke', 'E2E', 22],
  ['desktop-webkit', 'webkit-stl', 'WEBKIT', 24],
];
const desktopNodeSetups = {
  'desktop-runtime': {
    runtime: [22, 24],
    'source-artifacts': [24],
    'rust-verify': [24],
  },
  'desktop-native-macos': {
    'linux-precheck': [24],
    'server-smoke': [24],
    bundle: [24],
    'bundle-intel': [24],
    'macos-storage-gate': [22, 24],
  },
};
const additionalNodeWorkflows = ['integration-smoke', 'pr-evidence', ...desktopNodeLanes.map(([name]) => name), ...Object.keys(desktopNodeSetups)];
const desktopContracts = /^desktop-contracts\.ya?ml$/;
const directory = resolve(process.argv[2] ?? join(root, '.github/workflows'));
let declarations = 0;
let tools = 0;
let errors = 0;
const report = (file, path, code, message) => {
  errors++;
  console.error(`${file} ${path}: ${code}: ${message}`);
};
const mapping = value => value !== null && typeof value === 'object'
  && (Object.getPrototypeOf(value) === Object.prototype || Object.getPrototypeOf(value) === null);
const literal = value => typeof value === 'string' && value.trim() !== '' && !value.includes('${{');

function checkNodeWorkflowInventory(entries, file) {
  for (const name of additionalNodeWorkflows) {
    const matches = entries.filter(entry => new RegExp(`^${name}\\.ya?ml$`, 'i').test(entry.name));
    if (matches.length !== 1 || matches[0].name !== `${name}.yml` || !matches[0].regular) {
      report(file, name, 'INVALID_NODE_WORKFLOW', `expected exactly one regular canonical ${name}.yml workflow`);
    }
  }
}

function checkDesktopNode(workflow, file) {
  const job = workflow.jobs.parity;
  const matrix = job?.strategy?.matrix;
  const path = 'jobs.parity.strategy.matrix';
  if (!mapping(matrix) || Object.keys(matrix).length !== 1 || !Array.isArray(matrix.node)
    || matrix.node.length === 0 || new Set(matrix.node).size !== matrix.node.length) {
    report(file, path, 'INVALID_NODE_MATRIX', 'expected only a nonempty node sequence of distinct exact patch versions');
  } else {
    for (const [index, version] of matrix.node.entries()) {
      if (typeof version !== 'string' || !/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.test(version)) {
        report(file, `${path}.node[${index}]`, 'UNPINNED_NODE', `${JSON.stringify(version)}; expected major.minor.patch string`);
      }
    }
  }
  const setups = Array.isArray(job?.steps) ? job.steps.filter(step => mapping(step)
    && typeof step.uses === 'string' && step.uses.split('@')[0].toLowerCase() === 'actions/setup-node') : [];
  if (setups.length !== 1) {
    report(file, 'jobs.parity.steps', 'INVALID_NODE_SETUP', 'expected exactly one actions/setup-node step consuming matrix.node');
    return;
  }
  const inputs = setups[0].with;
  if (!mapping(inputs) || typeof inputs['node-version'] !== 'string'
    || !/^\$\{\{\s*matrix\.node\s*\}\}$/.test(inputs['node-version'])
    || Object.hasOwn(inputs, 'node-version-file')) {
    report(file, 'jobs.parity.steps', 'INVALID_NODE_SETUP', 'setup-node must use node-version: ${{ matrix.node }} without node-version-file');
  }
}

function checkNodeLanes(workflow, file, names, code = 'WEB', condition, major = 22) {
  let expected;
  for (const name of names) {
    const path = `jobs.${name}`;
    const job = workflow.jobs[name];
    const jobCondition = typeof job?.if === 'string' && job.if.startsWith(draftPrefix) && job.if.endsWith(')')
      ? job.if.slice(draftPrefix.length, -1) : job?.if;
    const hasCondition = Object.hasOwn(job ?? {}, 'if') && job.if !== draftGuard;
    const steps = Array.isArray(job?.steps) ? job.steps : [];
    const setups = steps.filter(step => mapping(step) && typeof step.uses === 'string'
      && step.uses.split('@')[0].toLowerCase() === 'actions/setup-node');
    if (!mapping(job) || ['strategy', 'needs', 'continue-on-error'].some(key => Object.hasOwn(job, key))
      || (condition === undefined ? hasCondition : jobCondition !== condition)
      || setups.length !== 1) {
      report(file, path, `INVALID_${code}_NODE_SETUP`, 'expected one setup-node step in each required lane with its permitted condition and without strategy or failure overrides');
      continue;
    }
    const setup = setups[0];
    const inputs = setup.with;
    const version = inputs?.['node-version'];
    if (!mapping(inputs) || typeof version !== 'string'
      || !new RegExp(`^${major}\\.(0|[1-9]\\d*)\\.(0|[1-9]\\d*)$`).test(version)
      || Object.hasOwn(inputs, 'node-version-file') || Object.hasOwn(setup, 'if')
      || Object.hasOwn(setup, 'continue-on-error')) {
      report(file, path, `UNPINNED_${code}_NODE`, `setup-node must unconditionally select one exact Node ${major} major.minor.patch string without node-version-file`);
      continue;
    }
    if (expected !== undefined && version !== expected) {
      report(file, path, `MISMATCHED_${code}_NODE`, `expected ${expected}, received ${version}`);
    }
    expected ??= version;
    if (steps.slice(0, steps.indexOf(setup)).some(step => typeof step?.run === 'string'
      && /\b(?:node|npm|npx|corepack|yarn|pnpm)\b/.test(step.run))) {
      report(file, path, `EARLY_${code}_NODE_COMMAND`, 'Node and dependency commands must follow setup-node and its runtime assertion');
    }
    const verification = steps[steps.indexOf(setup) + 1];
    if (!mapping(verification) || verification.run !== `test "$(node --version)" = "v${version}"`
      || Object.hasOwn(verification, 'if') || Object.hasOwn(verification, 'continue-on-error')) {
      report(file, path, `MISSING_${code}_NODE_VERIFICATION`, 'setup-node must be immediately followed by an unconditional matching runtime version assertion');
    }
  }
}

function checkDesktopNodeSetups(workflow, file, lanes) {
  const expectedVersions = new Map();
  for (const [name, majors] of Object.entries(lanes)) {
    const path = `jobs.${name}.steps`;
    const steps = workflow.jobs[name]?.steps;
    const setups = Array.isArray(steps) ? steps.filter(step => mapping(step)
      && typeof step.uses === 'string' && step.uses.split('@')[0].toLowerCase() === 'actions/setup-node') : [];
    if (setups.length !== majors.length) {
      report(file, path, 'INVALID_DESKTOP_NODE_SETUP', `expected ${majors.length} setup-node steps for the pinned Node majors`);
      continue;
    }
    for (const [index, setup] of setups.entries()) {
      const major = majors[index];
      const version = setup.with?.['node-version'];
      if (!mapping(setup.with) || typeof version !== 'string'
        || !new RegExp(`^${major}\\.(0|[1-9]\\d*)\\.(0|[1-9]\\d*)$`).test(version)
        || Object.hasOwn(setup.with, 'node-version-file') || Object.hasOwn(setup, 'if')
        || Object.hasOwn(setup, 'continue-on-error')) {
        report(file, path, 'UNPINNED_DESKTOP_NODE', `setup-node must select one exact Node ${major} patch without a step condition or failure overrides`);
        continue;
      }
      if (expectedVersions.has(major) && expectedVersions.get(major) !== version) {
        report(file, path, 'MISMATCHED_DESKTOP_NODE', `expected ${expectedVersions.get(major)}, received ${version}`);
      }
      expectedVersions.set(major, version);
      const verification = steps[steps.indexOf(setup) + 1];
      if (!mapping(verification) || verification.run !== `test "$(node --version)" = "v${version}"`
        || Object.hasOwn(verification, 'if') || Object.hasOwn(verification, 'continue-on-error')) {
        report(file, path, 'MISSING_DESKTOP_NODE_VERIFICATION', 'setup-node must be immediately followed by an unconditional matching runtime assertion');
      }
    }
  }
}

function checkUses(step, file, path, seen) {
  if (!Object.hasOwn(step, 'uses')) return;
  if (!literal(step.uses)) {
    report(file, `${path}.uses`, 'INVALID_USES', 'uses must be a nonempty literal string');
    return;
  }
  const pinned = step.uses.startsWith('docker://')
    ? /^docker:\/\/[^@\s]+@sha256:[0-9a-fA-F]{64}$/.test(step.uses)
    : /^[^@\s]+@[0-9a-fA-F]{40}$/.test(step.uses);
  if (!step.uses.startsWith('./') && !pinned) {
    report(file, `${path}.uses`, 'UNPINNED_ACTION', `${JSON.stringify(step.uses)}; expected a 40-character commit SHA or Docker sha256 digest`);
  }
  const action = step.uses.split('@')[0].trim().toLowerCase();
  if (action !== 'taiki-e/install-action' && !action.startsWith('taiki-e/install-action/')) return;
  if (seen.has(step)) return;
  seen.add(step);
  declarations++;
  if (action !== 'taiki-e/install-action' || !/^taiki-e\/install-action@[^@\s]+$/i.test(step.uses)) {
    report(file, `${path}.uses`, 'INVALID_INSTALLER', `unsupported installer reference ${step.uses}; use taiki-e/install-action@ref with with.tool`);
    return;
  }
  const tool = mapping(step.with) ? step.with.tool : undefined;
  if (!literal(tool)) {
    report(file, `${path}.with.tool`, 'INVALID_TOOL', 'installer requires a nonempty literal string with.tool');
    return;
  }
  const names = new Set();
  for (const group of tool.split(',')) {
    const tokens = group.trim().split(/\s+/);
    for (const token of tokens) {
      if (!/^[A-Za-z0-9_-]+@(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.test(token)) {
        report(file, `${path}.with.tool`, 'UNPINNED_TOOL', `${JSON.stringify(token)}; expected name@major.minor.patch`);
        continue;
      }
      tools++;
      const name = token.split('@')[0];
      if (names.has(name)) report(file, `${path}.with.tool`, 'DUPLICATE_TOOL', `duplicate tool ${name}`);
      names.add(name);
    }
  }
}

function checkWorkflow(file, source = readFileSync(file, 'utf8')) {
  const document = yaml.parseDocument(source, { uniqueKeys: true, merge: false });
  for (const error of [...document.errors, ...document.warnings]) {
    report(file, '$', 'INVALID_YAML', error.message);
  }
  if (document.errors.length || document.warnings.length) return;
  const initialErrors = errors;
  yaml.visit(document, {
    Pair(_key, pair) {
      if (!yaml.isScalar(pair.key) || typeof pair.key.value !== 'string') {
        report(file, '$', 'UNSUPPORTED_YAML', 'mapping keys must be strings');
      } else if (pair.key.value === '<<') {
        report(file, '$', 'UNSUPPORTED_YAML', 'merge keys are unsupported; use explicit mappings or aliases');
      }
    },
    Node(_key, node) {
      if (node.tag && !node.tag.startsWith('tag:yaml.org,2002:')) {
        report(file, '$', 'UNSUPPORTED_YAML', `unsupported tag ${node.tag}`);
      }
    },
  });
  if (errors !== initialErrors) return;
  const workflow = document.toJS({ maxAliasCount: 100 });
  if (!mapping(workflow) || !mapping(workflow.jobs) || Object.keys(workflow.jobs).length === 0) {
    report(file, 'jobs', 'INVALID_SHAPE', 'workflow requires a nonempty jobs mapping');
    return;
  }
  if (desktopContracts.test(basename(file))) {
    checkDesktopNode(workflow, file);
    if (eventMode) checkDesktopExecution(workflow, file);
  }
  for (const [name, lane, code, major] of desktopNodeLanes) {
    if (new RegExp(`^${name}\\.ya?ml$`, 'i').test(basename(file))) {
      checkNodeLanes(workflow, file, [lane], code, "github.event_name != 'pull_request' || contains(github.event.pull_request.labels.*.name, 'ci:desktop-smoke')", major);
    }
  }
  for (const [name, lanes] of Object.entries(desktopNodeSetups)) {
    if (!new RegExp(`^${name}\\.ya?ml$`, 'i').test(basename(file))) continue;
    checkDesktopNodeSetups(workflow, file, lanes);
  }
  if (/^web-ci\.ya?ml$/.test(basename(file))) checkNodeLanes(workflow, file, ['web', 'web-build']);
  if (/^integration-smoke\.ya?ml$/i.test(basename(file))) checkNodeLanes(workflow, file, ['postgres'], 'INTEGRATION', "github.event_name != 'workflow_dispatch' || inputs.postgres");
  if (/^pr-evidence\.ya?ml$/i.test(basename(file))) checkNodeLanes(workflow, file, ['lint-pr-body'], 'EVIDENCE');
  const seen = new Set();
  for (const [name, job] of Object.entries(workflow.jobs)) {
    const path = `jobs.${name}`;
    if (!mapping(job)) {
      report(file, path, 'INVALID_SHAPE', 'job must be a mapping');
      continue;
    }
    if (Object.hasOwn(job, 'uses')) {
      if (Object.hasOwn(job, 'steps')) report(file, path, 'INVALID_SHAPE', 'job cannot contain both uses and steps');
      checkUses(job, file, path, seen);
      continue;
    }
    if (!Array.isArray(job.steps)) {
      report(file, `${path}.steps`, 'INVALID_SHAPE', 'steps must be a sequence');
      continue;
    }
    for (const [index, step] of job.steps.entries()) {
      const stepPath = `${path}.steps[${index}]`;
      if (!mapping(step)) report(file, stepPath, 'INVALID_SHAPE', 'step must be a mapping');
      else checkUses(step, file, stepPath, seen);
    }
  }
}

function checkDesktopExecution(workflow, file) {
  const requiredPaths = [
    'fixtures/http/routes-inventory.json', 'rust/**', 'rust-toolchain.toml',
    'web/packages/contracts/**', 'web/packages/domain/**', 'web/packages/rust-client/**', 'web/apps/server/**',
    'web/apps/web/src/api/planReview.ts', 'web/scripts/desktop-parity/**',
    'web/scripts/desktop-contracts/**', 'web/scripts/rust-contracts/**',
    'web/package*.json', 'docs/adr/0006-autosave-text-contract.md',
    `.github/workflows/${basename(file)}`,
  ];
  for (const event of ['push', 'pull_request']) {
    const trigger = workflow.on?.[event];
    const allowed = event === 'push' ? ['branches', 'paths'] : ['paths', 'types'];
    if (!mapping(trigger) || Object.keys(trigger).some(key => !allowed.includes(key))
      || (event === 'pull_request' && Object.hasOwn(trigger, 'types')
        && (!Array.isArray(trigger.types) || !['opened', 'synchronize', 'reopened', 'ready_for_review', 'converted_to_draft']
          .every(type => trigger.types.includes(type))))
      || !Array.isArray(trigger.paths) || !requiredPaths.every(path => trigger.paths.includes(path))
      || trigger.paths.some(path => typeof path !== 'string' || path.startsWith('!'))
      || (event === 'push' && (!Array.isArray(trigger.branches) || !trigger.branches.includes('main')
        || trigger.branches.some(branch => typeof branch !== 'string' || branch.startsWith('!'))))) {
      report(file, `on.${event}`, 'INVALID_DESKTOP_TRIGGERS', 'required automatic trigger coverage must remain enabled');
    }
  }
  const job = workflow.jobs.parity;
  if (!mapping(job) || ['needs', 'continue-on-error'].some(key => Object.hasOwn(job, key))
    || (Object.hasOwn(job, 'if') && job.if !== draftGuard)) {
    report(file, 'jobs.parity', 'DISABLED_DESKTOP_JOB', 'parity must run on ready PRs and propagate failure');
    return;
  }
  for (const [index, step] of (Array.isArray(job.steps) ? job.steps : []).entries()) {
    if (!mapping(step)) continue;
    if (Object.hasOwn(step, 'continue-on-error') || (Object.hasOwn(step, 'if') && step.if !== 'always()')) {
      report(file, `jobs.parity.steps[${index}]`, 'DISABLED_DESKTOP_STEP', 'verification steps must run unconditionally and propagate failure');
    }
  }
  if (!job.steps?.some(step => typeof step?.run === 'string'
    && step.run.includes('npm run contracts:verify') && step.run.includes('npm run test:desktop-parity'))) {
    report(file, 'jobs.parity.steps', 'MISSING_DESKTOP_VERIFICATION', 'expected contracts verification and desktop parity commands');
  }
}

function checkEventTree() {
  if (!/^[0-9a-f]{40}$/.test(eventSha)) throw new Error('expected a full lowercase event commit SHA');
  const git = args => execFileSync('git', ['--no-replace-objects', ...args], { cwd: root, encoding: 'utf8' });
  if (git(['cat-file', '-t', eventSha]).trim() !== 'commit') throw new Error('event SHA must identify a commit');
  const entries = git(['ls-tree', '-z', eventSha, '--', '.github/workflows/']).split('\0')
    .filter(Boolean).map(entry => {
      const separator = entry.indexOf('\t');
      const metadata = entry.slice(0, separator);
      const path = entry.slice(separator + 1);
      const [mode, type, oid] = metadata.split(' ');
      return { mode, type, oid, path };
    });
  checkNodeWorkflowInventory(entries.map(entry => ({ name: basename(entry.path), regular: entry.type === 'blob' && /^100(?:644|755)$/.test(entry.mode) })), eventSha);
  const contracts = entries.filter(entry => /^\.github\/workflows\/desktop-contracts\.ya?ml$/.test(entry.path));
  if (contracts.length !== 1 || contracts[0].path !== '.github/workflows/desktop-contracts.yml'
    || !/^100(?:644|755)$/.test(contracts[0].mode) || contracts[0].type !== 'blob') {
    report(eventSha, '.github/workflows', 'INVALID_EVENT_DESKTOP_CONTRACTS', 'expected exactly one regular desktop-contracts.yml workflow in the event tree');
    return;
  }
  const web = entries.filter(entry => /^\.github\/workflows\/web-ci\.ya?ml$/i.test(entry.path));
  if (web.length !== 1 || web[0].path !== '.github/workflows/web-ci.yml'
    || !/^100(?:644|755)$/.test(web[0].mode) || web[0].type !== 'blob') {
    report(eventSha, '.github/workflows', 'INVALID_EVENT_WEB_CI', 'expected exactly one regular canonical web-ci.yml workflow in the event tree');
    return;
  }
  for (const entry of entries.filter(entry => /\.ya?ml$/i.test(entry.path))) {
    if (entry.type !== 'blob' || !/^100(?:644|755)$/.test(entry.mode)) {
      report(entry.path, '$', 'INVALID_EVENT_WORKFLOW', 'workflow must be a regular file');
      continue;
    }
    checkWorkflow(entry.path, git(['cat-file', 'blob', entry.oid]));
  }
}

if (eventMode) {
  try {
    checkEventTree();
  } catch (error) {
    report(eventSha, '$', 'INVALID_EVENT_TREE', error.message);
  }
} else try {
  const entries = readdirSync(directory, { withFileTypes: true });
  const files = entries
    .filter(entry => /\.ya?ml$/i.test(entry.name))
    .map(entry => join(directory, entry.name)).sort();
  if (files.length === 0) report(directory, '$', 'NO_WORKFLOWS', 'no .yml or .yaml workflow files found');
  const contracts = entries.filter(entry => entry.isFile() && desktopContracts.test(entry.name))
    .map(entry => entry.name).sort();
  if (contracts.length === 0) {
    report(directory, '$', 'MISSING_DESKTOP_CONTRACTS', 'expected one desktop-contracts.yml or desktop-contracts.yaml workflow');
  } else if (contracts.length > 1) {
    report(directory, '$', 'DUPLICATE_DESKTOP_CONTRACTS', 'expected one desktop contracts workflow; found ' + contracts.map(file => basename(file)).join(', '));
  }
  if (process.argv[2] === undefined) {
    checkNodeWorkflowInventory(entries.map(entry => ({ name: entry.name, regular: entry.isFile() })), directory);
    const trackedNodes = execFileSync('git', ['ls-tree', '-z', 'HEAD', '--', '.github/workflows/'], { cwd: root, encoding: 'utf8' })
      .split('\0').filter(Boolean).map(entry => ({ name: basename(entry.split('\t')[1]), regular: /^100(?:644|755) blob /.test(entry) }));
    checkNodeWorkflowInventory(trackedNodes, 'HEAD');
    const web = entries.filter(entry => /^web-ci\.ya?ml$/i.test(entry.name));
    if (web.length !== 1 || web[0].name !== 'web-ci.yml' || !web[0].isFile()) {
      report(directory, '$', 'INVALID_WEB_CI', 'expected exactly one regular canonical web-ci.yml workflow');
    }
    const trackedWeb = execFileSync('git', ['ls-tree', '-z', 'HEAD', '--', '.github/workflows/'], { cwd: root, encoding: 'utf8' })
      .split('\0').filter(entry => /\t\.github\/workflows\/web-ci\.ya?ml$/i.test(entry));
    if (trackedWeb.length !== 1 || !/^100(?:644|755) blob [0-9a-f]+\t\.github\/workflows\/web-ci\.yml$/.test(trackedWeb[0])) {
      report(directory, '$', 'UNTRACKED_WEB_CI', 'expected one regular canonical Web CI workflow in HEAD');
    }
    const tracked = execFileSync('git', ['ls-tree', '-z', 'HEAD', '--', '.github/workflows/'], { cwd: root, encoding: 'utf8' })
      .split('\0').filter(entry => /^100(?:644|755) blob [0-9a-f]+\t\.github\/workflows\/desktop-contracts\.ya?ml$/.test(entry))
      .map(entry => basename(entry.split('\t')[1]));
    if (tracked.length !== 1 || contracts.length !== 1 || tracked[0] !== contracts[0]) {
      report(directory, '$', 'UNTRACKED_DESKTOP_CONTRACTS', 'expected exactly one regular desktop contracts workflow in HEAD');
    }
  }
  for (const file of files) {
    try {
      checkWorkflow(file);
    } catch (error) {
      report(file, '$', 'READ_OR_PARSE_ERROR', error.message);
    }
  }
} catch (error) {
  report(directory, '$', 'READ_ERROR', error.message);
}
console.log(`install_declarations=${declarations} tool_specs=${tools} errors=${errors}`);
process.exitCode = errors ? 1 : 0;
