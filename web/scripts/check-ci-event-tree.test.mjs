import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { chmodSync, copyFileSync, mkdirSync, mkdtempSync, readFileSync, renameSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import process from 'node:process';
import test from 'node:test';
import { fileURLToPath, URL } from 'node:url';
import YAML from 'yaml';

const root = fileURLToPath(new URL('../../', import.meta.url));
const cli = 'web/scripts/check-ci-tool-pins.mjs';
const contracts = '.github/workflows/desktop-contracts.yml';
const original = readFileSync(join(root, contracts), 'utf8');
const webCI = YAML.parse(readFileSync(join(root, '.github/workflows/web-ci.yml'), 'utf8'));

function fixture(t, mutate = () => {}) {
  const directory = mkdtempSync(join(tmpdir(), 'ci-event-tree-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  mkdirSync(join(directory, '.github/workflows'), { recursive: true });
  mkdirSync(join(directory, 'web/scripts'), { recursive: true });
  copyFileSync(join(root, cli), join(directory, cli));
  const stateCLI = 'web/scripts/check-desktop-workflow-state.mjs';
  copyFileSync(join(root, stateCLI), join(directory, stateCLI));
  mkdirSync(join(directory, 'bin'));
  writeFileSync(join(directory, 'bin/gh'), `#!/usr/bin/env node
if (JSON.stringify(process.argv.slice(2)) !== JSON.stringify(['api', '--hostname', 'github.com', 'repos/Print-Partner/PrintPartner/actions/workflows/desktop-contracts.yml'])) process.exit(9);
console.log(JSON.stringify({ path: '.github/workflows/desktop-contracts.yml', state: process.env.TEST_WORKFLOW_STATE ?? 'active' }));
`);
  chmodSync(join(directory, 'bin/gh'), 0o755);
  symlinkSync(join(root, 'web/node_modules'), join(directory, 'web/node_modules'));
  writeFileSync(join(directory, contracts), original);
  writeFileSync(join(directory, '.github/workflows/web-ci.yml'), YAML.stringify(webCI));
  for (const name of ['integration-smoke', 'desktop-e2e', 'desktop-webkit', 'desktop-runtime', 'desktop-native-macos']) {
    copyFileSync(join(root, `.github/workflows/${name}.yml`), join(directory, `.github/workflows/${name}.yml`));
  }
  const git = (...args) => {
    const result = spawnSync('git', args, { cwd: directory, encoding: 'utf8' });
    assert.equal(result.status, 0, result.stderr);
    return result.stdout.trim();
  };
  git('init', '-q');
  mutate(directory);
  git('add', '.github', cli);
  const commit = () => {
    git('-c', 'user.name=CI test', '-c', 'user.email=ci@example.invalid', 'commit', '--allow-empty', '-qm', 'fixture');
    return git('rev-parse', 'HEAD');
  };
  const sha = commit();
  const run = (eventSha = sha) => spawnSync(process.execPath, [join(directory, cli), '--event-sha', eventSha], {
    cwd: directory, encoding: 'utf8',
  });
  return { directory, git, commit, sha, run };
}

for (const [name, mutate] of [
  ['missing', directory => rmSync(join(directory, contracts))],
  ['renamed', directory => renameSync(join(directory, contracts), join(directory, '.github/workflows/renamed.yml'))],
  ['disabled suffix', directory => renameSync(join(directory, contracts), join(directory, `${contracts}.disabled`))],
  ['uppercase extension', directory => renameSync(join(directory, contracts), join(directory, '.github/workflows/desktop-contracts.YML'))],
  ['symlink', directory => {
    rmSync(join(directory, contracts));
    symlinkSync('other.yml', join(directory, contracts));
    writeFileSync(join(directory, '.github/workflows/other.yml'), original);
  }],
  ['duplicate', directory => writeFileSync(join(directory, '.github/workflows/desktop-contracts.yaml'), original)],
]) {
  test(`event tree rejects ${name} despite restored worktree and HEAD`, t => {
    const f = fixture(t, mutate);
    rmSync(join(f.directory, contracts), { force: true });
    writeFileSync(join(f.directory, contracts), original);
    if (name === 'duplicate') rmSync(join(f.directory, '.github/workflows/desktop-contracts.yaml'));
    f.git('add', '.github');
    f.commit();
    const result = f.run();
    assert.equal(result.status, 1, result.stdout + result.stderr);
    assert.match(result.stderr, /INVALID_EVENT_DESKTOP_CONTRACTS/);
  });
}

const step = (w, name) => w.jobs.parity.steps.find(s => s.name === name);
const hostedOnly = "runner.environment == 'self-hosted'";

const modify = change => directory => {
  const workflow = YAML.parse(original);
  change(workflow);
  writeFileSync(join(directory, contracts), YAML.stringify(workflow));
};

test('event tree rejects unpinned actions in tabbed workflow filename', t => {
  const f = fixture(t, directory => {
    writeFileSync(join(directory, '.github/workflows/probe\t.yml'),
      'jobs: {probe: {steps: [{uses: actions/checkout@main}]}}');
  });
  const result = f.run();
  assert.equal(result.status, 1, result.stdout + result.stderr);
  assert.match(result.stderr, /UNPINNED_ACTION/);
});

test('event tree does not mistake tabbed filename for required contracts workflow', t => {
  const f = fixture(t, directory => {
    renameSync(join(directory, contracts), join(directory, `${contracts}\tother.yml`));
  });
  const result = f.run();
  assert.equal(result.status, 1, result.stdout + result.stderr);
  assert.match(result.stderr, /INVALID_EVENT_DESKTOP_CONTRACTS/);
});

test('event tree accepts pinned actions in tabbed workflow filename', t => {
  const f = fixture(t, directory => {
    writeFileSync(join(directory, '.github/workflows/probe\t.yml'),
      `jobs: {probe: {steps: [{uses: actions/checkout@${'a'.repeat(40)}}]}}`);
  });
  const result = f.run();
  assert.equal(result.status, 0, result.stderr);
});

for (const [name, change] of [
  ['pull request trigger', w => { w.on.pull_request = { types: ['opened'] }; }],
  ['push trigger', w => { w.on.push = { branches: ['main'] }; }],
  ['merge group trigger', w => { w.on.merge_group = { types: ['checks_requested'] }; }],
  ['schedule trigger', w => { w.on.schedule = [{ cron: '0 0 * * *' }]; }],
  ['manual trigger removed', w => { delete w.on.workflow_dispatch; }],
  ['trigger list', w => { w.on = ['workflow_dispatch']; }],
  ['job false', w => { w.jobs.parity.if = false; }],
  ['job expression', w => { w.jobs.parity.if = '${{ false }}'; }],
  ['job dependency', w => { w.jobs.parity.needs = ['skipped']; }],
  ['job failure ignored', w => { w.jobs.parity['continue-on-error'] = true; }],
  ['step false', w => { w.jobs.parity.steps[0].if = false; }],
  ['step conditional runner', w => { w.jobs.parity.steps[0].if = "runner.environment == 'self-hosted'"; }],
  ['step conditional always', w => { w.jobs.parity.steps[0].if = 'always() && false'; }],
  ['always step failure ignored', w => { w.jobs.parity.steps[0].if = 'always()'; w.jobs.parity.steps[0]['continue-on-error'] = true; }],
  ['step failure ignored', w => { w.jobs.parity.steps[0]['continue-on-error'] = true; }],
  ['verification only on ephemeral runners', w => { step(w, 'Verify contracts and frozen corpus').if = hostedOnly; }],
  ['hosted Rust install only on ephemeral runners', w => { step(w, 'Install pinned Rust checks').if = hostedOnly; }],
  ['self-hosted bootstrap step', w => { w.jobs.parity.steps.splice(1, 0, { name: 'Set up ephemeral Linux prerequisites', if: hostedOnly, run: 'bash "$GITHUB_WORKSPACE/scripts/setup-ci-linux.sh"' }); }],
  ['verification removed', w => { w.jobs.parity.steps = w.jobs.parity.steps.filter(s => !s.run?.includes('npm run contracts:verify')); }],
]) {
  test(`event tree rejects disabled workflow ${name}`, t => {
    const f = fixture(t, modify(change));
    writeFileSync(join(f.directory, contracts), original);
    const result = f.run();
    assert.equal(result.status, 1, result.stdout + result.stderr);
    assert.match(result.stderr, /INVALID_DESKTOP_TRIGGERS|DISABLED_DESKTOP_|MISSING_DESKTOP_VERIFICATION/);
  });
}

test('desktop contracts does not run on pull requests or main', () => {
  const events = YAML.parse(original).on;
  assert.equal(events.pull_request, undefined);
  assert.equal(events.push, undefined);
  assert.equal(events.merge_group, undefined);
  assert.equal(events.schedule, undefined);
  assert.ok(Object.hasOwn(events, 'workflow_dispatch'));
});

test('event tree allows no runner-conditioned steps', t => {
  const workflow = YAML.parse(original);
  const conditioned = workflow.jobs.parity.steps.filter(s => s.if !== undefined && s.if !== 'always()');
  assert.deepEqual(conditioned.map(s => [s.name, s.if]), []);
  const result = fixture(t).run();
  assert.equal(result.status, 0, result.stderr);
});

test('event tree accepts always-run steps that propagate failure', t => {
  const f = fixture(t, modify(w => {
    const verification = w.jobs.parity.steps.find(s => s.run?.includes('npm run contracts:verify'));
    verification.if = 'always()';
  }));
  const result = f.run();
  assert.equal(result.status, 0, result.stderr);
});

test('event tree accepts original blob despite deleted worktree and moved HEAD', t => {
  const f = fixture(t);
  rmSync(join(f.directory, contracts));
  f.git('add', '.github');
  f.commit();
  const result = f.run();
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stdout, 'install_declarations=1 tool_specs=1 errors=0\n');
});

test('event tree ignores replacement commit', t => {
  const f = fixture(t, directory => rmSync(join(directory, contracts)));
  writeFileSync(join(f.directory, contracts), original);
  f.git('add', '.github');
  const replacement = f.commit();
  f.git('replace', f.sha, replacement);
  assert.equal(f.run().status, 1);
});

test('event tree ignores replacement workflow blob', t => {
  const f = fixture(t, modify(w => { w.jobs.parity.if = false; }));
  const disabled = f.git('rev-parse', `${f.sha}:${contracts}`);
  writeFileSync(join(f.directory, contracts), original);
  f.git('add', '.github');
  const replacement = f.git('rev-parse', `:${contracts}`);
  f.git('replace', disabled, replacement);
  const result = f.run();
  assert.equal(result.status, 1, result.stdout + result.stderr);
  assert.match(result.stderr, /DISABLED_DESKTOP_JOB/);
});

test('event tree rejects gitlink at required workflow path', t => {
  const f = fixture(t);
  f.git('update-index', '--add', '--cacheinfo', `160000,${f.sha},${contracts}`);
  const sha = f.commit();
  const result = f.run(sha);
  assert.equal(result.status, 1, result.stdout + result.stderr);
  assert.match(result.stderr, /INVALID_EVENT_DESKTOP_CONTRACTS/);
});

test('event tree rejects yaml extension despite a dispatch-only workflow', t => {
  const f = fixture(t);
  f.git('mv', contracts, '.github/workflows/desktop-contracts.yaml');
  const sha = f.commit();
  const result = f.run(sha);
  assert.equal(result.status, 1, result.stdout + result.stderr);
  assert.match(result.stderr, /INVALID_EVENT_DESKTOP_CONTRACTS/);
});

test('event tree accepts the dispatch-only contracts workflow', t => {
  const result = fixture(t).run();
  assert.equal(result.status, 0, result.stderr);
});

for (const sha of ['', 'HEAD', 'a'.repeat(40), '--help', 'a'.repeat(39)]) {
  test(`event tree rejects invalid or unavailable SHA ${JSON.stringify(sha)}`, t => {
    const f = fixture(t);
    const result = f.run(sha);
    assert.equal(result.status, 1, result.stderr);
    assert.match(result.stderr, /INVALID_EVENT_TREE/);
  });
}

test('replacement UI job lints, tests and builds the UI package', () => {
  const job = webCI.jobs.ui;
  assert.equal(job.needs[0], 'changes');
  assert.match(job.if, /needs\.changes\.outputs\.ui != 'false'/);
  const run = job.steps.find(step => step.name === 'Lint, test and build the replacement UI').run;
  assert.match(run, /npm run lint --workspace @print-partner\/ui/);
  assert.match(run, /npm test -w @print-partner\/rust-client/);
  assert.match(run, /npm run test --workspace @print-partner\/ui/);
  assert.match(run, /npm run build --workspace @print-partner\/ui/);
  assert.doesNotMatch(run, /@print-partner\/web/);
});

test('Release grants its reusable Web CI caller permission to read workflow state', () => {
  const release = YAML.parse(readFileSync(join(root, '.github/workflows/release.yml'), 'utf8'));
  assert.equal(release.jobs.quality.uses, './.github/workflows/web-ci.yml');
  assert.equal(release.jobs.quality.permissions.actions, 'read');
});

for (const lane of ['ui']) {
  test(`event tree rejects floating Web CI ${lane} Node despite repaired checkout`, t => {
    const f = fixture(t, directory => {
      const workflow = JSON.parse(JSON.stringify(webCI));
      workflow.jobs[lane].steps.find(step => step.uses?.startsWith('actions/setup-node@')).with['node-version'] = '22';
      writeFileSync(join(directory, '.github/workflows/web-ci.yml'), YAML.stringify(workflow));
    });
    writeFileSync(join(f.directory, '.github/workflows/web-ci.yml'), YAML.stringify(webCI));
    f.git('add', '.github');
    f.commit();
    const result = f.run();
    assert.equal(result.status, 1, result.stdout + result.stderr);
    assert.match(result.stderr, new RegExp(`jobs.${lane}: UNPINNED_WEB_NODE:`));
    const repaired = f.run(f.git('rev-parse', 'HEAD'));
    assert.equal(repaired.status, 0, repaired.stderr);
  });
}

for (const [name, mutate] of [
  ['missing', directory => rmSync(join(directory, '.github/workflows/web-ci.yml'))],
  ['renamed', directory => renameSync(join(directory, '.github/workflows/web-ci.yml'), join(directory, '.github/workflows/renamed-web.yml'))],
  ['yaml extension', directory => renameSync(join(directory, '.github/workflows/web-ci.yml'), join(directory, '.github/workflows/web-ci.yaml'))],
  ['uppercase extension', directory => renameSync(join(directory, '.github/workflows/web-ci.yml'), join(directory, '.github/workflows/web-ci.YML'))],
  ['duplicate', directory => copyFileSync(join(directory, '.github/workflows/web-ci.yml'), join(directory, '.github/workflows/web-ci.yaml'))],
  ['symlink', directory => {
    renameSync(join(directory, '.github/workflows/web-ci.yml'), join(directory, '.github/workflows/real-web.yml'));
    symlinkSync('real-web.yml', join(directory, '.github/workflows/web-ci.yml'));
  }],
]) {
  test(`event tree rejects Web CI identity ${name}`, t => {
    const f = fixture(t, mutate);
    const result = f.run();
    assert.equal(result.status, 1, result.stdout + result.stderr);
    assert.match(result.stderr, /INVALID_EVENT_WEB_CI/);
    const scan = spawnSync(process.execPath, [join(f.directory, cli)], { cwd: f.directory, encoding: 'utf8' });
    assert.equal(scan.status, 1, scan.stdout + scan.stderr);
    assert.match(scan.stderr, /INVALID_WEB_CI/);
  });
}

test('default scan rejects untracked Web CI despite repaired worktree', t => {
  const f = fixture(t, directory => rmSync(join(directory, '.github/workflows/web-ci.yml')));
  writeFileSync(join(f.directory, '.github/workflows/web-ci.yml'), YAML.stringify(webCI));
  const result = spawnSync(process.execPath, [join(f.directory, cli)], { cwd: f.directory, encoding: 'utf8' });
  assert.equal(result.status, 1, result.stdout + result.stderr);
  assert.match(result.stderr, /UNTRACKED_WEB_CI/);
});

test('event tree rejects gitlink at required Web CI path', t => {
  const f = fixture(t);
  f.git('update-index', '--add', '--cacheinfo', `160000,${f.sha},.github/workflows/web-ci.yml`);
  const sha = f.commit();
  const result = f.run(sha);
  assert.equal(result.status, 1, result.stdout + result.stderr);
  assert.match(result.stderr, /INVALID_EVENT_WEB_CI/);
});

for (const [name, job, diagnostic] of [
  ['integration-smoke', 'postgres', 'UNPINNED_INTEGRATION_NODE'],
  ['desktop-e2e', 'linux-smoke', 'UNPINNED_E2E_NODE'],
  ['desktop-webkit', 'webkit-stl', 'UNPINNED_WEBKIT_NODE'],
  ['desktop-runtime', 'runtime', 'UNPINNED_DESKTOP_NODE'],
  ['desktop-native-macos', 'bundle', 'UNPINNED_DESKTOP_NODE'],
]) {
  test(`event tree rejects floating ${name} despite repaired checkout`, t => {
    const path = `.github/workflows/${name}.yml`;
    const f = fixture(t, directory => {
      const workflow = YAML.parse(readFileSync(join(directory, path), 'utf8'));
      workflow.jobs[job].steps.find(step => step.uses?.startsWith('actions/setup-node@')).with['node-version'] = '22';
      writeFileSync(join(directory, path), YAML.stringify(workflow));
    });
    copyFileSync(join(root, path), join(f.directory, path));
    const result = f.run();
    assert.equal(result.status, 1, result.stdout);
    assert.match(result.stderr, new RegExp(diagnostic));
  });
  for (const variant of ['duplicate', 'symlink', 'gitlink']) {
    test(`event tree rejects ${name} ${variant}`, t => {
      const f = fixture(t, directory => {
        const path = join(directory, `.github/workflows/${name}.yml`);
        if (variant === 'duplicate') copyFileSync(path, join(directory, `.github/workflows/${name}.yaml`));
        else if (variant === 'symlink') {
          renameSync(path, `${path}.source`);
          symlinkSync(`${name}.yml.source`, path);
        }
      });
      if (variant === 'gitlink') {
        f.git('update-index', '--cacheinfo', `160000,${f.sha},.github/workflows/${name}.yml`);
        f.commit();
      }
      const result = f.run(variant === 'gitlink' ? f.git('rev-parse', 'HEAD') : f.sha);
      assert.equal(result.status, 1, result.stdout);
      assert.match(result.stderr, /INVALID_NODE_WORKFLOW/);
    });
  }
  for (const replacement of [null, `${name}.yaml`, `${name}.YML`, `${name}-renamed.yml`]) {
    test(`event tree rejects ${name} identity ${replacement}`, t => {
      const f = fixture(t, directory => {
        const original = join(directory, `.github/workflows/${name}.yml`);
        if (replacement) renameSync(original, join(directory, '.github/workflows', replacement));
        else rmSync(original);
      });
      const result = f.run();
      assert.equal(result.status, 1, result.stdout);
      assert.match(result.stderr, /INVALID_NODE_WORKFLOW/);
    });
  }
}

test('event tree rejects disabled smoke lane despite repaired checkout', t => {
  const path = '.github/workflows/integration-smoke.yml';
  const f = fixture(t, directory => {
    const workflow = YAML.parse(readFileSync(join(directory, path), 'utf8'));
    workflow.jobs.postgres.if = false;
    writeFileSync(join(directory, path), YAML.stringify(workflow));
  });
  copyFileSync(join(root, path), join(f.directory, path));
  const result = f.run();
  assert.equal(result.status, 1, result.stdout);
  assert.match(result.stderr, /INVALID_INTEGRATION_NODE_SETUP/);
});

for (const [name, lane, code] of [
  ['desktop-e2e', 'linux-smoke', 'E2E'],
  ['desktop-webkit', 'webkit-stl', 'WEBKIT'],
  ['desktop-runtime', 'runtime', 'DESKTOP'],
  ['desktop-native-macos', 'bundle', 'DESKTOP'],
]) {
  test(`event tree rejects missing ${name} assertion despite repaired checkout`, t => {
    const path = `.github/workflows/${name}.yml`;
    const f = fixture(t, directory => {
      const workflow = YAML.parse(readFileSync(join(directory, path), 'utf8'));
      const steps = workflow.jobs[lane].steps;
      const setup = steps.findIndex(step => step.uses?.startsWith('actions/setup-node@'));
      steps.splice(setup + 1, 1);
      writeFileSync(join(directory, path), YAML.stringify(workflow));
    });
    copyFileSync(join(root, path), join(f.directory, path));
    const result = f.run();
    assert.equal(result.status, 1, result.stdout);
    assert.match(result.stderr, new RegExp(`MISSING_${code}_NODE_VERIFICATION`));
  });
}
