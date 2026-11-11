# Network sharing

The desktop app's "Share on network" setting serves the web UI to phones, tablets and other computers on the local network. This page will describe how to turn it on and how access is protected. The feature does not exist yet. Desktop startup keeps the Rust gateway on loopback. The Docker launcher can set an explicit listener; see [Docker image](docker.md#run).

## Rules

- Sharing is off by default.
- Every device on the network must sign in or pair with the desktop app before it can use the UI.

## To be written

The API milestone fills in these sections:

- Sign-in and sessions for a single user.
- Device pairing with a code or a QR code.
- Optional TLS and mDNS discovery.
- The firewall self-test.
- How the gateway checks the `Host` and `Origin` headers.

The Docker image does not use this setting. It requires sign-in. See [Docker image](docker.md).

## Content security policy

The gateway sets `Content-Security-Policy` on every document and asset it serves. The policy depends on which UI build the gateway serves. `pp-core` names it with `pp_gateway::ServedUi` when it calls `Gateway::start_rust_only`:

- The old `apps/web` build, in every runtime (Node-backed, or Rust-only with `ServedUi::Legacy`): `frame-src 'none'; object-src 'none'; base-uri 'self'; frame-ancestors 'none'`, unchanged.
- The replacement `apps/ui` build on a Rust-only runtime (`desktop_rebuild` with `PP_DESKTOP_REBUILD=1` or `true`, and `ServedUi::Replacement`): the policy in [`replacement-csp.txt`](../rust/crates/pp-gateway/src/replacement-csp.txt), for the replacement UI (GRE-1117):

  ```text
  default-src 'none'; script-src 'self'; style-src 'self' 'nonce-<nonce>'; font-src 'self'; img-src 'self'; connect-src 'self' ws://127.0.0.1:<port>/ws/printers; worker-src 'self'; manifest-src 'self'; frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'
  ```

  Scripts, styles, fonts, images, workers and connections stay on the gateway's own origin. `connect-src` also names the printer event stream by its full `ws://` URL, because older WebKit builds do not match `ws:` to `'self'`. There is no `'unsafe-inline'` and no `'unsafe-eval'`, and `base-uri` is `'none'`.

  Each document response gets a fresh 128-bit nonce. The gateway reads `index.html` (never its `.gz` sibling) and inserts `<meta name="pp-style-nonce" nonce="<nonce>">` right after the `<head>` start tag, or after the doctype when there is no `<head>` tag. The document is always sent uncoded, even to a client that refuses `identity`. [`web/apps/ui/src/csp.ts`](../web/apps/ui/src/csp.ts) hands the nonce to Radix's scroll lock, the one library that inserts a `<style>` element at run time, and turns on Zod's jitless mode so Zod does not probe `new Function`. Asset responses carry the same policy without a nonce.

  Today `pp-server`, `pp-desktop` and the Rust-only Docker image still serve the old `apps/web` build in the Rust-only runtime, so they pass `ServedUi::Legacy` and send the legacy policy. That build needs `blob:` thumbnails, an inline theme script, Google Fonts and un-nonced `<style>` elements, which the replacement policy blocks. The replacement policy takes effect when [GRE-1294](https://linear.app/greetz/issue/GRE-1294) serves `apps/ui` from the Rust-only runtime and passes `ServedUi::Replacement`.

Checks:

- `cargo nextest run --locked --manifest-path rust/Cargo.toml -p pp-gateway -E 'test(csp)'` checks both policies in all three runtimes, the nonce element's placement, a fresh nonce per document, and the uncoded document for an identity-refusing client.
- `web/apps/ui/test/browser/csp.browser.mjs` (part of `npm run test:browser`) loads the built replacement UI under the template's policy in Chromium and WebKitGTK. It requires no violation while the frame loads, the printer stream opens and the command palette's scroll lock applies, and requires a planted external script, a planted inline script and an external `fetch` to be blocked without reaching the server. A control load without the policy shows all three run.
- `rust/tests/workflow_m0.py` and `rust/tests/gateway_product.py` run the Node-backed runtime and assert its legacy policy, with no `script-src`, `style-src`, `connect-src` or nonce.
