#!/usr/bin/env python3
"""Launch the packaged Linux app on an older database and prove it is usable.

The smoke extracts nothing and builds nothing: it takes the installed
executable from an unpacked .deb, a fixture directory from legacy_fixture.py
and an evidence directory. It starts its own Xvfb display, launches the app
with a disposable HOME, and then requires:

1. a mapped native X11 window owned by the app whose title is "Print Partner";
2. the fixture's Build name in the app's AT-SPI accessibility tree, which
   includes the WebKitGTK page content;
3. a clean exit after SIGTERM (exit code 0, which the app only returns after a
   complete core shutdown receipt) with no surviving child processes;
4. the database migrated past the fixture's stamp with every preserved Plan
   and Plate row unchanged.

With --expect-refusal it instead requires the startup error dialog and a data
directory whose entries and bytes did not change. The rebuild build refuses an
existing installation that has no .pp-rust-owned.json marker (docs/storage.md).

Run it inside "dbus-run-session" so the accessibility bus is private to the run.
"""

import argparse
import hashlib
import json
import os
import pathlib
import secrets
import shutil
import signal
import sqlite3
import subprocess
import sys
import tempfile
import time

import legacy_fixture

WINDOW_TITLE = "Print Partner"
STARTUP_ERROR_TITLE = "Print Partner could not start"
DISPLAY_SIZE = "1280x1024x24"
# The Build name must be page content (the Builds table link or cell), not only window chrome.
CONTENT_ROLES = ("link", "table cell", "cell")
PAINTED_COLORS = 64
# The first-run administrator form the rebuild build shows when the migrated
# installation has no account (web/apps/web/src/pages/LoginPage.tsx).
SETUP_FIELDS = ("Display name", "Email", "Password")
SETUP_BUTTON = "Create administrator"
CONTINUE_BUTTON = "Continue to Print Partner"
WALK_LIMIT = 50000


class SmokeFailure(Exception):
    pass


def proc_parents():
    parents = {}
    for entry in pathlib.Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        try:
            status = (entry / "status").read_text()
        except OSError:
            continue
        for line in status.splitlines():
            if line.startswith("PPid:"):
                parents.setdefault(int(line.split()[1]), []).append(int(entry.name))
                break
    return parents


def start_identity(pid):
    try:
        text = pathlib.Path(f"/proc/{pid}/stat").read_text()
        return text[text.rindex(")") + 2 :].split()[19]
    except (OSError, ValueError, IndexError):
        return None


def descendants(root):
    parents = proc_parents()
    found, pending = {}, list(parents.get(root, []))
    while pending:
        pid = pending.pop()
        if pid in found:
            continue
        found[pid] = start_identity(pid)
        pending.extend(parents.get(pid, []))
    return found


def alive(owned):
    return sorted(
        pid for pid, identity in owned.items() if identity and start_identity(pid) == identity
    )


def free_display():
    for number in range(140, 200):
        if not os.path.exists(f"/tmp/.X11-unix/X{number}") and not os.path.exists(
            f"/tmp/.X{number}-lock"
        ):
            return number
    raise SmokeFailure("no free X display")


def start_display():
    number = free_display()
    xvfb = subprocess.Popen(
        ["Xvfb", f":{number}", "-screen", "0", DISPLAY_SIZE, "-nolisten", "tcp"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    deadline = time.monotonic() + 10
    while not os.path.exists(f"/tmp/.X11-unix/X{number}"):
        if xvfb.poll() is not None or time.monotonic() > deadline:
            xvfb.kill()
            raise SmokeFailure("Xvfb did not start")
        time.sleep(0.05)
    return xvfb, f":{number}"


def x11(display, *args):
    result = subprocess.run(
        list(args),
        env={**os.environ, "DISPLAY": display},
        capture_output=True,
        text=True,
        timeout=20,
    )
    return result.stdout if result.returncode == 0 else ""


def windows(display, pids):
    """Return mapped X11 windows that belong to any of the pids."""
    found = []
    for pid in pids:
        for wid in x11(
            display, "xdotool", "search", "--all", "--onlyvisible", "--pid", str(pid), "--name", ""
        ).split():
            info = x11(display, "xwininfo", "-id", wid)
            facts = dict(line.strip().split(": ", 1) for line in info.splitlines() if ": " in line)
            found.append(
                {
                    "id": wid,
                    "pid": pid,
                    "title": x11(display, "xdotool", "getwindowname", wid).rstrip("\n"),
                    "mapState": facts.get("Map State"),
                    "width": int(facts.get("Width", 0)),
                    "height": int(facts.get("Height", 0)),
                }
            )
    return found


def accessible_text(node, name):
    parts = [name]
    try:
        text = node.get_text_iface()
        if text is not None:
            parts.append(atspi().Text.get_text(text, 0, atspi().Text.get_character_count(text)))
    except Exception:
        pass
    return " ".join(part for part in parts if part)


def atspi():
    import gi

    gi.require_version("Atspi", "2.0")
    from gi.repository import Atspi

    return Atspi


def walk(pids):
    """Yield (node, role, name, ancestor roles) for every application owned by pids."""
    desktop = atspi().get_desktop(0)
    visited = 0
    for index in range(desktop.get_child_count()):
        application = desktop.get_child_at_index(index)
        try:
            if application is None or application.get_process_id() not in pids:
                continue
        except Exception:
            continue
        pending = [(application, [])]
        while pending and visited < WALK_LIMIT:
            node, path = pending.pop()
            visited += 1
            try:
                role = node.get_role_name()
                name = node.get_name() or ""
                count = node.get_child_count()
            except Exception:
                continue
            yield node, role, name, path
            for child_index in range(count - 1, -1, -1):
                try:
                    child = node.get_child_at_index(child_index)
                except Exception:
                    child = None
                if child is not None:
                    pending.append((child, path + [role]))


def search_accessibility(pids, needle):
    """Find needle as a link or table cell inside the page, not in window chrome."""
    for node, role, name, path in walk(pids):
        if (
            role in CONTENT_ROLES
            and "document web" in path
            and needle in accessible_text(node, name)
        ):
            return {"found": True, "role": role, "path": path + [role]}
    return {"found": False}


def setup_form(pids):
    """Return the first-run administrator form's fields and button, if it is showing."""
    fields, button = {}, None
    for node, role, name, path in walk(pids):
        if "document web" not in path:
            continue
        if role in ("entry", "password text") and name in SETUP_FIELDS:
            fields[name] = node
        elif role == "push button" and name == SETUP_BUTTON:
            button = node
    return (fields, button) if button is not None and len(fields) == len(SETUP_FIELDS) else None


def press_continue(pids):
    """Press the page's Continue button; return whether it was pressed."""
    for node, role, name, path in walk(pids):
        if "document web" in path and role in ("push button", "link") and name == CONTINUE_BUTTON:
            try:
                node.get_action_iface().do_action(0)
            except Exception:
                return False
            return True
    return False


def complete_setup(display, window, form):
    """Type a disposable administrator into the first-run form and submit it."""
    fields, button = form
    values = {
        "Display name": "Packaged Smoke",
        "Email": "packaged-smoke@example.invalid",
        "Password": secrets.token_urlsafe(18),
    }
    x11(display, "xdotool", "windowfocus", "--sync", window["id"])
    for name in SETUP_FIELDS:
        fields[name].get_component_iface().grab_focus()
        x11(display, "xdotool", "type", "--delay", "15", "--", values[name])
    button.get_action_iface().do_action(0)


def wait_for(description, probe, timeout, app):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if app.poll() is not None:
            raise SmokeFailure(
                f"the app exited with {app.returncode} while waiting for {description}"
            )
        result = probe()
        if result:
            return result
        time.sleep(1)
    raise SmokeFailure(f"timed out after {timeout}s waiting for {description}")


def app_env(display, home, rebuild):
    env = {
        "PATH": "/usr/local/bin:/usr/bin:/bin",
        "HOME": str(home),
        "DISPLAY": display,
        "LANG": "C.UTF-8",
        "XDG_DATA_HOME": str(home / ".local/share"),
        "XDG_CONFIG_HOME": str(home / ".config"),
        "XDG_CACHE_HOME": str(home / ".cache"),
        "XDG_STATE_HOME": str(home / ".local/state"),
        "LIBGL_ALWAYS_SOFTWARE": "1",
        "WEBKIT_DISABLE_DMABUF_RENDERER": "1",
        "PP_DESKTOP_REBUILD": "1" if rebuild else "0",
    }
    for name in ("DBUS_SESSION_BUS_ADDRESS", "XDG_RUNTIME_DIR"):
        if os.environ.get(name):
            env[name] = os.environ[name]
    return env


def verify_database(database, fixture):
    connection = sqlite3.connect(f"file:{database}?mode=ro", uri=True)
    try:
        version = legacy_fixture.schema_version(connection)
        tables = legacy_fixture.table_names(connection)
        snapshot = legacy_fixture.preserved_rows(connection, fixture["preservedColumns"])
    finally:
        connection.close()
    if version is None or version <= fixture["startingVersionStamp"]:
        raise SmokeFailure(f"the database was not migrated: schema_version is {version}")
    for table in ("board_posts", "board_comments"):
        if table not in tables:
            raise SmokeFailure(f"migration did not restore {table}")
    if legacy_fixture.digest(snapshot) != fixture["preservedDigest"]:
        counts = {table: len(rows) for table, rows in snapshot.items()}
        raise SmokeFailure(
            f"Plan or Plate rows changed during migration: {counts} vs {fixture['rowCounts']}"
        )
    return {
        "schemaVersion": version,
        "rowCounts": fixture["rowCounts"],
        "preservedDigest": fixture["preservedDigest"],
    }


# The ownership marker exactly as docs/storage.md specifies it. The cutover
# process writes it on the Rust-owned copy of an installation; the rebuild
# build opens only a marked or empty directory.
RUST_OWNED_MARKER = b'{"version":1,"owner":"rust","created_at":"2026-11-03T00:00:00Z"}\n'


def prepare_data(data, fixture_dir, rust_owned):
    data.mkdir(parents=True, mode=0o700)
    shutil.copyfile(fixture_dir / "print-partner.db", data / "print-partner.db")
    if rust_owned:
        marker = data / ".pp-rust-owned.json"
        marker.write_bytes(RUST_OWNED_MARKER)
        marker.chmod(0o600)


def directory_state(data):
    """Mode and SHA-256 or link target of every entry under data, for the refusal check."""
    state = {".": oct(data.stat().st_mode & 0o7777)}
    for entry in sorted(data.rglob("*")):
        mode = oct(entry.lstat().st_mode & 0o7777)
        if entry.is_symlink():
            value = ["link", os.readlink(entry)]
        elif entry.is_file():
            value = ["file", mode, hashlib.sha256(entry.read_bytes()).hexdigest()]
        else:
            value = ["dir" if entry.is_dir() else "other", mode]
        state[entry.relative_to(data).as_posix()] = value
    return state


def stop(app, owned, timeout):
    app.send_signal(signal.SIGTERM)
    try:
        code = app.wait(timeout)
    except subprocess.TimeoutExpired as error:
        raise SmokeFailure(f"the app did not exit within {timeout}s of SIGTERM") from error
    deadline = time.monotonic() + 10
    while alive(owned) and time.monotonic() < deadline:
        time.sleep(0.2)
    return code, alive(owned)


def last_json_line(path):
    for line in reversed(path.read_text(errors="replace").splitlines()):
        line = line.strip()
        if line.startswith("{"):
            try:
                return json.loads(line)
            except ValueError:
                continue
    return None


class Launch:
    """One app process on its own display, with the processes it started."""

    def __init__(self, args, display, home):
        self.args, self.display = args, display
        self.started = time.monotonic()
        self.stdout = args.output / "app-stdout.log"
        self.stderr = args.output / "app-stderr.log"
        with self.stdout.open("wb") as out, self.stderr.open("wb") as err:
            self.app = subprocess.Popen(
                [str(args.app)],
                env=app_env(display, home, args.rebuild),
                cwd=home,
                stdin=subprocess.DEVNULL,
                stdout=out,
                stderr=err,
            )
        self.owned = {self.app.pid: start_identity(self.app.pid)}

    def elapsed(self):
        return round(time.monotonic() - self.started, 1)

    def pids(self):
        self.owned.update(descendants(self.app.pid))
        return {self.app.pid, *self.owned}

    def window(self, title, refuse=None):
        found = windows(self.display, self.pids())
        if refuse and any(window["title"] == refuse for window in found):
            raise SmokeFailure(f"the app showed the window {refuse!r}")
        matches = [w for w in found if w["title"] == title and w["mapState"] == "IsViewable"]
        return matches[0] if matches else None

    def screenshot(self, name):
        path = self.args.output / name
        x11(self.display, "import", "-window", "root", str(path))
        colors = x11(self.display, "identify", "-format", "%k", str(path)).strip()
        return int(colors) if colors.isdigit() and int(colors) >= PAINTED_COLORS else None

    def wait(self, description, probe, timeout=None):
        return wait_for(description, probe, timeout or self.args.timeout, self.app)

    def close(self):
        if self.app.poll() is None:
            self.app.kill()
            self.app.wait()
        for pid in alive(self.owned):
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass


def expect_usable(launch, receipt, fixture, database):
    needle = receipt["expectText"]

    def main_window():
        window = launch.window(WINDOW_TITLE, refuse=STARTUP_ERROR_TITLE)
        return window if window and window["width"] >= 900 and window["height"] >= 650 else None

    receipt["window"] = launch.wait("the native window", main_window)
    receipt["windowSeconds"] = launch.elapsed()

    def content():
        result = search_accessibility(launch.pids(), needle)
        if result["found"]:
            return result
        if receipt.get("setupCompleted"):
            if not receipt.get("setupContinued") and press_continue(launch.pids()):
                receipt["setupContinued"] = launch.elapsed()
            return None
        form = setup_form(launch.pids())
        if form:
            complete_setup(launch.display, receipt["window"], form)
            receipt["setupCompleted"] = launch.elapsed()
        return None

    receipt["content"] = launch.wait(f"{needle!r} in the accessibility tree", content)
    receipt["contentSeconds"] = launch.elapsed()
    receipt["screenshotColors"] = launch.wait(
        "the page to paint", lambda: launch.screenshot("window.png"), 60
    )
    launch.pids()
    receipt["ownedProcesses"] = len(launch.owned)
    code, survivors = stop(launch.app, launch.owned, 60)
    receipt.update(exitCode=code, survivors=survivors)
    receipt["shutdownReceipt"] = shutdown = last_json_line(launch.stdout)
    if code != 0:
        raise SmokeFailure(f"the app exited with {code} after SIGTERM")
    if not shutdown or shutdown.get("errors") or shutdown.get("storage_released") is not True:
        raise SmokeFailure(f"incomplete shutdown receipt: {shutdown}")
    if survivors:
        raise SmokeFailure(f"child processes survived shutdown: {survivors}")
    receipt["database"] = verify_database(database, fixture)


def expect_refusal(launch, receipt, data, before):
    receipt["window"] = launch.wait(
        "the startup error dialog", lambda: launch.window(STARTUP_ERROR_TITLE)
    )
    receipt["windowSeconds"] = launch.elapsed()
    launch.wait("the dialog to paint", lambda: launch.screenshot("refusal.png"), 60)
    diagnostic = launch.stderr.read_text(errors="replace").strip()
    if diagnostic != "desktop_startup_failed: core_startup":
        raise SmokeFailure(f"expected only desktop_startup_failed: core_startup, got {diagnostic!r}")
    code, survivors = stop(launch.app, launch.owned, 30)
    receipt.update(exitCode=code, survivors=survivors)
    if survivors:
        raise SmokeFailure(f"child processes survived the refusal: {survivors}")
    after = directory_state(data)
    receipt["dataDirectory"] = after
    if after != before:
        raise SmokeFailure(f"the refused data directory changed: {before} -> {after}")


def run(args):
    fixture = json.loads((args.fixture / "fixture.json").read_text())
    args.output.mkdir(parents=True, exist_ok=True)
    root = pathlib.Path(tempfile.mkdtemp(prefix="pp-packaged-smoke-"))
    home = root / "home"
    data = home / ".print-partner"
    prepare_data(data, args.fixture, args.rust_owned)
    before = directory_state(data)
    receipt = {
        "app": str(args.app),
        "rebuild": args.rebuild,
        "rustOwned": args.rust_owned,
        "expect": "refusal" if args.expect_refusal else "usable",
        "fixture": fixture["derivedSha256"],
        "startingVersionStamp": fixture["startingVersionStamp"],
        "expectText": args.expect_text or fixture["builds"][0],
    }
    xvfb, display = start_display()
    launch = None
    try:
        launch = Launch(args, display, home)
        if args.expect_refusal:
            expect_refusal(launch, receipt, data, before)
        else:
            expect_usable(launch, receipt, fixture, data / "print-partner.db")
        receipt["seconds"] = launch.elapsed()
        receipt["status"] = "passed"
        return 0
    except Exception as failure:
        if not isinstance(failure, SmokeFailure):
            failure = f"{type(failure).__name__}: {failure}"
        receipt.update(status="failed", failure=str(failure))
        if launch is not None and launch.app.poll() is None:
            launch.screenshot("failure.png")
        print(f"packaged smoke failed: {failure}", file=sys.stderr)
        return 1
    finally:
        if launch is not None:
            launch.close()
        xvfb.terminate()
        xvfb.wait()
        shutil.rmtree(root, ignore_errors=True)
        text = json.dumps(receipt, indent=2, sort_keys=True) + "\n"
        (args.output / "receipt.json").write_text(text)
        print(text)


def main(argv):
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("--app", type=pathlib.Path, required=True, help="installed executable")
    parser.add_argument(
        "--fixture", type=pathlib.Path, required=True, help="legacy_fixture.py output"
    )
    parser.add_argument("--output", type=pathlib.Path, required=True, help="evidence directory")
    parser.add_argument("--rebuild", action="store_true", help="launch with PP_DESKTOP_REBUILD=1")
    parser.add_argument(
        "--rust-owned",
        action="store_true",
        help="add the .pp-rust-owned.json marker a cutover writes",
    )
    parser.add_argument(
        "--expect-refusal",
        action="store_true",
        help="require the startup error dialog and an unchanged data directory",
    )
    parser.add_argument(
        "--expect-text", help="text to find instead of the fixture's first Build name"
    )
    parser.add_argument("--timeout", type=int, default=180)
    return run(parser.parse_args(argv))


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
