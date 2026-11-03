# Replace the old screens instead of porting them

Status: accepted on 2026-11-02 by Chad. Supersedes the "Rust parity first, then UI" ordering in the desktop rebuild plan and the UI v11 sequence in the Notion UI plan.

The old React screens in `web/apps/web` are not a parity target. The new UI is built once, against the Rust API, and the old screens are frozen now and deleted after cutover. Work that exists only to make an old screen work against Rust is cancelled rather than finished.

The earlier plan held the UI until Rust reached parity with Node, so each screen could be swapped without a user-visible gap. That plan assumed the old screens would keep their shape. They will not. The UI and UX work in Notion replaces the four-stage spine, the global Checkoff and All Production pages, and the per-page server polling that produced 10,905 requests from one open page. Porting those screens to Rust and then replacing them would pay for the same work twice, and the parity fixtures would describe screens that no longer exist.

Two decisions follow from this and are recorded separately. First, the per-route strangler loses its purpose. The gateway proxy and the operation ownership manifest exist to serve a mix of Node and Rust routes to old screens. With one UI against one runtime there is no consumer for a half-migrated route. Second, Node and Rust never share a live data directory. The cutover is a whole-runtime move with a verified pre-cutover copy, not a route-by-route switch.

The trade-off is that no shippable Rust product exists until the new screens land. The Node product keeps shipping during the rebuild and is maintained, not grown. Bug fixes on old screens are limited to correctness and data safety; new behaviour waits for the new UI. The old screens also stop being a reference implementation, so anything worth keeping from them (export formats, checkoff matching rules, plate packing) moves to Rust with golden tests before the screens are deleted.

Deleted with the old screens: `web/apps/web` page code, `web/packages/contracts` as a hand-maintained mirror, and the UI v11 tickets that describe screens of the old shape. Kept and re-scoped: design tokens (GRE-1008), and the contract tickets for printers, discovery, cancel, queue and the event stream, because the new UI needs them and they are no longer "after parity".
