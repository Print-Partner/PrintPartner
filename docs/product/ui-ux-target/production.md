# Production and sends UI target

[UI and UX target](../ui-ux-target.md) owns the shared design rules.
This page owns this feature's contracts and delivery slices.

## Contracts

Status terms and the Rust-only rule are defined in the [contract overview](../ui-ux-target.md#contracts-the-ui-needs).

| Need | Contract | Status |
| --- | --- | --- |
| Plates workspace, initialize, arrange, edits and restore | `GET /plans/{id}/plates`, `POST /plans/{id}/plates/initialize`, `/arrange`, `/restore`, and the unit routes under `/plans/{id}/plates/{plateId}/units/{token}` (also at `/api/v2`) | Exists in Node; served from Rust behind `desktop_rebuild` (GRE-419); see [Accepted Plate routes in Rust](../../records/d19-publication-client.md#accepted-plate-routes-in-rust-gre-419) |
| Production setup | `GET` and `PATCH /plans/{id}/production-setup` (root only, as in Node) | Exists in Node; served from Rust behind `desktop_rebuild` (GRE-1209); see [Production setup in Rust](../../records/d19-publication-client.md#production-setup-in-rust-gre-1209) |
| Send queue | `GET /api/v2/printer-send-queue`; `POST .../{id}/dispatch`, `.../drain`, `.../{id}/reconcile`, `DELETE .../{id}` | Exists in Rust behind `desktop_rebuild`: the read (GRE-1125), plus Send now, Send ready, Reconcile and Remove with the upload worker (GRE-365). Adding to the queue is still Node only. Reconcile has no UI yet (GRE-1219). See [Send queue](../../send-queue.md) |

## Delivery slices

Numbers preserve the [product build order](../ui-ux-target.md#build-order); Linear owns execution state.

| # | Slice | Usable result |
| --- | --- | --- |
| 10 | Production, send sheet and risk-scaled cancel confirmation | Send and cancel explicitly |
