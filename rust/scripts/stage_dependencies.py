import os
import shutil


def stage_locked_dependencies(source, web, lock):
    """Copy the lockfile's production packages from source into the staged web tree.

    Run this after the desktop artifacts are staged. A workspace link is staged
    only when its target is already in the stage, so a workspace the desktop
    bundle does not ship (such as apps/ui) leaves no dangling link. Returns the
    skipped link names.
    """
    skipped = []
    for name, package in lock['packages'].items():
        if 'node_modules/' not in name or package.get('dev'):
            continue
        original = source / name
        target = web / name
        if original.is_symlink():
            link = os.readlink(original)
            if package.get('link') and not os.path.exists(os.path.normpath(target.parent / link)):
                skipped.append(name)
                continue
            target.parent.mkdir(parents=True, exist_ok=True)
            target.symlink_to(link)
        elif original.is_dir() and not target.exists():
            shutil.copytree(original, target, symlinks=True, ignore=shutil.ignore_patterns('node_modules'))
        elif not original.exists() and not package.get('optional'):
            raise ValueError('Locked runtime dependency missing: '+name)
    return skipped
