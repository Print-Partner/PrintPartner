#!/usr/bin/env python3
"""Load a URL in a fresh, ephemeral WebKitGTK web view and print what it requested.

Usage: webkitgtk-requests.py URL PROBE_BODY CONTROL_BODY

PROBE_BODY and CONTROL_BODY are async JavaScript function bodies that return a string.
Prints one JSON object: {"report": <probe result>, "requests": [...], "control": [...]},
where "control" lists the requests made while CONTROL_BODY ran. Exits 3 when PyGObject
or WebKit2GTK 4.1 is missing, so the caller can report SKIP.
"""

import json
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


def main() -> int:
    url, probe_body, control_body = sys.argv[1:4]
    requests: list[str] = []
    result: dict = {}
    failure: list[str] = []
    loop = GLib.MainLoop()

    view = WebKit2.WebView.new_with_context(WebKit2.WebContext.new_ephemeral())
    view.connect("resource-load-started", lambda _v, _r, request: requests.append(request.get_uri()))
    window = Gtk.OffscreenWindow()
    window.set_default_size(1280, 800)
    window.add(view)
    window.show_all()

    def run(body: str, on_done) -> None:
        def finished(source, task) -> None:
            try:
                on_done(source.call_async_javascript_function_finish(task).to_string())
            except GLib.Error as error:
                failure.append(f"script failed: {error.message}")
                loop.quit()

        view.call_async_javascript_function(body, -1, None, None, None, None, finished)

    def control_done(_value: str) -> None:
        # Leave time for the request to be issued before reading the log.
        GLib.timeout_add(500, lambda: (result.update(control=requests[result["before"] :]), loop.quit()))

    def probe_done(value: str) -> None:
        result["report"] = json.loads(value)
        result["requests"] = list(requests)
        result["before"] = len(requests)
        run(control_body, control_done)

    def load_changed(_view, event) -> None:
        if event == WebKit2.LoadEvent.FINISHED and "report" not in result:
            run(probe_body, probe_done)

    def timed_out() -> bool:
        failure.append(f"timed out after {TIMEOUT_SECONDS}s")
        loop.quit()
        return False

    view.connect("load-changed", load_changed)
    view.connect(
        "load-failed",
        lambda _v, _e, uri, error: (failure.append(f"load failed for {uri}: {error.message}"), loop.quit()),
    )
    GLib.timeout_add_seconds(TIMEOUT_SECONDS, timed_out)
    view.load_uri(url)
    loop.run()

    if failure:
        print("; ".join(failure), file=sys.stderr)
        return 1
    result.pop("before")
    print(json.dumps(result))
    return 0


if __name__ == "__main__":
    sys.exit(main())
