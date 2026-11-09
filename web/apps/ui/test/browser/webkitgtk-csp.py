#!/usr/bin/env python3
"""Load a URL in a fresh, ephemeral WebKitGTK web view with a document-start script.

Usage: webkitgtk-csp.py URL INIT_SCRIPT PROBE_BODY

INIT_SCRIPT runs in the page's main world before any page script. PROBE_BODY is an
async JavaScript function body that returns a string; it runs once the load finishes,
and its result is printed. Exits 3 when PyGObject or WebKit2GTK 4.1 is missing, so the
caller can report SKIP.
"""

import sys

try:
    import gi

    gi.require_version("Gtk", "3.0")
    gi.require_version("WebKit2", "4.1")
    from gi.repository import GLib, Gtk, WebKit2
except (ImportError, ValueError) as error:
    print(f"WebKitGTK unavailable: {error}", file=sys.stderr)
    sys.exit(3)

TIMEOUT_SECONDS = 45


def main() -> int:
    url, init_script, probe_body = sys.argv[1:4]
    result: list[str] = []
    failure: list[str] = []
    loop = GLib.MainLoop()

    content = WebKit2.UserContentManager()
    content.add_script(
        WebKit2.UserScript(
            init_script,
            WebKit2.UserContentInjectedFrames.TOP_FRAME,
            WebKit2.UserScriptInjectionTime.START,
            None,
            None,
        )
    )
    view = WebKit2.WebView(web_context=WebKit2.WebContext.new_ephemeral(), user_content_manager=content)
    window = Gtk.OffscreenWindow()
    window.set_default_size(1280, 800)
    window.add(view)
    window.show_all()

    def finished(source, task) -> None:
        try:
            result.append(source.call_async_javascript_function_finish(task).to_string())
        except GLib.Error as error:
            failure.append(f"script failed: {error.message}")
        loop.quit()

    def load_changed(_view, event) -> None:
        if event == WebKit2.LoadEvent.FINISHED and not result:
            view.call_async_javascript_function(probe_body, -1, None, None, None, None, finished)

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
    print(result[0])
    return 0


if __name__ == "__main__":
    sys.exit(main())
