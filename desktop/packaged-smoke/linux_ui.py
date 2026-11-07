"""Linux UI for the packaged smoke: Xvfb, X11 windows and the AT-SPI tree."""

import os
import pathlib
import subprocess
import time

DISPLAY_SIZE = "1280x1024x24"
# The Build name must be page content (the Builds table link or cell), not only window chrome.
CONTENT_ROLES = ("link", "table cell", "cell")
WALK_LIMIT = 50000


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


def free_display():
    for number in range(140, 200):
        if not os.path.exists(f"/tmp/.X11-unix/X{number}") and not os.path.exists(
            f"/tmp/.X{number}-lock"
        ):
            return number
    raise RuntimeError("no free X display")


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


class LinuxUI:
    """Run inside dbus-run-session so the accessibility bus is private to the run."""

    start_identity = staticmethod(start_identity)
    descendants = staticmethod(descendants)

    def __init__(self):
        self.xvfb = None
        self.display = None

    def start(self):
        number = free_display()
        self.xvfb = subprocess.Popen(
            ["Xvfb", f":{number}", "-screen", "0", DISPLAY_SIZE, "-nolisten", "tcp"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        deadline = time.monotonic() + 10
        while not os.path.exists(f"/tmp/.X11-unix/X{number}"):
            if self.xvfb.poll() is not None or time.monotonic() > deadline:
                self.xvfb.kill()
                raise RuntimeError("Xvfb did not start")
            time.sleep(0.05)
        self.display = f":{number}"

    def stop(self):
        if self.xvfb is not None:
            self.xvfb.terminate()
            self.xvfb.wait()

    def env(self, home):
        env = {
            "PATH": "/usr/local/bin:/usr/bin:/bin",
            "DISPLAY": self.display,
            "LANG": "C.UTF-8",
            "XDG_DATA_HOME": str(home / ".local/share"),
            "XDG_CONFIG_HOME": str(home / ".config"),
            "XDG_CACHE_HOME": str(home / ".cache"),
            "XDG_STATE_HOME": str(home / ".local/state"),
            "LIBGL_ALWAYS_SOFTWARE": "1",
            "WEBKIT_DISABLE_DMABUF_RENDERER": "1",
        }
        for name in ("DBUS_SESSION_BUS_ADDRESS", "XDG_RUNTIME_DIR"):
            if os.environ.get(name):
                env[name] = os.environ[name]
        return env

    def x11(self, *args):
        result = subprocess.run(
            list(args),
            env={**os.environ, "DISPLAY": self.display},
            capture_output=True,
            text=True,
            timeout=20,
        )
        return result.stdout if result.returncode == 0 else ""

    def windows(self, pids):
        """Return the X11 windows that belong to any of the pids."""
        found = []
        for pid in pids:
            for wid in self.x11(
                "xdotool", "search", "--all", "--onlyvisible", "--pid", str(pid), "--name", ""
            ).split():
                info = self.x11("xwininfo", "-id", wid)
                facts = dict(
                    line.strip().split(": ", 1) for line in info.splitlines() if ": " in line
                )
                found.append(
                    {
                        "id": wid,
                        "pid": pid,
                        "title": self.x11("xdotool", "getwindowname", wid).rstrip("\n"),
                        "visible": facts.get("Map State") == "IsViewable",
                        "width": int(facts.get("Width", 0)),
                        "height": int(facts.get("Height", 0)),
                    }
                )
        return found

    def startup_error(self, pids, title):
        matches = [w for w in self.windows(pids) if w["title"] == title and w["visible"]]
        return matches[0] if matches else None

    def find_content(self, pids, needle):
        """Find needle as a link or table cell inside the page, not in window chrome."""
        for node, role, name, path in walk(pids):
            if (
                role in CONTENT_ROLES
                and "document web" in path
                and needle in accessible_text(node, name)
            ):
                return {"role": role, "path": path + [role]}
        return None

    def setup_form(self, pids, labels, button_name):
        fields, button = {}, None
        for node, role, name, path in walk(pids):
            if "document web" not in path:
                continue
            if role in ("entry", "password text") and name in labels:
                fields[name] = node
            elif role == "push button" and name == button_name:
                button = node
        return (fields, button) if button is not None and len(fields) == len(labels) else None

    def complete_setup(self, window, form, values):
        fields, button = form
        self.x11("xdotool", "windowfocus", "--sync", window["id"])
        for name, value in values.items():
            fields[name].get_component_iface().grab_focus()
            self.x11("xdotool", "type", "--delay", "15", "--", value)
        button.get_action_iface().do_action(0)
        return None

    def press(self, pids, button_name):
        for node, role, name, path in walk(pids):
            if "document web" in path and role in ("push button", "link") and name == button_name:
                try:
                    node.get_action_iface().do_action(0)
                except Exception:
                    return False
                return True
        return False

    def tray_quit(self, pid, item):
        """The AppIndicator menu is exported over D-Bus, not AT-SPI; not driven here."""
        return {"pressed": False, "reason": "tray quit is macOS only"}

    def dismiss(self, dialog):
        """The GTK dialog belongs to the app process and closes with it."""
        return True

    def evidence_screenshot(self, path):
        self.screenshot(None, path)

    def snapshot(self, pids, path):
        """The AT-SPI tree is not dumped; the screenshot is the failure evidence."""

    def screenshot(self, window, path):
        self.x11("import", "-window", "root", str(path))
        colors = self.x11("identify", "-format", "%k", str(path)).strip()
        return int(colors) if colors.isdigit() else 0
