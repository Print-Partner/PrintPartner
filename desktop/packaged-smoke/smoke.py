#!/usr/bin/env python3
"""Launch the packaged app on an older database and prove it is usable.

The smoke extracts nothing and builds nothing: it takes the installed
executable (usr/bin/pp-desktop from the unpacked .deb on Linux, or
Contents/MacOS/pp-desktop inside the installed Print Partner.app on macOS), a
fixture directory from legacy_fixture.py and an evidence directory. It launches
the app with a disposable HOME and requires:

1. a visible native window owned by the app whose title is "Print Partner";
2. the fixture's Build name as a link or table cell inside the web page, read
   from the accessibility tree (AT-SPI on Linux, the Accessibility API on macOS);
3. a clean exit after SIGTERM (exit code 0 and a shutdown receipt with no
   errors and storage_released) with no surviving child processes;
4. the database migrated past the fixture's stamp with every preserved Plan
   and Plate row unchanged.

With --expect-refusal it instead requires the startup error dialog and a data
directory whose entries and bytes did not change. The rebuild build refuses an
existing installation that has no .pp-rust-owned.json marker (docs/storage.md).

On Linux, run it inside "dbus-run-session" so the accessibility bus is private
to the run; it starts its own Xvfb display (linux_ui.py). On macOS it uses the
logged-in session and ax_probe.swift (macos_ui.py).
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
PAINTED_COLORS = 64
# The first-run administrator form the rebuild build shows when the migrated
# installation has no account (web/apps/web/src/pages/LoginPage.tsx).
SETUP_FIELDS = ("Display name", "Email", "Password")
SETUP_BUTTON = "Create administrator"
CONTINUE_BUTTON = "Continue to Print Partner"
TRAY_QUIT = "Quit Print Partner"


class SmokeFailure(Exception):
    pass


def platform_ui():
    if sys.platform.startswith("linux"):
        import linux_ui

        return linux_ui.LinuxUI()
    if sys.platform == "darwin":
        import macos_ui

        return macos_ui.MacUI()
    raise SmokeFailure(f"no packaged smoke UI for {sys.platform}")


def alive(ui, owned):
    return sorted(
        pid for pid, identity in owned.items() if identity and ui.start_identity(pid) == identity
    )


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


def app_env(ui, home, rebuild):
    return {
        **ui.env(home),
        "HOME": str(home),
        "PP_DESKTOP_REBUILD": "1" if rebuild else "0",
    }


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


def stop(ui, app, owned, timeout, quit=None):
    """Quit the app (SIGTERM unless quit is given) and wait for it and its children."""
    if quit is None:
        app.send_signal(signal.SIGTERM)
    else:
        quit()
    try:
        code = app.wait(timeout)
    except subprocess.TimeoutExpired as error:
        raise SmokeFailure(f"the app did not exit within {timeout}s of the quit") from error
    deadline = time.monotonic() + 10
    while alive(ui, owned) and time.monotonic() < deadline:
        time.sleep(0.2)
    return code, alive(ui, owned)


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
    """One app process with the processes it started."""

    def __init__(self, args, ui, home):
        self.args, self.ui = args, ui
        self.started = time.monotonic()
        self.stdout = args.output / "app-stdout.log"
        self.stderr = args.output / "app-stderr.log"
        with self.stdout.open("wb") as out, self.stderr.open("wb") as err:
            self.app = subprocess.Popen(
                [str(args.app)],
                env=app_env(ui, home, args.rebuild),
                cwd=home,
                stdin=subprocess.DEVNULL,
                stdout=out,
                stderr=err,
            )
        self.owned = {self.app.pid: ui.start_identity(self.app.pid)}

    def elapsed(self):
        return round(time.monotonic() - self.started, 1)

    def pids(self):
        self.owned.update(self.ui.descendants(self.app.pid))
        return {self.app.pid, *self.owned}

    def main_window(self):
        pids = self.pids()
        if self.ui.startup_error(pids, STARTUP_ERROR_TITLE):
            raise SmokeFailure(f"the app showed {STARTUP_ERROR_TITLE!r}")
        for window in self.ui.windows(pids):
            if window["title"] == WINDOW_TITLE and window["visible"]:
                return window
        return None

    def screenshot(self, window, name):
        colors = self.ui.screenshot(window, self.args.output / name)
        return colors if colors >= PAINTED_COLORS else None

    def wait(self, description, probe, timeout=None):
        return wait_for(description, probe, timeout or self.args.timeout, self.app)

    def close(self):
        if self.app.poll() is None:
            self.app.kill()
            self.app.wait()
        for pid in alive(self.ui, self.owned):
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass


def expect_usable(launch, receipt, fixture, database):
    needle = receipt["expectText"]

    def main_window():
        window = launch.main_window()
        return window if window and window["width"] >= 900 and window["height"] >= 650 else None

    receipt["window"] = launch.wait("the native window", main_window)
    receipt["windowSeconds"] = launch.elapsed()

    def content():
        result = launch.ui.find_content(launch.pids(), needle)
        if result:
            return result
        if receipt.get("setupCompleted"):
            if not receipt.get("setupContinued") and launch.ui.press(
                launch.pids(), CONTINUE_BUTTON
            ):
                receipt["setupContinued"] = launch.elapsed()
            return None
        form = launch.ui.setup_form(launch.pids(), SETUP_FIELDS, SETUP_BUTTON)
        if form:
            values = {
                "Display name": "Packaged Smoke",
                "Email": "packaged-smoke@example.invalid",
                "Password": secrets.token_urlsafe(18),
            }
            launch.ui.complete_setup(receipt["window"], form, values)
            receipt["setupCompleted"] = launch.elapsed()
        return None

    receipt["content"] = launch.wait(f"{needle!r} in the accessibility tree", content)
    receipt["contentSeconds"] = launch.elapsed()
    receipt["screenshotColors"] = launch.wait(
        "the page to paint", lambda: launch.screenshot(receipt["window"], "window.png"), 60
    )
    launch.pids()
    receipt["ownedProcesses"] = len(launch.owned)
    quit = None
    if launch.args.quit == "tray":

        def quit():
            receipt["trayQuit"] = report = launch.ui.tray_quit(launch.app.pid, TRAY_QUIT)
            if not report.get("pressed"):
                raise SmokeFailure(f"could not choose {TRAY_QUIT!r} from the tray: {report}")

    receipt["quit"] = launch.args.quit
    code, survivors = stop(launch.ui, launch.app, launch.owned, 60, quit)
    receipt.update(exitCode=code, survivors=survivors)
    receipt["shutdownReceipt"] = shutdown = last_json_line(launch.stdout)
    if code != 0:
        raise SmokeFailure(f"the app exited with {code} after the {launch.args.quit} quit")
    if not shutdown or shutdown.get("errors") or shutdown.get("storage_released") is not True:
        raise SmokeFailure(f"incomplete shutdown receipt: {shutdown}")
    if survivors:
        raise SmokeFailure(f"child processes survived shutdown: {survivors}")
    receipt["database"] = verify_database(database, fixture)


def expect_refusal(launch, receipt, data, before):
    receipt["window"] = launch.wait(
        "the startup error dialog",
        lambda: launch.ui.startup_error(launch.pids(), STARTUP_ERROR_TITLE),
    )
    receipt["windowSeconds"] = launch.elapsed()
    launch.wait(
        "the dialog to paint", lambda: launch.screenshot(receipt["window"], "refusal.png"), 60
    )
    diagnostic = launch.stderr.read_text(errors="replace").strip()
    if diagnostic != "desktop_startup_failed: core_startup":
        raise SmokeFailure(
            f"expected only desktop_startup_failed: core_startup, got {diagnostic!r}"
        )
    code, survivors = stop(launch.ui, launch.app, launch.owned, 30)
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
    ui = platform_ui()
    launch = None
    try:
        ui.start()
        launch = Launch(args, ui, home)
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
            launch.screenshot(receipt.get("window"), "failure.png")
            ui.snapshot(launch.pids(), args.output / "accessibility.json")
        print(f"packaged smoke failed: {failure}", file=sys.stderr)
        return 1
    finally:
        if launch is not None:
            launch.close()
        ui.stop()
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
    parser.add_argument(
        "--quit",
        choices=("signal", "tray"),
        default="signal",
        help="quit with SIGTERM, or with the tray menu's Quit item (macOS)",
    )
    parser.add_argument("--timeout", type=int, default=180)
    return run(parser.parse_args(argv))


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
