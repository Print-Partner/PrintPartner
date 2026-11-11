import argparse
import hashlib
import json
import os
import pathlib
import shutil
import subprocess
import sys

from desktop_workspaces import ARTIFACTS
from stage_dependencies import stage_locked_dependencies

NATIVE_NODE_HEADERS = frozenset({
    b'\x7fELF',
    b'\xfe\xed\xfa\xcf',
    b'\xcf\xfa\xed\xfe',
    b'\xfe\xed\xfa\xce',
    b'\xce\xfa\xed\xfe',
    b'\xca\xfe\xba\xbe',
    b'\xbe\xba\xfe\xca',
    b'\xca\xfe\xba\xbf',
    b'\xbf\xba\xfe\xca',
})

if '--self-test' in sys.argv[1:]:
    import unittest
    suite = unittest.defaultTestLoader.discover(
        str(pathlib.Path(__file__).resolve().parent),
        pattern='test_stage_desktop.py',
    )
    raise SystemExit(0 if unittest.TextTestRunner(verbosity=2).run(suite).wasSuccessful() else 1)


parser = argparse.ArgumentParser()
parser.add_argument('--web', type=pathlib.Path, required=True)
parser.add_argument('--node', type=pathlib.Path, required=True)
parser.add_argument('--output', type=pathlib.Path, required=True)
parser.add_argument('--commit', required=True)
args = parser.parse_args()
source = args.web.resolve(strict=True)
stage = args.output.absolute()
if stage.exists():
    parser.error('Stage must be a new directory')

def reject_node(path):
    parser.error(
        'supply the native pinned Node 24.21.0 ABI 137 runtime '
        '(Linux ELF or macOS Mach-O), not a dispatcher script: ' + str(path))

def native_node_binary(path):
    try:
        resolved = path.resolve(strict=True)
    except OSError:
        parser.error('Node runtime does not exist: ' + str(path))
    if not resolved.is_file():
        reject_node(path)
    with resolved.open('rb') as handle:
        header = handle.read(4)
    if header not in NATIVE_NODE_HEADERS:
        reject_node(path)
    return resolved

node_binary = native_node_binary(args.node)
node_identity = json.loads(subprocess.check_output([str(node_binary), '-p',
    'JSON.stringify({version:process.version,abi:process.versions.modules,os:process.platform,arch:process.arch})'], text=True))
if node_identity['version'] != 'v24.21.0' or node_identity['abi'] != '137':
    parser.error('Node 24.21.0 ABI 137 is required')
system = {'darwin': 'macos', 'linux': 'linux'}.get(node_identity['os'])
arch = {'x64': 'x86_64', 'arm64': 'aarch64'}.get(node_identity['arch'])
if not system or not arch or system != ('macos' if sys.platform == 'darwin' else sys.platform):
    parser.error('Stage requires the target-native Node runtime')
web_relative = pathlib.Path('Resources/desktop-runtime/web' if system == 'macos' else 'web')
node_relative = pathlib.Path('MacOS/printpartner-node' if system == 'macos' else 'bin/node')
web = stage / web_relative
node = stage / node_relative
node.parent.mkdir(parents=True)
shutil.copy2(node_binary, node)
for path in ARTIFACTS.values():
    shutil.copytree(source / path, web / path)
for path in ['package.json', 'package-lock.json', 'apps/server/package.json',
             'packages/contracts/package.json', 'packages/domain/package.json']:
    target = web / path
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source / path, target)
skipped_links = stage_locked_dependencies(source, web, json.loads((source / 'package-lock.json').read_text()))
for name, relative in [('contracts', 'packages/contracts'), ('domain', 'packages/domain')]:
    alias = web / 'node_modules/@print-partner' / name
    if not alias.exists():
        alias.parent.mkdir(parents=True, exist_ok=True)
        alias.symlink_to(os.path.relpath(web / relative, alias.parent))
if system == 'macos':
    from macos_addon_slices import normalize_addon, select_better_sqlite3_prebuild
    target_architecture = {'aarch64': 'arm64', 'x86_64': 'x86_64'}[arch]
    sqlite_prebuilds = web / 'node_modules/better-sqlite3/prebuilds'
    sqlite_selection = select_better_sqlite3_prebuild(
        [entry.name for entry in sqlite_prebuilds.iterdir()],
        target_architecture,
    )
    for name in sqlite_selection.discard_names:
        (sqlite_prebuilds / name).unlink()
    for addon in list(web.rglob('*.node')):
        normalize_addon(addon, target_architecture)
    frameworks = stage / 'Frameworks'
    frameworks.mkdir()
    for addon in list(web.rglob('*.node')):
        name = hashlib.sha256(addon.relative_to(web).as_posix().encode()).hexdigest()[:12]+'_'+addon.name
        destination = frameworks / name
        shutil.move(addon, destination)
        addon.symlink_to(os.path.relpath(destination, addon.parent))
subprocess.run([str(node), '-e',
    "const Database=require('better-sqlite3');const db=new Database(':memory:');"
    "if(db.prepare('SELECT 42 answer').get().answer!==42)process.exit(1);db.close();"], cwd=web, check=True)
package = json.loads((web / 'package.json').read_text())
release = {'runtime_version': package['version']+'-web', 'commit': args.commit,
           'node': node_relative.as_posix(), 'web': web_relative.as_posix(), 'os': system,
           'arch': arch, 'node_version': node_identity['version'], 'node_abi': node_identity['abi']}
identity_path = stage / ('Resources/desktop-runtime/release.json' if system == 'macos' else 'release.json')
identity_path.write_text(json.dumps(release, sort_keys=True)+'\n')
if system == 'macos':
    native_config = json.loads((pathlib.Path(__file__).resolve().parents[1] / 'crates/pp-desktop/tauri.conf.json').read_text())
    bundle = {'resources': {},
              'macOS': {'minimumSystemVersion': native_config['bundle']['macOS']['minimumSystemVersion'],
                        'files': {str(path.relative_to(stage)): str(path)
                                  for directory in ['MacOS', 'Frameworks']
                                  for path in (stage / directory).iterdir() if path.is_file()}}}
else:
    bundle = {'resources': {str(stage / 'web'): 'desktop-runtime/web',
                            str(stage / 'bin'): 'desktop-runtime/bin',
                            str(stage / 'release.json'): 'desktop-runtime/release.json'}}
bundle.update({'active': True, 'icon': [str(pathlib.Path(__file__).resolve().parents[1] / 'crates/pp-desktop/icons/icon.png')]})
(stage / 'bundle-config.json').write_text(json.dumps({'bundle': bundle}, sort_keys=True)+'\n')
manifest = stage / 'bundle-manifest.json'
subprocess.run([sys.executable, str(pathlib.Path(__file__).with_name('build-desktop-manifest.py')),
    '--web', str(web), '--node', str(node), '--package-root', str(stage), '--output', str(manifest),
    '--commit', args.commit], check=True)
print(json.dumps({'stage': str(stage), 'release': release, 'sqlite_answer': 42,
                  'signed': False, 'manifest': str(manifest),
                  'skipped_workspace_links': skipped_links}))
