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
        if not self.call("trusted")["trusted"]:
            raise RuntimeError("this process is not trusted for Accessibility")

    def stop(self):
        if self.build:
            subprocess.run(["rm", "-rf", self.build], check=False)

    def env(self, home):
        return {"PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "LANG": "en_US.UTF-8"}

    def call(self, *args):
        result = subprocess.run(
            [self.probe, *map(str, args)], capture_output=True, text=True, timeout=60
        )
        if result.returncode != 0:
            return None
        return json.loads(result.stdout)

    def dump(self, pids):
        return self.call("dump", DUMP_LIMIT, *sorted(pids)) or {"apps": []}

    def windows(self, pids):
        numbers = self.call("cgwindows", *sorted(pids)) or []
        found = []
        for pid, path, node, ancestors in walk(self.dump(pids)):
            if ancestors or node.get("r") != "AXWindow":
                continue
            frame = (node.get("x"), node.get("y"), node.get("w"), node.get("h"))
            match = [
                n["id"]
                for n in numbers
                if n["pid"] == pid and n["layer"] == 0 and (n["x"], n["y"], n["w"], n["h"]) == frame
            ]
            found.append(
                {
                    "id": match[0] if match else None,
                    "pid": pid,
                    "title": node.get("t", ""),
                    "visible": not node.get("min", False),
                    "width": node.get("w", 0),
                    "height": node.get("h", 0),
                }
            )
        return found

    def startup_error(self, pids, title):
        dump = self.dump(pids)
        windows = {}
        for pid, path, node, ancestors in walk(dump):
            if not ancestors:
                windows[(pid, path)] = node
        for (pid, path), window in windows.items():
            if window.get("t") == title or title in node_text(window):
                return {
                    "pid": pid,
                    "title": window.get("t", ""),
                    "text": title,
                    "width": window.get("w", 0),
                    "height": window.get("h", 0),
                    "id": self.window_number(pid, window),
                }
        return None

    def window_number(self, pid, window):
        frame = (window.get("x"), window.get("y"), window.get("w"), window.get("h"))
        for n in self.call("cgwindows", pid) or []:
            if (n["x"], n["y"], n["w"], n["h"]) == frame:
                return n["id"]
        return None

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
        fields, button = form
        for name, value in values.items():
            pid, path = fields[name]
            self.call("type", pid, path, value)
        pid, path = button
        self.call("press", pid, path)

    def press(self, pids, button_name):
        for pid, path, node, ancestors in walk(self.dump(pids)):
            if (
                "AXWebArea" in ancestors
                and node.get("r") in ("AXButton", "AXLink")
                and button_name in (label(node), node_text(node))
            ):
                return bool((self.call("press", pid, path) or {}).get("pressed"))
        return False

    def screenshot(self, window, path):
        target = ["-l", str(window["id"])] if window and window.get("id") else []
        subprocess.run(["screencapture", "-x", "-o", *target, str(path)], check=False)
        return (self.call("colors", path) or {}).get("colors", 0)

    def snapshot(self, pids, path):
        """Write the AX tree for a failed run's evidence."""
        pathlib.Path(path).write_text(json.dumps(self.dump(pids), indent=1))
