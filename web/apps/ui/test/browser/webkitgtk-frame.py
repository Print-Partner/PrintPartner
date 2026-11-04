#!/usr/bin/env python3
"""Run frame.browser.mjs jobs, one at a time, in a fresh, ephemeral WebKitGTK web view.

Usage: webkitgtk-frame.py JOBS_JSON, a file holding {"axe": <axe-core source>, "jobs": [...]}.
Each job has "url", "body" (an async JavaScript function body that returns a JSON string
with "scrollHeight" and "innerWidth"; it may call runAxe()), "zoom", "text_only" and "png"
(a screenshot path, or null). Prints the parsed results as a JSON list. Exits 3 when
PyGObject or WebKit2GTK 4.1 is missing, so the caller can report SKIP.
"""

import json
import math
import sys

try:
    import gi

    gi.require_version("Gtk", "3.0")
    gi.require_version("WebKit2", "4.1")
    from gi.repository import GLib, Gtk, WebKit2
except (ImportError, ValueError) as error:
    print(f"WebKitGTK unavailable: {error}", file=sys.stderr)
    sys.exit(3)

TIMEOUT_SECONDS = 30
WIDTH, HEIGHT, MAX_HEIGHT = 1280, 800, 6000
PRELUDE = "if (!window.axe) (0, eval)(axeSource);\nconst runAxe = (o) => window.axe.run(document, o);\n"


def main() -> int:
    with open(sys.argv[1], encoding="utf-8") as handle:
        spec = json.load(handle)
    axe_args = GLib.Variant("a{sv}", {"axeSource": GLib.Variant("s", spec["axe"])})
    jobs, results, failure = spec["jobs"], [], []
    state = {"index": -1, "timer": 0}
    loop = GLib.MainLoop()
    settings = WebKit2.Settings()
    view = WebKit2.WebView.new_with_context(WebKit2.WebContext.new_ephemeral())
    view.set_settings(settings)
    window = Gtk.OffscreenWindow()
    window.set_default_size(WIDTH, HEIGHT)
    window.add(view)
    window.show_all()

    def fail(message: str) -> None:
        failure.append(message)
        loop.quit()

    def timed_out() -> bool:
        state["timer"] = 0
        fail(f"{jobs[state['index']]['url']}: timed out after {TIMEOUT_SECONDS}s")
        return False

    def resize(height: int) -> None:
        view.set_size_request(WIDTH, height)
        window.resize(WIDTH, height)

    def next_job() -> bool:
        if state["timer"]:
            GLib.source_remove(state["timer"])
        state["timer"] = GLib.timeout_add_seconds(TIMEOUT_SECONDS, timed_out)
        state["index"] += 1
        if state["index"] >= len(jobs):
            loop.quit()
            return False
        job = jobs[state["index"]]
        settings.set_zoom_text_only(bool(job.get("text_only")))
        view.set_zoom_level(float(job.get("zoom", 1)))
        view.load_uri(job["url"])
        return False

    def screenshot(png: str) -> bool:
        # Without the GI cairo bindings get_snapshot() cannot be read, so the window grows
        # to the whole document and its pixels are saved instead.
        window.get_pixbuf().savev(png, "png", [], [])
        resize(HEIGHT)
        return next_job()

    def body_done(source, task) -> None:
        job = jobs[state["index"]]
        try:
            result = json.loads(source.call_async_javascript_function_finish(task).to_string())
        except GLib.Error as error:
            fail(f"{job['url']}: script failed: {error.message}")
            return
        results.append({"url": job["url"], "result": result})
        if not job.get("png"):
            next_job()
            return
        scale = WIDTH / max(result["innerWidth"], 1)
        resize(min(MAX_HEIGHT, max(HEIGHT, math.ceil(result["scrollHeight"] * scale))))
        GLib.timeout_add(400, screenshot, job["png"])  # let the page lay out at the new size

    def load_changed(_view, event) -> None:
        if event == WebKit2.LoadEvent.FINISHED:
            body = PRELUDE + jobs[state["index"]]["body"]
            view.call_async_javascript_function(body, -1, axe_args, None, None, None, body_done)

    view.connect("load-changed", load_changed)
    view.connect("load-failed", lambda _v, _e, uri, error: fail(f"load failed for {uri}: {error.message}"))
    GLib.idle_add(next_job)
    loop.run()
    if failure:
        print("; ".join(failure), file=sys.stderr)
        return 1
    print(json.dumps(results))
    return 0


if __name__ == "__main__":
    sys.exit(main())
