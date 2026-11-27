# Domain

This folder holds one page per product area. Each page describes the area's rules and which runtime owns them, Node or Rust. A page is added in the same PR that ports its area to Rust with parity fixtures.

## Core model

A **Library Source** points to a Git repository, a local directory or an uploaded archive. A sync creates a source revision and records the STL files and metadata at that revision.

A **Build** selects one or more sources. Its draft records file choices, quantities, roles, colors and manifest options. Applying the draft creates the accepted Plan that Checkoff and Production use.

An accepted Plan revision owns:

- Required units.
- Checkoff state.
- Plate arrangements.
- Exported artifacts.
- Printer handoff records.

A change to source inputs invalidates the draft or accepted state that depends on it. Explicit revision checks detect this. The UI does not infer freshness from timestamps.

## Areas

| Area | What the page will cover | Current reference |
| --- | --- | --- |
| Sources | Import, scan, sync, file watching, and PDF and notes attachments. A sync prepares the new revision away from the active path and publishes it only after validation. Readers keep the previous revision until then. | [Source snapshots](../../rust/crates/pp-source/README.md) |
| Planning and Checkoff | Autosave, Required units, Plates and Plan publication. | [Plan autosave](../plan-autosave.md) |
| Exports | STL packs, HTML checklists, 3MF and kit bundles. Each accepted Plate revision is immutable, so downloads and handoffs stay tied to the Plan that produced them. | [3MF export validation](../3MF_EXPORT_VALIDATION.md) |
| Printer drivers and send queue | Moonraker, PrusaLink and Bambu status, and the send queue. | [Printer setup](../integrations/PRINTER_SETUP.md) |
| Spoolman | Filament inventory and usage deduction. | [Spoolman](../integrations/SPOOLMAN.md) |
| Slicer handoff | Delivery of a managed Plate's verified 3MF to a configured local inbox. Slicing stays in the slicer. | [Local slicer handoff](../api.md#local-slicer-handoff) |
| Integrations | Home Assistant, webhooks and Discord, and profile sync. | none yet |
