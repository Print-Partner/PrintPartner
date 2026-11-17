# One Rust program serves the React screens

Status: accepted on 2026-11-15 by Chad.

"Rust native" means one Rust program owns the data directory, printers, backups and MCP, and serves the replacement UI (`web/apps/ui`) as static files. The desktop window, a LAN browser and the bench phone's browser all load those same files from it. Node runs only at build time (Vite), never at runtime. Print Partner ships as a Docker image and as a desktop app for Windows, macOS and Linux.

We kept the React UI instead of a Rust UI toolkit because a UI toolkit makes none of the slow operations faster: thumbnails, Plan changes, Checkoff and exports are Rust core work. A rewrite would cost 4–10 weeks.

## Considered options

- **Dioxus or Leptos:** these still render in the same system WebView and ship WASM to the browser. Dioxus is pre-1.0 and has broken apps between releases; Leptos has hydration bugs and a 2 MB default upload limit.
- **Slint, egui, Iced or GPUI:** native widgets can't serve a browser or phone, so we would maintain two UIs.
- **Server-rendered HTML with htmx or Datastar:** the only route to no JavaScript at all. It means rewriting about 24k lines of UI and tests.

Revisit only if measurements show the WebView, not the core, is the slow part.
