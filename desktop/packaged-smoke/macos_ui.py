"""macOS UI for the packaged smoke: the Accessibility API through ax_probe.swift.

The probe reads the app's AX tree, which includes the WKWebView page, presses
and types into page elements, finds window numbers for screencapture, and
counts screenshot colors. The process that runs the smoke must be trusted for
Accessibility; GitHub's macOS runner images grant that to the runner.
"""

import json
import os
import pathlib
import subprocess
import tempfile

# The Build name must be page content (the Builds table link or cell), not only window chrome.
CONTENT_ROLES = ("AXLink", "AXCell")
DUMP_LIMIT = 20000
HERE = pathlib.Path(__file__).resolve().parent


def ps_table():
    """Return {pid: (ppid, start time)} for every process."""
    output = subprocess.run(
        ["ps", "-axo", "pid=,ppid=,lstart="], capture_output=True, text=True, check=True
    ).stdout
    table = {}
    for line in output.splitlines():
        parts = line.split(None, 2)
        if len(parts) == 3 and parts[0].isdigit() and parts[1].isdigit():
            table[int(parts[0])] = (int(parts[1]), parts[2].strip())
    return table


def start_identity(pid):
    entry = ps_table().get(pid)
    return entry[1] if entry else None


def descendants(root):
    table = ps_table()
    children = {}
    for pid, (parent, _) in table.items():
        children.setdefault(parent, []).append(pid)
    found, pending = {}, list(children.get(root, []))
    while pending:
        pid = pending.pop()
        if pid in found:
            continue
        found[pid] = table[pid][1]
        pending.extend(children.get(pid, []))
    return found


def notification_center():
    """Pids of the agent that draws CFUserNotification alerts."""
    result = subprocess.run(
        ["pgrep", "-x", "UserNotificationCenter"], capture_output=True, text=True
    )
    return {int(pid) for pid in result.stdout.split() if pid.isdigit()}


def label(node):
    return node.get("t") or node.get("d") or node.get("l") or node.get("p") or ""


def node_text(node):
    parts = [node.get(key, "") for key in ("t", "d", "v")]
    parts.extend(node_text(child) for child in node.get("c", []))
    return " ".join(part for part in parts if part)


def walk(dump):
    """Yield (pid, path, node, ancestor roles) for every node in a dump."""
    for app in dump["apps"]:
        pending = [((index,), window, []) for index, window in enumerate(app["windows"])]
        while pending:
            path, node, ancestors = pending.pop()
            yield app["pid"], ".".join(map(str, path)), node, ancestors
            for index, child in enumerate(node.get("c", [])):
                pending.append((path + (index,), child, ancestors + [node.get("r", "")]))


class MacUI:
    start_identity = staticmethod(start_identity)
    descendants = staticmethod(descendants)

    def __init__(self):
        self.probe = os.environ.get("PP_AX_PROBE")
        self.build = None

    def start(self):
        if not self.probe:
            self.build = tempfile.mkdtemp(prefix="pp-ax-")
            self.probe = str(pathlib.Path(self.build) / "pp-ax")
            subprocess.run(
                ["swiftc", "-O", str(HERE / "ax_probe.swift"), "-o", self.probe], check=True
            )
        if not (self.call("trusted") or {}).get("trusted"):
            raise RuntimeError("this process is not trusted for Accessibility")

    def stop(self):
        if self.build:
            subprocess.run(["rm", "-rf", self.build], check=False)

    def env(self, home):
        return {"PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "LANG": "en_US.UTF-8"}

    def call(self, *args):
        try:
            result = subprocess.run(
                [self.probe, *map(str, args)], capture_output=True, text=True, timeout=60
            )
        except subprocess.TimeoutExpired:
            return None
        if result.returncode != 0:
            return None
        return json.loads(result.stdout)

    def dump(self, pids):
        return self.call("dump", DUMP_LIMIT, *sorted(pids)) or {"apps": []}

    def on_screen(self, pids, any_layer=False):
        """On-screen windows by (pid, frame); normal-layer only unless any_layer."""
        return {
            (n["pid"], (n["x"], n["y"], n["w"], n["h"])): n["id"]
            for n in self.call("cgwindows", *sorted(pids)) or []
            if n["onscreen"] and (any_layer or n["layer"] == 0)
        }

    def windows(self, pids):
        """AX windows; visible only when a matching on-screen window exists."""
        shown = self.on_screen(pids)
        found = []
        for pid, path, node, ancestors in walk(self.dump(pids)):
            if not ancestors and node.get("r") == "AXWindow":
                frame = (node.get("x"), node.get("y"), node.get("w"), node.get("h"))
                number = shown.get((pid, frame))
                found.append(
                    {
                        "id": number,
                        "pid": pid,
                        "title": node.get("t", ""),
                        "visible": number is not None and not node.get("min", False),
                        "frame": list(frame),
                        "width": node.get("w", 0),
                        "height": node.get("h", 0),
                    }
                )
            elif node.get("r") == "AXWebArea" and found and "page" not in found[-1]:
                origin = found[-1]["frame"]
                if None not in (node.get("x"), node.get("y"), origin[0], origin[1]):
                    found[-1]["page"] = [
                        node["x"] - origin[0],
                        node["y"] - origin[1],
                        node.get("w", 0),
                        node.get("h", 0),
                    ]
        return found

    def startup_error(self, pids, title):
        """The startup error alert. rfd shows it with CFUserNotificationDisplayAlert,
        which the UserNotificationCenter agent draws, so its windows are searched too."""
        owners = set(pids) | notification_center()
        shown = self.on_screen(owners, any_layer=True)
        for pid, path, node, ancestors in walk(self.dump(owners)):
            if ancestors or not (node.get("t") == title or title in node_text(node)):
                continue
            frame = (node.get("x"), node.get("y"), node.get("w"), node.get("h"))
            return {
                "kind": "dialog",
                "id": shown.get((pid, frame)),
                "pid": pid,
                "path": path,
                "title": node.get("t", ""),
                "text": title,
                "width": node.get("w", 0),
                "height": node.get("h", 0),
            }
        return None

    def dismiss(self, dialog):
        """Press the alert's OK button, so it cannot outlive the launch."""
        for pid, path, node, ancestors in walk(self.dump({dialog["pid"]})):
            if path.split(".")[0] == dialog["path"] and node.get("r") == "AXButton":
                if label(node) == "OK" or node_text(node) == "OK":
                    return bool((self.call("press", pid, path) or {}).get("pressed"))
        return False

    def find_content(self, pids, needle):
        for pid, path, node, ancestors in walk(self.dump(pids)):
            if (
                node.get("r") in CONTENT_ROLES
                and "AXWebArea" in ancestors
                and needle in node_text(node)
            ):
                return {"role": node["r"], "path": ancestors + [node["r"]]}
        return None

    def setup_form(self, pids, labels, button_name):
        fields, button = {}, None
        for pid, path, node, ancestors in walk(self.dump(pids)):
            if "AXWebArea" not in ancestors:
                continue
            if node.get("r") == "AXTextField" and label(node) in labels:
                fields[label(node)] = (pid, path)
            elif node.get("r") == "AXButton" and button_name in (label(node), node_text(node)):
                button = (pid, path)
        return (fields, button) if button is not None and len(fields) == len(labels) else None

    def complete_setup(self, window, form, values):
        """Type each value into its field and press the button; return the probe's reports."""
        fields, button = form
        reports = {}
        for name, value in values.items():
            pid, path = fields[name]
            reports[name] = self.call("type", pid, path, value)
        pid, path = button
        reports["submit"] = self.call("press", pid, path)
        return reports

    def press(self, pids, button_name):
        for pid, path, node, ancestors in walk(self.dump(pids)):
            if (
                "AXWebArea" in ancestors
                and node.get("r") in ("AXButton", "AXLink")
                and button_name in (label(node), node_text(node))
            ):
                return bool((self.call("press", pid, path) or {}).get("pressed"))
        return False

    def tray_quit(self, pid, item):
        """Choose item from the app's menu bar extra; return the probe's report."""
        return self.call("tray", pid, item) or {"pressed": False}

    def screenshot(self, window, path):
        """Count the colors of the page area of the main window, or of a dialog.

        The capture is the window's own image (screencapture -o -l), so other
        windows cannot cover it; the count excludes the title bar. A main
        window without a page area, or a window that is not on screen, counts 0.
        """
        if not window or window.get("id") is None:
            return 0
        capture = ["screencapture", "-x", "-o", "-l", str(window["id"]), str(path)]
        subprocess.run(capture, check=False)
        if window.get("kind") == "dialog":
            return (self.call("colors", path) or {}).get("colors", 0)
        page = window.get("page")
        if not page or not window.get("width"):
            return 0
        return (self.call("colors", path, window["width"], *page) or {}).get("colors", 0)

    def evidence_screenshot(self, path):
        """The whole screen, for a failed launch only."""
        subprocess.run(["screencapture", "-x", str(path)], check=False)

    def snapshot(self, pids, path):
        """Write the AX tree for a failed run's evidence."""
        pathlib.Path(path).write_text(json.dumps(self.dump(pids), indent=1))
