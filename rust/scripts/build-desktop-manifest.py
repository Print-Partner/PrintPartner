import argparse
import hashlib
import json
import pathlib
import re
import os
import subprocess
import sys

parser = argparse.ArgumentParser()
parser.add_argument('--web', type=pathlib.Path, required=True)
parser.add_argument('--node', type=pathlib.Path, required=True)
parser.add_argument('--output', type=pathlib.Path, required=True)
parser.add_argument('--commit', required=True)
parser.add_argument('--package-root', type=pathlib.Path)
args = parser.parse_args()
if not re.fullmatch(r"[0-9a-f]{40}", args.commit):
    parser.error("--commit must be a full lowercase Git commit")
web = args.web.resolve(strict=True)
package_root = (args.package_root or web).resolve(strict=True)
web_path = web.relative_to(package_root).as_posix()

# The workspace builds the desktop measures and ships. stage-desktop.py copies these
# paths and check-macos-bundle.py lists the same ones.
ARTIFACTS = {'frontend': 'apps/web/dist', 'backend': 'apps/server/dist/current',
             'contracts': 'packages/contracts/dist/current', 'domain': 'packages/domain/dist/current'}
SHIPPED_WORKSPACES = {web / pathlib.Path(path).parts[0] / pathlib.Path(path).parts[1] for path in ARTIFACTS.values()}
WORKSPACE_PARENTS = {web / 'apps', web / 'packages'}

def digest(path):
    return hashlib.file_digest(path.open('rb'), 'sha256').hexdigest()

def inventory(root):
    root = root.resolve(strict=True)
    files = {}
    for path in sorted(root.rglob('*')):
        if path.is_file():
            if not path.resolve(strict=True).is_relative_to(root):
                raise ValueError('Artifact escapes its root')
            files[path.relative_to(root).as_posix()] = digest(path)
    if not files:
        raise ValueError('Empty artifact root')
    return files

def dependencies():
    roots = []
    files = {}
    links = {}
    unshipped_links = []
    pending = []
    visited = set()
    search = {web / 'node_modules', web / 'apps/node_modules', web / 'packages/node_modules'}
    ancestor = web.parent
    while ancestor.is_relative_to(package_root):
        search.add(ancestor / 'node_modules')
        if ancestor == package_root:
            break
        ancestor = ancestor.parent
    # Only shipped workspaces are resolver roots. Shipped code never resolves modules
    # from another workspace's node_modules, and a source checkout keeps tool caches
    # there, such as Vitest's apps/ui/node_modules/.vite.
    for workspace in sorted(SHIPPED_WORKSPACES):
        for directory in [workspace, workspace / 'dist', workspace / 'dist/current']:
            search.add(directory / 'node_modules')
    for directory in sorted(search):
        roots.append(directory.relative_to(package_root).as_posix())
        if not directory.exists():
            continue
        pending.append(directory)
    while pending:
        directory = pending.pop().resolve(strict=True)
        if not directory.is_relative_to(package_root):
            raise ValueError('Dependency root escapes package root')
        if directory in visited:
            continue
        visited.add(directory)
        for path in directory.iterdir():
            relative = path.relative_to(package_root).as_posix()
            if path.is_symlink():
                target = path.resolve(strict=True)
                if not target.is_relative_to(package_root):
                    raise ValueError('Dependency link escapes package root')
                link = os.readlink(path)
                if pathlib.Path(link).is_absolute():
                    raise ValueError('Dependency link must be relative')
                links[relative] = link
                if target.parent in WORKSPACE_PARENTS and target not in SHIPPED_WORKSPACES:
                    # A workspace link the desktop does not ship, such as
                    # node_modules/@print-partner/ui in a source checkout. Record the link
                    # but not its target, as stage_dependencies.py leaves it out of a stage.
                    unshipped_links.append(relative)
                elif target.is_file():
                    files[target.relative_to(package_root).as_posix()] = digest(target)
                elif target.is_dir():
                    pending.append(target)
                else:
                    raise ValueError('Dependency link target is not a file or directory')
            elif path.is_file():
                files[relative] = digest(path)
            elif path.is_dir():
                pending.append(path)
            else:
                raise ValueError('Dependency entry is not a file or directory')
    if not files or not roots:
        raise ValueError('Runtime dependencies unavailable')
    return {'web_path': web_path, 'roots': sorted(roots), 'files': files, 'links': links,
            'unshipped_links': sorted(unshipped_links)}

metadata = ['package.json', 'package-lock.json', 'apps/server/package.json',
            'packages/contracts/package.json', 'packages/domain/package.json']
frontend = web / ARTIFACTS['frontend']
desktop = json.loads((frontend / 'desktop-build.json').read_text())
if desktop['mode'] != 'desktop' or desktop['service_worker'] is not False:
    raise ValueError('Expected desktop frontend build')
version = json.loads((web / 'package.json').read_text())['version']
if desktop['version'] != version:
    raise ValueError('Frontend version mismatch')
subprocess.run([sys.executable, str(pathlib.Path(__file__).with_name('precompress-spa.py')),
                str(frontend), '--boundary', str(web)], check=True, stdout=subprocess.DEVNULL)
backend = inventory(web / ARTIFACTS['backend'])
if 'desktop-resolution.js' not in backend:
    raise ValueError('Measured desktop resolution preload missing')
manifest = {
    'schema': 1,
    'runtime_version': version + '-web',
    'commit': args.commit,
    'node_version': subprocess.check_output([str(args.node), '--version'], text=True).strip(),
    'node_sha256': digest(args.node),
    'frontend': inventory(frontend),
    'backend': backend,
    'contracts': inventory(web / ARTIFACTS['contracts']),
    'domain': inventory(web / ARTIFACTS['domain']),
    'metadata': {name: digest(web / name) for name in metadata},
    'dependencies': dependencies(),
}
encoded = (json.dumps(manifest, sort_keys=True, separators=(',', ':')) + '\n').encode()
args.output.parent.mkdir(parents=True, exist_ok=True)
args.output.write_bytes(encoded)
print(json.dumps({'manifest_sha256': hashlib.sha256(encoded).hexdigest(),
                  'measured_files': sum(len(manifest[key]) for key in ['frontend', 'backend', 'contracts', 'domain', 'metadata']) + 1,
                  'dependency_files': len(manifest['dependencies']['files']),
                  'dependency_links': len(manifest['dependencies']['links']),
                  'node_version': manifest['node_version']}))
