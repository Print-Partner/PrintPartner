"""The web workspaces the desktop ships, shared by the manifest builder and the bundle checker."""

import json
import pathlib

# The workspace builds the desktop measures and ships. stage-desktop.py copies these paths.
ARTIFACTS = {'frontend': 'apps/web/dist', 'backend': 'apps/server/dist/current',
             'contracts': 'packages/contracts/dist/current', 'domain': 'packages/domain/dist/current'}


def shipped_workspaces(web):
    """The workspace directories under web that hold a measured build."""
    return {web.joinpath(*pathlib.PurePosixPath(path).parts[:2]) for path in ARTIFACTS.values()}


def workspace_parents(web):
    """The directories that the workspace patterns in web/package.json name, such as web/apps."""
    parents = set()
    for pattern in json.loads((web / 'package.json').read_text()).get('workspaces', []):
        parent = pattern.removesuffix('/*')
        if not pattern.endswith('/*') or '*' in parent:
            raise ValueError('Unsupported workspace pattern: ' + pattern)
        parents.add(web / parent)
    return parents
