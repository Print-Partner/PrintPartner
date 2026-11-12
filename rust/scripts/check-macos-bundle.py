#!/usr/bin/env python3
import argparse
import ast
from dataclasses import asdict, dataclass
import hashlib
import importlib.util
import json
import os
import pathlib
import plistlib
import re
import selectors
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

from desktop_workspaces import ARTIFACTS, shipped_workspaces
from macos_addon_slices import better_sqlite3_addon_path, select_better_sqlite3_prebuild

WEB = 'Resources/desktop-runtime/web'
FLOOR = (13, 5, 0)
MAX_PROBE_STREAM_BYTES = 4096
PROBE_CLEANUP_SECONDS = 1
PROBE_FORCE_CLEANUP_SECONDS = 0.2


@dataclass(frozen=True)
class ProbeCapture:
    timed_out: bool
    cleanup_timed_out: bool
    exit_code: int | None
    stdout: str
    stderr: str
    stdout_truncated: bool
    stderr_truncated: bool


def require(condition, message):
    if not condition:
        raise ValueError(message)


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def relative_path(value):
    require(isinstance(value, str) and value and '\\' not in value, 'Invalid inventory path')
    path = pathlib.PurePosixPath(value)
    require(not path.is_absolute() and all(part not in ('', '.', '..') for part in value.split('/')),
            'Inventory path must be canonical and relative: '+value)
    return value


def contained(root, path):
    resolved = path.resolve(strict=True)
    require(resolved.is_relative_to(root), 'Resource escapes Contents: '+str(path))
    return resolved


def measured_files(root, directory):
    result = {}
    for path in sorted(directory.rglob('*')):
        contained(root, path)
        if path.is_file():
            result[path.relative_to(directory).as_posix()] = digest(path)
    require(result, 'Empty artifact: '+str(directory))
    return result


def dependency_roots(root, web):
    search = {web / 'node_modules', web / 'apps/node_modules', web / 'packages/node_modules'}
    ancestor = web.parent
    while ancestor.is_relative_to(root):
        search.add(ancestor / 'node_modules')
        if ancestor == root:
            break
        ancestor = ancestor.parent
    for workspace in shipped_workspaces(web):
        for directory in [workspace, workspace / 'dist', workspace / 'dist/current']:
            search.add(directory / 'node_modules')
    return sorted(p.relative_to(root).as_posix() for p in search)


def dependency_inventory(root, roots):
    files, links, visited = {}, {}, set()
    pending = [root / relative_path(value) for value in roots if (root / value).exists()]
    while pending:
        directory = contained(root, pending.pop())
        if directory in visited:
            continue
        visited.add(directory)
        require(directory.is_dir(), 'Dependency root must be a directory')
        require(any(directory.iterdir()), 'Unexpected empty dependency directory: '+str(directory))
        for path in directory.iterdir():
            name = path.relative_to(root).as_posix()
            if path.is_symlink():
                target = contained(root, path)
                link = os.readlink(path)
                require(not pathlib.Path(link).is_absolute(), 'Absolute dependency alias: '+name)
                links[name] = link
                if target.is_file():
                    files[target.relative_to(root).as_posix()] = digest(target)
                else:
                    require(target.is_dir(), 'Invalid alias target: '+name)
                    pending.append(target)
            elif path.is_file():
                files[name] = digest(path)
            elif path.is_dir():
                pending.append(path)
            else:
                raise ValueError('Invalid dependency entry: '+name)
    return {'files': files, 'links': links}


def hash_map(value):
    require(isinstance(value, dict) and value, 'Empty or malformed hash inventory')
    for name, checksum in value.items():
        relative_path(name)
        require(isinstance(checksum, str) and re.fullmatch('[0-9a-f]{64}', checksum), 'Invalid SHA-256')


def verify_locked_closure(root, web, lock):
    require(isinstance(lock.get('packages'), dict), 'Missing locked production closure')
    shipped = shipped_workspaces(web)
    for name, package in lock['packages'].items():
        if 'node_modules/' in name and not package.get('dev') and not package.get('devOptional'):
            relative_path(name)
            if package.get('link') and web / relative_path(package.get('resolved', '')) not in shipped:
                # stage_dependencies.py skips a workspace link whose target the bundle does not ship
                # as a Node workspace. A static build such as apps/ui/dist ships without its alias.
                require(not (web / name).exists() and not (web / name).is_symlink(),
                        'Workspace link to an unshipped target: '+name)
                continue
            if not package.get('optional'):
                require((web / name).is_dir(), 'Locked production package omitted: '+name)
            if (web / name).exists():
                contained(root, web / name)


def verify_resources(root, manifest, arch):
    require(manifest.get('schema') == 1, 'Unsupported manifest schema')
    require(re.fullmatch('[0-9a-f]{40}', manifest.get('commit', '')), 'Invalid manifest commit')
    require(manifest.get('node_version') == 'v24.21.0', 'Unexpected Node version')
    require(re.fullmatch('[0-9a-f]{64}', manifest.get('node_sha256', '')), 'Invalid Node hash')
    web = root / WEB
    contained(root, web)
    node = root / 'MacOS/printpartner-node'
    require(contained(root, node).is_file() and os.access(node, os.X_OK), 'Installed Node missing or not executable')
    require(digest(node) == manifest['node_sha256'], 'Installed Node bytes changed')
    for key, suffix in ARTIFACTS.items():
        hash_map(manifest.get(key))
        require(measured_files(root, web / suffix) == manifest[key], 'Changed or missing '+key+' inventory')
    require('desktop-resolution.js' in manifest['backend'], 'Measured preload missing')
    hash_map(manifest.get('metadata'))
    expected_metadata = {'package.json', 'package-lock.json', 'apps/server/package.json',
                         'packages/contracts/package.json', 'packages/domain/package.json'}
    require(set(manifest['metadata']) == expected_metadata, 'Incomplete metadata inventory')
    for name, checksum in manifest['metadata'].items():
        require(digest(contained(root, web / name)) == checksum, 'Changed metadata: '+name)
    deps = manifest.get('dependencies')
    require(isinstance(deps, dict) and deps.get('web_path') == WEB, 'Invalid installed web root')
    hash_map(deps.get('files'))
    require(isinstance(deps.get('links'), dict), 'Malformed alias inventory')
    for name, target in deps['links'].items():
        relative_path(name)
        require(isinstance(target, str) and target and not pathlib.Path(target).is_absolute(), 'Invalid alias target')
    require(deps.get('roots') == dependency_roots(root, web), 'Missing or changed resolver roots')
    require(not deps.get('unshipped_links'), 'Bundle manifest lists unshipped workspace links')
    actual = dependency_inventory(root, deps['roots'])
    require(actual == {'files': deps['files'], 'links': deps['links']}, 'Changed dependency bytes or aliases')
    for name in ('contracts', 'domain'):
        alias = web / 'node_modules/@print-partner' / name
        require(alias.is_symlink() and contained(root, alias) == web / 'packages' / name,
                'Missing or incorrect workspace alias: '+name)
    verify_locked_closure(root, web, json.loads((web / 'package-lock.json').read_text()))
    release = json.loads((root / 'Resources/desktop-runtime/release.json').read_text())
    expected = {'runtime_version': manifest['runtime_version'], 'commit': manifest['commit'],
                'node': 'MacOS/printpartner-node', 'web': WEB, 'os': 'macos',
                'arch': {'arm64': 'aarch64', 'x86_64': 'x86_64'}[arch],
                'node_version': 'v24.21.0', 'node_abi': '137'}
    require(release == expected, 'Installed release identity mismatch')
    desktop = json.loads((web / 'apps/web/dist/desktop-build.json').read_text())
    require(desktop == {'mode': 'desktop', 'version': manifest['runtime_version'].removesuffix('-web'),
                        'service_worker': False}, 'Installed frontend is not the desktop build')
    return {'node_sha256': manifest['node_sha256'], 'dependency_files': len(deps['files']),
            'dependency_links': len(deps['links']), 'artifact_files': {key: len(manifest[key]) for key in ARTIFACTS}}


def version(value):
    require(isinstance(value, str) and re.fullmatch(r'\d+\.\d+(?:\.\d+)?', value), 'Malformed deployment target')
    parts = tuple(int(part) for part in value.split('.'))
    return parts + (0,) * (3 - len(parts))


def load_commands(output):
    blocks = re.split(r'(?m)^Load command \d+\s*$', output)
    commands = []
    for block in blocks[1:]:
        names = re.findall(r'(?m)^\s*cmd (LC_\w+)\s*$', block)
        require(len(names) == 1, 'Malformed otool load command')
        name = names[0]
        wanted = {'LC_BUILD_VERSION': {'platform', 'minos'}, 'LC_VERSION_MIN_MACOSX': {'version'},
                  'LC_RPATH': {'path'}, 'LC_ID_DYLIB': {'name'}}.get(name, set())
        fields = {}
        for line in block.splitlines():
            parts = line.strip().split(maxsplit=1)
            if len(parts) == 2 and parts[0] in wanted:
                require(parts[0] not in fields, 'Duplicate otool field')
                fields[parts[0]] = parts[1]
        commands.append((name, fields))
    require(commands, 'Missing otool load commands')
    return commands


def deployment_target(output):
    targets = []
    for command, fields in load_commands(output):
        if command == 'LC_BUILD_VERSION':
            require(fields.get('platform') in ('1', 'macos', 'MACOS'), 'Mach-O targets a non-macOS platform')
            targets.append(version(fields.get('minos')))
        elif command == 'LC_VERSION_MIN_MACOSX':
            targets.append(version(fields.get('version')))
        elif command.startswith('LC_VERSION_MIN_'):
            raise ValueError('Mach-O targets a non-macOS platform')
    require(len(targets) == 1, 'Missing or ambiguous macOS deployment target')
    require((10, 0, 0) <= targets[0] <= FLOOR, 'Mach-O deployment target exceeds macOS 13.5')
    return '.'.join(map(str, targets[0]))


def architecture(output, expected):
    require(output.split() == [expected], 'Incorrect Mach-O architecture: '+output)


def bundle_info(info):
    require(info.get('LSMinimumSystemVersion') == '13.5', 'Info.plist must promise macOS 13.5')
    require(info.get('CFBundleIdentifier') == 'com.poitee.printpartner', 'Bundle identifier changed')
    executable = relative_path(info.get('CFBundleExecutable'))
    require('/' not in executable, 'Main executable must be in MacOS')
    return executable


def linked_libraries(output):
    lines = output.splitlines()
    require(lines and lines[0].endswith(':'), 'Malformed otool library header')
    result = []
    for line in lines[1:]:
        match = re.fullmatch(r'\s+(.+) \(compatibility version [^,]+, current version [^)]+\)', line)
        require(match is not None, 'Malformed otool library record')
        result.append(match[1])
    return result


def command(*args, **kwargs):
    return subprocess.check_output(args, text=True, **kwargs).strip()


def run_bundle_probe(argv, *, cwd, env, timeout_seconds):  # noqa: C901
    process = subprocess.Popen(
        argv,
        cwd=cwd,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        start_new_session=True,
    )
    selector = selectors.DefaultSelector()
    captures = {
        'stdout': {'bytes': bytearray(), 'total': 0},
        'stderr': {'bytes': bytearray(), 'total': 0},
    }
    selector.register(process.stdout, selectors.EVENT_READ, 'stdout')
    selector.register(process.stderr, selectors.EVENT_READ, 'stderr')

    def drain_until(deadline):
        while process.poll() is None or selector.get_map():
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return False
            if not selector.get_map():
                try:
                    process.wait(timeout=remaining)
                except subprocess.TimeoutExpired:
                    return False
                continue
            events = selector.select(remaining)
            if not events:
                return False
            for key, _ in events:
                chunk = os.read(key.fd, 65536)
                if not chunk:
                    selector.unregister(key.fileobj)
                    key.fileobj.close()
                    continue
                capture = captures[key.data]
                capture['total'] += len(chunk)
                available = MAX_PROBE_STREAM_BYTES - len(capture['bytes'])
                if available > 0:
                    capture['bytes'].extend(chunk[:available])
        return True

    def process_group_gone(deadline):
        while True:
            try:
                os.killpg(process.pid, 0)
            except ProcessLookupError:
                return True
            except PermissionError:
                pass
            if time.monotonic() >= deadline:
                return False
            time.sleep(0.01)

    deadline = time.monotonic() + timeout_seconds
    completed = drain_until(deadline)
    if completed:
        completed = process_group_gone(deadline)
    timed_out = not completed
    cleanup_timed_out = False
    try:
        if timed_out:
            cleanup_deadline = time.monotonic() + PROBE_CLEANUP_SECONDS
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            graceful_deadline = cleanup_deadline - PROBE_FORCE_CLEANUP_SECONDS
            cleanup_complete = drain_until(graceful_deadline)
            if cleanup_complete:
                cleanup_complete = process_group_gone(graceful_deadline)
            if not cleanup_complete:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                cleanup_complete = drain_until(cleanup_deadline)
                if cleanup_complete:
                    cleanup_complete = process_group_gone(cleanup_deadline)
            cleanup_timed_out = not cleanup_complete
        else:
            process.wait()
    finally:
        selector.close()
        for stream in (process.stdout, process.stderr):
            if not stream.closed:
                stream.close()
    stdout = captures['stdout']['bytes'].decode('utf-8', errors='replace')
    stderr = captures['stderr']['bytes'].decode('utf-8', errors='replace')
    return ProbeCapture(
        timed_out=timed_out,
        cleanup_timed_out=cleanup_timed_out,
        exit_code=process.returncode,
        stdout=stdout,
        stderr=stderr,
        stdout_truncated=captures['stdout']['total'] > MAX_PROBE_STREAM_BYTES,
        stderr_truncated=captures['stderr']['total'] > MAX_PROBE_STREAM_BYTES,
    )


def decode_bundle_probe(capture):
    require(not capture.timed_out, 'Installed runtime probe timed out')
    require(not capture.cleanup_timed_out, 'Installed runtime probe cleanup timed out')
    require(not capture.stdout_truncated and not capture.stderr_truncated,
            'Installed runtime probe output exceeded 4096 bytes')
    require(capture.exit_code == 0, 'Installed runtime probe failed')
    try:
        return json.loads(capture.stdout)
    except json.JSONDecodeError as error:
        raise ValueError('Installed runtime probe returned malformed JSON') from error


def validate_bundle_probe(capture, expected):
    result = decode_bundle_probe(capture)
    require(result == expected, 'Installed runtime identity mismatch')
    return result


def bundle_probe_environment(environment):
    for key in ('LD_PRELOAD', 'DYLD_INSERT_LIBRARIES'):
        require(key not in environment, key+' is forbidden during bundle verification')
    return {key: value for key, value in environment.items() if key not in ('NODE_OPTIONS', 'NODE_PATH')}


def verify_macho(root, arch, executable, receipt):  # noqa: C901
    records, sysroot = {}, set()
    receipt['mach_o'] = records
    receipt['sysroot_libraries'] = []
    main = root / 'MacOS' / executable
    require(main.is_file() and os.access(main, os.X_OK), 'Main executable missing')
    candidates = [p for p in root.rglob('*') if p.is_file() and not p.is_symlink()]
    outputs = {}
    for path in candidates:
        description = command('/usr/bin/file', '-b', str(path))
        if 'Mach-O' not in description:
            require(path.suffix not in ('.node', '.dylib') and path not in (main, root / 'MacOS/printpartner-node'),
                    'Expected native file is not Mach-O: '+str(path))
            continue
        name = path.relative_to(root).as_posix()
        observed_arch = command('/usr/bin/lipo', '-archs', str(path))
        records[name] = {'architecture': observed_arch, 'sha256': digest(path), 'file': description}
        architecture(observed_arch, arch)
        outputs[path] = command('/usr/bin/otool', '-l', str(path))
        records[name]['load_commands'] = outputs[path]
        records[name]['minimum_macos'] = deployment_target(outputs[path])
    require(main in outputs and root / 'MacOS/printpartner-node' in outputs, 'Required Mach-O executable missing')
    require(any(name.endswith('.node') for name in records), 'Installed SQLite addon missing')
    libraries = {}
    rpaths = {}
    for path, output in outputs.items():
        rpaths[path] = []
        identities = []
        for cmd, fields in load_commands(output):
            if cmd in ('LC_RPATH', 'LC_ID_DYLIB'):
                match = re.fullmatch(r'(.+) \(offset \d+\)', fields.get('path' if cmd == 'LC_RPATH' else 'name', ''))
                require(match is not None, 'Malformed '+cmd)
                (rpaths[path] if cmd == 'LC_RPATH' else identities).append(match[1])
        require(len(identities) <= 1, 'Ambiguous dylib identity')
        libraries[path] = linked_libraries(command('/usr/bin/otool', '-L', str(path)))
        if identities:
            require(libraries[path] and libraries[path][0] == identities[0], 'Dylib identity mismatch')
            libraries[path] = libraries[path][1:]
        records[path.relative_to(root).as_posix()]['libraries'] = libraries[path]
        records[path.relative_to(root).as_posix()]['loader_contexts'] = []

    def system_library(path):
        value = str(path)
        return ((value in ('/usr/lib', '/System/Library') or value.startswith(('/usr/lib/', '/System/Library/')))
                and '\\' not in value
                and all(part not in ('', '.', '..') for part in value[1:].split('/')))

    def expand(value, loader, loading_executable):
        if value == '@loader_path':
            return loader.parent
        if value == '@executable_path':
            return loading_executable.parent
        if value.startswith('@loader_path/'):
            return loader.parent / value.removeprefix('@loader_path/')
        if value.startswith('@executable_path/'):
            return loading_executable.parent / value.removeprefix('@executable_path/')
        require(value.startswith('/'), 'Unresolved dyld path: '+value)
        require('\\' not in value and all(part not in ('', '.', '..') for part in value[1:].split('/')),
                'Noncanonical dyld path: '+value)
        return pathlib.Path(value)

    checked = set()
    reached = set()

    def visit(image, loading_executable, ancestors):
        search = []
        for owner in (image, *reversed(ancestors)):
            for value in rpaths[owner]:
                base = expand(value, owner, loading_executable)
                if not system_library(base):
                    base = base.resolve()
                    require(base.is_relative_to(root), 'LC_RPATH escapes Contents')
                if base not in search:
                    search.append(base)
        key = (image, loading_executable, tuple(search))
        if key in checked:
            return
        checked.add(key)
        reached.add(image)
        context = {'executable': loading_executable.relative_to(root).as_posix(),
                   'loader_chain': [p.relative_to(root).as_posix() for p in (*ancestors, image)],
                   'rpaths': [str(p) for p in search], 'resolved_libraries': {}}
        records[image.relative_to(root).as_posix()]['loader_contexts'].append(context)
        for library in libraries[image]:
            if library.startswith('@rpath/'):
                suffix = relative_path(library.removeprefix('@rpath/'))
                choices = [base / suffix for base in search]
            else:
                choices = [expand(library, image, loading_executable)]
            target = None
            for choice in choices:
                if system_library(choice):
                    sysroot.add(str(choice))
                    receipt['sysroot_libraries'] = sorted(sysroot)
                    target = choice
                    break
                require(choice.is_relative_to(root), 'Native dependency escapes Contents: '+library)
                resolved = choice.resolve()
                require(resolved.is_relative_to(root), 'Native dependency escapes Contents: '+library)
                if resolved in outputs:
                    target = resolved
                    break
                require(not resolved.exists(), 'Native dependency is not Mach-O: '+library)
            require(target is not None, 'Unbundled native dependency in '+str(loading_executable)+': '+library)
            context['resolved_libraries'][library] = str(target)
            if target in outputs and target not in ancestors and target != image:
                visit(target, loading_executable, (*ancestors, image))

    node = root / 'MacOS/printpartner-node'
    visit(main, main, ())
    visit(node, node, ())
    for addon in outputs:
        if addon.suffix == '.node':
            visit(addon, node, (node,))
    for image in outputs:
        if image not in reached:
            visit(image, node, (node,))
    return {'mach_o': records, 'sysroot_libraries': sorted(sysroot)}


def verify(app, manifest_path, arch, receipt):
    require(sys.platform == 'darwin', 'Real bundle verification requires macOS; use --self-test on Linux')
    root = (app / 'Contents').resolve(strict=True)
    with (root / 'Info.plist').open('rb') as stream:
        info = plistlib.load(stream)
    receipt['info_plist'] = {key: info.get(key) for key in
                            ('LSMinimumSystemVersion', 'CFBundleIdentifier', 'CFBundleExecutable')}
    executable = bundle_info(info)
    manifest = json.loads(manifest_path.read_text())
    receipt['manifest_sha256'] = digest(manifest_path)
    for path in root.rglob('*'):
        if path.is_symlink():
            require(not pathlib.Path(os.readlink(path)).is_absolute(), 'Absolute installed alias')
            contained(root, path)
    resources = verify_resources(root, manifest, arch)
    receipt['resources'] = resources
    native = verify_macho(root, arch, executable, receipt)
    measured_native = set(manifest['dependencies']['files']) | {'MacOS/printpartner-node', 'MacOS/'+executable}
    require(set(native['mach_o']).issubset(measured_native), 'Unmeasured native code in app')
    env = bundle_probe_environment(os.environ)
    receipt['bundle_probe_environment'] = {
        'forbidden': ['DYLD_INSERT_LIBRARIES', 'LD_PRELOAD'],
        'removed': ['NODE_OPTIONS', 'NODE_PATH'],
    }
    program = "const D=require('better-sqlite3');const db=new D(':memory:');const sqliteStatement=db.prepare('SELECT 42 answer');const answer=sqliteStatement.get().answer;if(answer!==42)throw Error('SQLite failed');const {createCanvas}=require('@napi-rs/canvas');const canvas=createCanvas(2,2);const ctx=canvas.getContext('2d');ctx.fillStyle='#123456';ctx.fillRect(0,0,2,2);const pixel=Array.from(ctx.getImageData(0,0,1,1).data);const png=canvas.toBuffer('image/png');if(JSON.stringify(pixel)!=='[18,52,86,255]'||png.subarray(0,8).toString('hex')!=='89504e470d0a1a0a')throw Error('Canvas failed');console.log(JSON.stringify({version:process.version,abi:process.versions.modules,os:process.platform,arch:process.arch,sqlite_answer:answer,canvas_pixel:pixel,canvas_png_signature:png.subarray(0,8).toString('hex'),native_addons:Object.keys(require.cache).filter(p=>p.endsWith('.node')).map(p=>require('node:fs').realpathSync(p)).sort()}));"
    node_arch = {'arm64': 'arm64', 'x86_64': 'x64'}[arch]
    expected_addons = sorted(str(contained(root, root / WEB / name)) for name in [
        better_sqlite3_addon_path(arch),
        f'node_modules/@napi-rs/canvas-darwin-{node_arch}/skia.darwin-{node_arch}.node'])
    capture = run_bundle_probe([str(root / 'MacOS/printpartner-node'), '--no-global-search-paths',
        '--import', str(root / WEB / 'apps/server/dist/current/desktop-resolution.js'), '-e', program,
        'bundle-probe', '--pp-desktop-package-root='+str(root)], cwd=root / WEB, env=env,
        timeout_seconds=30)
    receipt['bundle_probe'] = asdict(capture)
    result = validate_bundle_probe(capture, {
        'version': 'v24.21.0', 'abi': '137', 'os': 'darwin',
        'arch': {'arm64': 'arm64', 'x86_64': 'x64'}[arch], 'sqlite_answer': 42,
        'canvas_pixel': [18, 52, 86, 255], 'canvas_png_signature': '89504e470d0a1a0a',
        'native_addons': expected_addons,
    })
    require(digest(root / 'MacOS/printpartner-node') == manifest['node_sha256'], 'Node changed during probe')
    return {'proof_class': 'unsigned_macos_bundle', 'app': str(app.resolve()), 'architecture': arch,
            'manifest_sha256': digest(manifest_path), 'minimum_macos': '13.5', 'resources': resources,
            **native, 'installed_runtime': result, 'signed_release': False, 'native_launch': False, 'rendered_ui': False}


class ParserTests(unittest.TestCase):
    def locked_closure(self, link_ui, ui_dist=False):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = pathlib.Path(temporary.name).resolve()
        web = root / WEB
        (web / 'packages/contracts').mkdir(parents=True)
        if ui_dist:
            (web / 'apps/ui/dist').mkdir(parents=True)
            (web / 'apps/ui/dist/index.html').write_text('<!doctype html>')
        scope = web / 'node_modules/@print-partner'
        scope.mkdir(parents=True)
        (scope / 'contracts').symlink_to('../../packages/contracts')
        if link_ui:
            (scope / 'ui').symlink_to('../../apps/ui')
        lock = {'packages': {
            'node_modules/@print-partner/contracts': {'resolved': 'packages/contracts', 'link': True},
            'node_modules/@print-partner/ui': {'resolved': 'apps/ui', 'link': True}}}
        return root, web, lock

    def test_locked_closure_accepts_skipped_workspace_link(self):
        verify_locked_closure(*self.locked_closure(link_ui=False))

    def test_locked_closure_rejects_link_to_unshipped_workspace(self):
        with self.assertRaisesRegex(ValueError, 'Workspace link to an unshipped target: node_modules/@print-partner/ui'):
            verify_locked_closure(*self.locked_closure(link_ui=True))

    def test_locked_closure_accepts_static_ui_build_without_alias(self):
        verify_locked_closure(*self.locked_closure(link_ui=False, ui_dist=True))

    def test_locked_closure_rejects_alias_to_static_ui_build(self):
        with self.assertRaisesRegex(ValueError, 'Workspace link to an unshipped target: node_modules/@print-partner/ui'):
            verify_locked_closure(*self.locked_closure(link_ui=True, ui_dist=True))

    def test_locked_closure_requires_shipped_workspace_link(self):
        root, web, lock = self.locked_closure(link_ui=False)
        (web / 'node_modules/@print-partner/contracts').unlink()
        with self.assertRaisesRegex(ValueError, 'Locked production package omitted: node_modules/@print-partner/contracts'):
            verify_locked_closure(root, web, lock)
    def test_bundle_probe_signals_exited_leader_group_promptly(self):
        with tempfile.TemporaryDirectory() as temporary:
            heartbeat = pathlib.Path(temporary) / 'heartbeat'
            script = (
                'import os,sys,time\n'
                'heartbeat=sys.argv[1]\n'
                'time.sleep(0.75)\n'  # Startup deliberately exceeds the probe deadline.
                'parent_pid=os.getpid()\n'
                'pid=os.fork()\n'
                'if pid:\n'
                ' os.write(1,f"{os.getpid()} {pid}\\n".encode())\n'
                ' os._exit(0)\n'
                'while os.getppid()==parent_pid:\n'
                ' time.sleep(0.001)\n'
                'os.close(1)\n'
                'os.close(2)\n'
                'fd=os.open(heartbeat,os.O_WRONLY|os.O_CREAT|os.O_TRUNC,0o600)\n'
                'end=time.monotonic()+3\n'
                'while time.monotonic()<end:\n'
                ' os.write(fd,f"{time.monotonic()}\\n".encode())\n'
                ' time.sleep(0.01)\n'
                'os.close(fd)\n'
                'os._exit(0)\n'
            )
            popen = subprocess.Popen
            started = None

            def ready_popen(*args, **kwargs):
                nonlocal started
                process = popen(*args, **kwargs)
                startup_deadline = time.monotonic() + 10
                while not (heartbeat.exists() and heartbeat.stat().st_size):
                    if time.monotonic() >= startup_deadline:
                        try:
                            os.killpg(process.pid, signal.SIGKILL)
                        except ProcessLookupError:
                            pass
                        stdout, stderr = process.communicate(timeout=1)
                        self.fail(f'Heartbeat fixture did not start: {stdout!r} {stderr!r}')
                    time.sleep(0.01)
                started = time.monotonic()
                return process

            group_pid = None
            descendant_pid = None
            cleanup_complete = False
            try:
                with patch.object(subprocess, 'Popen', side_effect=ready_popen):
                    capture = run_bundle_probe(
                        [sys.executable, '-c', script, str(heartbeat)],
                        cwd=pathlib.Path.cwd(),
                        env=os.environ.copy(),
                        timeout_seconds=0.5,
                    )
                elapsed = time.monotonic() - started
                group_pid, descendant_pid = map(int, capture.stdout.split())
                heartbeats = [float(value) for value in heartbeat.read_text().splitlines()]
                try:
                    os.killpg(group_pid, 0)
                    group_alive = True
                except ProcessLookupError:
                    group_alive = False
                try:
                    os.kill(descendant_pid, 0)
                    descendant_alive = True
                except ProcessLookupError:
                    descendant_alive = False
            finally:
                if group_pid is not None:
                    try:
                        os.killpg(group_pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                if descendant_pid is not None:
                    cleanup_deadline = time.monotonic() + 1
                    while time.monotonic() < cleanup_deadline:
                        try:
                            os.kill(descendant_pid, 0)
                        except ProcessLookupError:
                            cleanup_complete = True
                            break
                        time.sleep(0.01)

        self.assertTrue(cleanup_complete)
        self.assertGreater(heartbeats[-1] - started, 0.2)
        self.assertLess(heartbeats[-1] - started, 1.05)
        self.assertTrue(capture.timed_out)
        self.assertFalse(capture.cleanup_timed_out)
        self.assertEqual(capture.exit_code, 0)
        self.assertFalse(group_alive)
        self.assertFalse(descendant_alive)
        self.assertLess(elapsed, 1.7)

    def test_bundle_probe_deadline_includes_inherited_pipe_lifetime(self):
        script = (
            'printf "{}\\n"\n'
            'sleep 5 &\n'
        )
        started = time.monotonic()
        capture = run_bundle_probe(
            ['/bin/sh', '-c', script],
            cwd=pathlib.Path.cwd(),
            env=os.environ.copy(),
            timeout_seconds=0.5,
        )
        elapsed = time.monotonic() - started

        self.assertTrue(capture.timed_out)
        self.assertFalse(capture.cleanup_timed_out)
        self.assertEqual(capture.exit_code, 0)
        self.assertEqual(capture.stdout, '{}\n')
        self.assertEqual(capture.stderr, '')
        self.assertFalse(capture.stdout_truncated)
        self.assertFalse(capture.stderr_truncated)
        self.assertLess(elapsed, 1.75)

    def test_better_sqlite3_prebuild_selection_and_containment(self):
        inventory = [
            'darwin-arm64.node',
            'darwin-x64.node',
            'linux-arm64.node',
            'linux-x64.node',
            'linuxmusl-arm64.node',
            'linuxmusl-x64.node',
            'win32-arm64.node',
            'win32-x64.node',
        ]
        for architecture, selected in [('arm64', 'darwin-arm64.node'),
                                       ('x86_64', 'darwin-x64.node')]:
            selection = select_better_sqlite3_prebuild(inventory, architecture)
            self.assertEqual(selection.selected_name, selected)
            self.assertEqual(selection.discard_names,
                             tuple(sorted(name for name in inventory if name != selected)))
            with self.assertRaises(ValueError):
                select_better_sqlite3_prebuild(
                    [name for name in inventory if name != selected],
                    architecture,
                )
        with self.assertRaises(ValueError):
            select_better_sqlite3_prebuild([*inventory, 'darwin-universal.node'], 'arm64')
        with self.assertRaises(ValueError):
            select_better_sqlite3_prebuild(inventory, 'unsupported')

        with tempfile.TemporaryDirectory() as temporary:
            storage = pathlib.Path(temporary)
            real_temporary = storage / 'real'
            real_temporary.mkdir()
            temporary = storage / 'alias'
            temporary.symlink_to(real_temporary, target_is_directory=True)
            fixture_root = temporary / 'installed/Contents'
            root = fixture_root.resolve()
            self.assertNotEqual(fixture_root, root)
            framework = root / 'Frameworks/fixture.node'
            framework.parent.mkdir(parents=True)
            framework.write_bytes(b'fixture')
            alias = root / WEB / better_sqlite3_addon_path('arm64')
            alias.parent.mkdir(parents=True)
            alias.symlink_to(os.path.relpath(framework, alias.parent))
            self.assertEqual(contained(root, alias), framework.resolve())
            outside = real_temporary / 'outside.node'
            outside.write_bytes(b'outside')
            escape = alias.with_name('escape.node')
            escape.symlink_to(os.path.relpath(outside, escape.parent))
            with self.assertRaisesRegex(ValueError, 'Resource escapes Contents'):
                contained(root, escape)
            escape.unlink()
            relocated = (temporary / 'relocated app/Contents').resolve()
            shutil.copytree(root, relocated, symlinks=True)
            relocated_alias = relocated / WEB / better_sqlite3_addon_path('arm64')
            self.assertEqual(contained(relocated, relocated_alias),
                             (relocated / 'Frameworks/fixture.node').resolve())

    def test_bundle_probe_capture_preserves_failure(self):
        script = ('import sys;sys.stdout.write("x"*5000);sys.stderr.write("y"*5000);'
                  'sys.stdout.flush();sys.stderr.flush();sys.exit(7)')
        capture = run_bundle_probe(
            [sys.executable, '-c', script],
            cwd=pathlib.Path.cwd(),
            env=os.environ.copy(),
            timeout_seconds=5,
        )
        self.assertFalse(capture.timed_out)
        self.assertEqual(capture.exit_code, 7)
        self.assertEqual(len(capture.stdout.encode()), MAX_PROBE_STREAM_BYTES)
        self.assertEqual(len(capture.stderr.encode()), MAX_PROBE_STREAM_BYTES)
        self.assertTrue(capture.stdout_truncated)
        self.assertTrue(capture.stderr_truncated)
        with self.assertRaisesRegex(ValueError, 'output exceeded'):
            decode_bundle_probe(capture)

        nonzero = run_bundle_probe(
            [sys.executable, '-c', 'import sys;print("{}");sys.exit(7)'],
            cwd=pathlib.Path.cwd(),
            env=os.environ.copy(),
            timeout_seconds=5,
        )
        self.assertFalse(nonzero.timed_out)
        self.assertEqual(nonzero.exit_code, 7)
        with self.assertRaisesRegex(ValueError, 'probe failed'):
            decode_bundle_probe(nonzero)

        malformed = run_bundle_probe(
            [sys.executable, '-c', 'print("not-json")'],
            cwd=pathlib.Path.cwd(),
            env=os.environ.copy(),
            timeout_seconds=5,
        )
        self.assertFalse(malformed.timed_out)
        self.assertEqual(malformed.exit_code, 0)
        with self.assertRaisesRegex(ValueError, 'malformed JSON'):
            decode_bundle_probe(malformed)

        valid = ProbeCapture(
            timed_out=False,
            cleanup_timed_out=False,
            exit_code=0,
            stdout='{"answer": 42}\n',
            stderr='',
            stdout_truncated=False,
            stderr_truncated=False,
        )
        self.assertEqual(validate_bundle_probe(valid, {'answer': 42}), {'answer': 42})
        with self.assertRaisesRegex(ValueError, 'identity mismatch'):
            validate_bundle_probe(valid, {'answer': 41})

        timed_out = run_bundle_probe(
            [sys.executable, '-c', 'import time;time.sleep(10)'],
            cwd=pathlib.Path.cwd(),
            env=os.environ.copy(),
            timeout_seconds=0.05,
        )
        self.assertTrue(timed_out.timed_out)
        with self.assertRaisesRegex(ValueError, 'timed out'):
            decode_bundle_probe(timed_out)
        with self.assertRaisesRegex(ValueError, 'LD_PRELOAD'):
            bundle_probe_environment({'LD_PRELOAD': '/tmp/injected.dylib'})
        self.assertEqual(bundle_probe_environment({'NODE_OPTIONS': 'forbidden', 'SAFE': '1'}),
                         {'SAFE': '1'})

    def test_system_install_names_preserve_loader_authority(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary).resolve()
            main = root / 'MacOS/pp-desktop'
            node = root / 'MacOS/printpartner-node'
            addon = root / 'Frameworks/addon.node'
            for image in (main, node, addon):
                image.parent.mkdir(parents=True, exist_ok=True)
                image.write_bytes(image.name.encode())
                image.chmod(0o755)
            framework = '/System/Library/Frameworks/WebKit.framework/Versions/A/WebKit'
            dependencies = {main: framework}
            paths = {}
            base = 'binary:\nLoad command 0\n cmd LC_BUILD_VERSION\n platform 1\n minos 13.5\n'

            def tool(*args, **kwargs):
                image = pathlib.Path(args[-1])
                if args[0] == '/usr/bin/file':
                    return 'Mach-O 64-bit arm64'
                if args[0] == '/usr/bin/lipo':
                    return 'arm64'
                if args[1] == '-l':
                    value = paths.get(image)
                    return base + ('Load command 1\n cmd LC_RPATH\n path '+value+' (offset 12)\n' if value else '')
                if args[1] == '-L':
                    value = dependencies.get(image, '/usr/lib/libSystem.B.dylib')
                    return str(image)+':\n\t'+value+' (compatibility version 1.0.0, current version 1.0.0)'
                raise AssertionError(args)

            original_resolve = pathlib.Path.resolve

            def resolve(path, *args, **kwargs):
                if str(path) == framework:
                    return pathlib.Path('/outside/synthetic-system-indirection/WebKit')
                return original_resolve(path, *args, **kwargs)

            with patch(__name__+'.command', side_effect=tool), patch.object(pathlib.Path, 'resolve', resolve):
                report = verify_macho(root, 'arm64', 'pp-desktop', {})
                self.assertIn(framework, report['sysroot_libraries'])
                self.assertEqual(report['mach_o']['MacOS/pp-desktop']['loader_contexts'][0]
                                 ['resolved_libraries'][framework], framework)
                with patch.object(pathlib.Path, 'exists', side_effect=AssertionError('Cache-only name touched disk')):
                    verify_macho(root, 'arm64', 'pp-desktop', {})
                paths[main] = '/System/Library/Frameworks/WebKit.framework/Versions/A'
                dependencies[main] = '@rpath/WebKit'
                verify_macho(root, 'arm64', 'pp-desktop', {})
                paths[main] = '/usr/lib'
                dependencies[main] = '@rpath/libSystem.B.dylib'
                verify_macho(root, 'arm64', 'pp-desktop', {})
                paths.clear()
                for library in ['/usr/local/lib/libcustom.dylib', '/System/Library-lookalike/libcustom.dylib',
                                '/Library/Frameworks/Custom.framework/Custom', '/private/tmp/libcustom.dylib',
                                '/Users/fixture/libcustom.dylib', '/System/Volumes/unproven/libcustom.dylib',
                                '/System/Library/../../tmp/libcustom.dylib', '/usr/lib/../../tmp/libcustom.dylib',
                                '/System//Library/Frameworks/WebKit.framework/Versions/A/WebKit',
                                '/System/Library/./Frameworks/WebKit.framework/Versions/A/WebKit',
                                '//System/Library/Frameworks/WebKit.framework/Versions/A/WebKit',
                                '/usr/lib\\libcustom.dylib', '@unknown/libcustom.dylib', 'libcustom.dylib',
                                '@rpath/libcustom.dylib']:
                    dependencies[main] = library
                    with self.subTest(library=library), self.assertRaises(ValueError):
                        verify_macho(root, 'arm64', 'pp-desktop', {})
                dependencies[main] = '@rpath/WebKit'
                for rpath in ['/usr/local/lib', '/System/Library-lookalike', '/private/tmp',
                              '/System/Library/../../tmp', '/System//Library/Frameworks']:
                    paths[main] = rpath
                    with self.subTest(rpath=rpath), self.assertRaises(ValueError):
                        verify_macho(root, 'arm64', 'pp-desktop', {})
                paths[main] = '/System/Library/Frameworks/WebKit.framework/Versions/A'
                dependencies[main] = '@rpath/../../../../../tmp/libcustom.dylib'
                with self.assertRaises(ValueError):
                    verify_macho(root, 'arm64', 'pp-desktop', {})
                paths.clear()
                dependencies[main] = framework
                alias = root / 'Frameworks/user-alias.dylib'
                alias.symlink_to('/usr/lib/libSystem.B.dylib')
                dependencies[addon] = '@loader_path/user-alias.dylib'
                with self.assertRaisesRegex(ValueError, 'Native dependency escapes Contents'):
                    verify_macho(root, 'arm64', 'pp-desktop', {})

    def test_node_addon_uses_node_executable_and_loader_chain(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary).resolve()
            main = root / 'MacOS/pp-desktop'
            node = root / 'MacOS/printpartner-node'
            addon = root / 'Frameworks/addon.node'
            library = root / 'Frameworks/libfixture.dylib'
            child = root / 'Frameworks/nested/libchild.dylib'
            for path in (main, node, addon, library):
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(path.name.encode())
                path.chmod(0o755)
            base = 'binary:\nLoad command 0\n cmd LC_BUILD_VERSION\n platform 1\n minos 13.5\n'
            paths = {main: '@executable_path/../Frameworks'}
            dependencies = {addon: '@rpath/libfixture.dylib'}
            identities = {}

            def tool(*args, **kwargs):
                image = pathlib.Path(args[-1])
                if args[0] == '/usr/bin/file':
                    return 'Mach-O 64-bit arm64'
                if args[0] == '/usr/bin/lipo':
                    return 'arm64'
                if args[1] == '-l':
                    value = paths.get(image)
                    identity = identities.get(image)
                    return (base + ('Load command 1\n cmd LC_RPATH\n path '+value+' (offset 12)\n' if value else '')
                            + ('Load command 2\n cmd LC_ID_DYLIB\n name '+identity+' (offset 24)\n' if identity else ''))
                if args[1] == '-L':
                    value = dependencies.get(image, '/usr/lib/libSystem.B.dylib')
                    values = ([identities[image]] if image in identities else []) + [value]
                    return str(image)+':\n'+ '\n'.join('\t'+v+' (compatibility version 1.0.0, current version 1.0.0)' for v in values)
                raise AssertionError(args)

            with patch(__name__+'.command', side_effect=tool):
                with self.assertRaisesRegex(ValueError, r'printpartner-node: @rpath/libfixture'):
                    verify_macho(root, 'arm64', 'pp-desktop', {})
                paths[node] = '@executable_path/../Frameworks'
                valid = verify_macho(root, 'arm64', 'pp-desktop', {})
                context = valid['mach_o']['Frameworks/addon.node']['loader_contexts'][0]
                self.assertEqual(context['executable'], 'MacOS/printpartner-node')
                self.assertNotIn('MacOS/pp-desktop', context['loader_chain'])
                paths.pop(node)
                paths[addon] = '@loader_path'
                verify_macho(root, 'arm64', 'pp-desktop', {})
                child.parent.mkdir()
                child.write_bytes(b'child library')
                dependencies[library] = '@rpath/libchild.dylib'
                paths[library] = '@loader_path/nested'
                dependencies[child] = '@rpath/libfixture.dylib'
                chained = verify_macho(root, 'arm64', 'pp-desktop', {})
                child_context = chained['mach_o']['Frameworks/nested/libchild.dylib']['loader_contexts'][0]
                self.assertEqual(child_context['loader_chain'], ['MacOS/printpartner-node', 'Frameworks/addon.node',
                                                                'Frameworks/libfixture.dylib', 'Frameworks/nested/libchild.dylib'])
                paths.pop(library)
                with self.assertRaisesRegex(ValueError, r'printpartner-node: @rpath/libchild'):
                    verify_macho(root, 'arm64', 'pp-desktop', {})
                dependencies[library] = '@loader_path/nested/libchild.dylib'
                verify_macho(root, 'arm64', 'pp-desktop', {})
                dependencies[child] = '/usr/lib/libSystem.B.dylib'
                paths.pop(addon)
                dependencies[addon] = '@executable_path/../Frameworks/libfixture.dylib'
                verify_macho(root, 'arm64', 'pp-desktop', {})
                identities[library] = '@rpath/libfixture.dylib'
                verify_macho(root, 'arm64', 'pp-desktop', {})

    def test_architecture_and_plist_controls(self):
        architecture('arm64', 'arm64')
        architecture('x86_64', 'x86_64')
        for output in ['', 'x86_64', 'arm64 x86_64', 'arm64e']:
            with self.subTest(output=output), self.assertRaises(ValueError):
                architecture(output, 'arm64')
        info = {'LSMinimumSystemVersion': '13.5', 'CFBundleIdentifier': 'com.poitee.printpartner',
                'CFBundleExecutable': 'pp-desktop'}
        self.assertEqual(bundle_info(info), 'pp-desktop')
        for key, value in [('LSMinimumSystemVersion', '14.0'), ('LSMinimumSystemVersion', None),
                           ('CFBundleIdentifier', 'wrong'), ('CFBundleExecutable', '../escape')]:
            with self.subTest(key=key, value=value), self.assertRaises(ValueError):
                bundle_info({**info, key: value})

    def test_deployment_controls(self):
        modern = 'binary:\nLoad command 0\n      cmd LC_BUILD_VERSION\n  cmdsize 32\n platform 1\n    minos 13.5\n      sdk 26.0\n   ntools 0\n'
        legacy = 'binary:\nLoad command 0\n cmd LC_VERSION_MIN_MACOSX\n cmdsize 16\n version 11.0\n sdk 26.0\n'
        self.assertEqual(deployment_target(modern), '13.5.0')
        self.assertEqual(deployment_target(legacy), '11.0.0')
        sections = 'binary:\nLoad command 0\n cmd LC_SEGMENT_64\n sectname __text\n segname __TEXT\n sectname __stubs\n segname __TEXT\n'
        self.assertEqual(deployment_target(sections + modern.replace('Load command 0', 'Load command 1')), '13.5.0')
        for bad in ['', modern.replace('13.5', '14.0'), modern.replace('platform 1', 'platform 2'),
                    modern.replace('minos 13.5', 'minos unknown'), modern.replace('minos 13.5', ''),
                    modern + legacy, legacy.replace('LC_VERSION_MIN_MACOSX', 'LC_VERSION_MIN_IPHONEOS'),
                    modern.replace('sdk 26.0', 'minos 13.0')]:
            with self.subTest(output=bad), self.assertRaises(ValueError):
                deployment_target(bad)

    def test_library_controls(self):
        output = 'binary:\n\t/usr/lib/libSystem.B.dylib (compatibility version 1.0.0, current version 1351.0.0)'
        self.assertEqual(linked_libraries(output), ['/usr/lib/libSystem.B.dylib'])
        for bad in ['', 'binary\n', 'binary:\n bad library', output+'\nmalformed']:
            with self.subTest(output=bad), self.assertRaises(ValueError):
                linked_libraries(bad)

    def completed_stage_fixture(self, root):
        web = root / WEB
        node = root / 'MacOS/printpartner-node'
        node.parent.mkdir(parents=True)
        node.write_text('fixture node bytes')
        node.chmod(0o755)
        for suffix in ARTIFACTS.values():
            directory = web / suffix
            directory.mkdir(parents=True)
            (directory / 'index.js').write_text('fixture artifact')
        (web / ARTIFACTS['backend'] / 'desktop-resolution.js').write_text('fixture preload')
        (web / ARTIFACTS['frontend'] / 'desktop-build.json').write_text(json.dumps(
            {'mode': 'desktop', 'version': '3.3.0', 'service_worker': False}))
        metadata = ['package.json', 'package-lock.json', 'apps/server/package.json',
                    'packages/contracts/package.json', 'packages/domain/package.json']
        source = root / 'source'
        for name in [*metadata, 'apps/web/package.json']:
            target = source / name
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(json.dumps({'version': '3.3.0'}))
        # Execute the producer's metadata-copy loop so an extra copied file
        # cannot silently diverge from the completed-stage inventory again.
        staging = ast.parse(pathlib.Path(__file__).with_name('stage-desktop.py').read_text())
        copy_loop, = [statement for statement in staging.body
                      if isinstance(statement, ast.For) and isinstance(statement.iter, ast.List)
                      and any(isinstance(item, ast.Constant) and item.value == 'package-lock.json'
                              for item in statement.iter.elts)]
        exec(compile(ast.Module(body=[copy_loop], type_ignores=[]), '<staged metadata>', 'exec'),
             {'source': source, 'web': web, 'shutil': shutil})
        shutil.rmtree(source)
        (web / 'package-lock.json').write_text(json.dumps({'packages': {
            'node_modules/better-sqlite3': {'version': 'fixture'}}}))
        addon = root / 'Frameworks/fixture.node'
        addon.parent.mkdir()
        addon.write_text('fixture addon bytes')
        alias = web / better_sqlite3_addon_path('arm64')
        alias.parent.mkdir(parents=True)
        alias.symlink_to(os.path.relpath(addon, alias.parent))
        workspaces = web / 'node_modules/@print-partner'
        workspaces.mkdir()
        for name in ('contracts', 'domain'):
            (workspaces / name).symlink_to('../../packages/'+name)
        roots = dependency_roots(root, web)
        manifest = {'schema': 1, 'commit': 'a'*40, 'node_version': 'v24.21.0',
                    'runtime_version': '3.3.0-web', 'node_sha256': digest(node),
                    'metadata': {name: digest(web / name) for name in metadata},
                    'dependencies': {'web_path': WEB, 'roots': roots, **dependency_inventory(root, roots)},
                    **{key: measured_files(root, web / suffix) for key, suffix in ARTIFACTS.items()}}
        (root / 'Resources/desktop-runtime/release.json').write_text(json.dumps({
            'runtime_version': '3.3.0-web', 'commit': 'a'*40, 'node': 'MacOS/printpartner-node',
            'web': WEB, 'os': 'macos', 'arch': 'aarch64', 'node_version': 'v24.21.0', 'node_abi': '137'}))
        self.assertGreater(verify_resources(root, manifest, 'arm64')['dependency_files'], 0)
        (root / 'bundle-manifest.json').write_text(json.dumps(manifest))
        (root / 'bundle-config.json').write_text(json.dumps({'bundle': {
            'active': True, 'resources': {}, 'macOS': {
                'minimumSystemVersion': '13.5',
                'files': {name: str(root / name) for name in
                          ['MacOS/printpartner-node', 'Frameworks/fixture.node']}}}}))
        spec = importlib.util.spec_from_file_location(
            'macos_assembly', pathlib.Path(__file__).with_name('assemble-macos-resources.py'))
        assembly = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(assembly)
        return manifest, node, addon, web, workspaces, assembly

    def test_completed_stage_accepts_symlinked_temporary_parent(self):
        with tempfile.TemporaryDirectory() as temporary:
            parent = pathlib.Path(temporary).resolve()
            private_tmp = parent / 'private/tmp'
            private_tmp.mkdir(parents=True)
            tmp = parent / 'tmp'
            tmp.symlink_to(private_tmp, target_is_directory=True)
            root = tmp / 'desktop-stage'
            root.mkdir()
            _, _, _, _, _, assembly = self.completed_stage_fixture(root.resolve(strict=True))
            config_path = root / 'bundle-config.json'
            config = json.loads(config_path.read_text())
            config['bundle']['macOS']['files'] = {
                name: str(root / name) for name in config['bundle']['macOS']['files']}
            config_path.write_text(json.dumps(config))

            self.assertNotEqual(root.absolute(), root.resolve(strict=True))
            stage = assembly.completed_stage(root)
            self.assertEqual(stage.root, root.resolve(strict=True))
            self.assertEqual(set(stage.native_files),
                             {'MacOS/printpartner-node', 'Frameworks/fixture.node'})

    def test_staged_metadata_passes_completed_stage_and_rejects_changed_or_missing_closure(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary).resolve()
            manifest, node, addon, web, workspaces, assembly = self.completed_stage_fixture(root)
            self.assertIn(WEB+'/package.json', assembly.completed_stage(root).runtime_files)
            extra = web / 'apps/web/package.json'
            extra.write_text('{}')
            with self.assertRaisesRegex(ValueError, 'Runtime tree differs'):
                assembly.completed_stage(root)
            extra.unlink()
            for key in ['frontend', 'backend', 'metadata', 'dependencies']:
                broken = json.loads(json.dumps(manifest))
                if key == 'dependencies':
                    broken[key]['files'].pop(next(iter(broken[key]['files'])))
                else:
                    broken[key].pop(next(iter(broken[key])))
                with self.subTest(omitted=key), self.assertRaises(ValueError):
                    verify_resources(root, broken, 'arm64')
            for path in [node, addon, web / ARTIFACTS['backend'] / 'index.js']:
                original = path.read_bytes()
                path.write_bytes(b'changed')
                with self.subTest(changed=str(path)), self.assertRaises(ValueError):
                    verify_resources(root, manifest, 'arm64')
                path.unlink()
                with self.subTest(missing=str(path)), self.assertRaises((ValueError, FileNotFoundError)):
                    verify_resources(root, manifest, 'arm64')
                path.write_bytes(original)
                if path == node:
                    path.chmod(0o755)
            workspace_alias = workspaces / 'domain'
            workspace_alias.unlink()
            workspace_alias.symlink_to('/etc')
            with self.assertRaises(ValueError):
                verify_resources(root, manifest, 'arm64')

    def test_inventory_controls(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary).resolve()
            package = root / 'web/node_modules/package'
            package.mkdir(parents=True)
            file = package / 'index.js'
            file.write_text('measured')
            roots = ['web/node_modules']
            expected = dependency_inventory(root, roots)
            self.assertEqual(dependency_inventory(root, roots), expected)
            file.write_text('changed')
            self.assertNotEqual(dependency_inventory(root, roots), expected)
            file.unlink()
            with self.assertRaises(ValueError):
                dependency_inventory(root, roots)
            file.write_text('measured')
            (package / 'extra.js').write_text('unmeasured')
            self.assertNotEqual(dependency_inventory(root, roots), expected)
            alias = package / 'alias'
            alias.symlink_to('/etc/passwd')
            with self.assertRaises(ValueError):
                dependency_inventory(root, roots)
            alias.unlink()
            alias.symlink_to('../../../missing')
            with self.assertRaises(FileNotFoundError):
                dependency_inventory(root, roots)
            alias.unlink()
            alias.symlink_to('index.js')
            linked = dependency_inventory(root, roots)
            alias.unlink()
            alias.symlink_to('extra.js')
            self.assertNotEqual(dependency_inventory(root, roots), linked)
        for bad in [None, {}, {'../escape': 'a'*64}, {'ok': 'invalid'}, {'/absolute': 'a'*64}]:
            with self.subTest(inventory=bad), self.assertRaises(ValueError):
                hash_map(bad)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--app', type=pathlib.Path)
    parser.add_argument('--manifest', type=pathlib.Path)
    parser.add_argument('--arch', choices=['arm64', 'x86_64'])
    parser.add_argument('--receipt', type=pathlib.Path)
    parser.add_argument('--self-test', action='store_true')
    args = parser.parse_args()
    if args.self_test:
        suite = unittest.defaultTestLoader.loadTestsFromTestCase(ParserTests)
        return 0 if unittest.TextTestRunner(verbosity=2).run(suite).wasSuccessful() else 1
    if not all((args.app, args.manifest, args.arch, args.receipt)):
        parser.error('--app, --manifest, --arch, and --receipt are required')
    receipt = {'status': 'failed', 'app': str(args.app), 'architecture': args.arch}
    try:
        receipt.update(verify(args.app, args.manifest, args.arch, receipt))
        receipt['status'] = 'passed'
    except Exception as error:
        receipt['error'] = str(error)
    args.receipt.parent.mkdir(parents=True, exist_ok=True)
    args.receipt.write_text(json.dumps(receipt, indent=2, sort_keys=True)+'\n')
    print(json.dumps(receipt, sort_keys=True))
    return 0 if receipt['status'] == 'passed' else 1


if __name__ == '__main__':
    sys.exit(main())
