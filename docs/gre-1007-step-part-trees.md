# STEP part trees (GRE-1007)

occt-import-js 0.0.23 took 417 s and about 2.7 GB to load the 185 MB Trident STEP file, and returned a part tree only 2 levels deep (its bug #60). Print Partner reads the tree from the file's product structure instead, in Rust, in four steps:

1. GRE-1007: [`pp_domain::step_assembly::product_structure`](../rust/crates/pp-domain/src/step_assembly.rs) reads a STEP file and returns its checked product-structure records. Its module docs state the input contract: the ISO 10303-21 forms it accepts, the typed errors it refuses everything else with, and its limits.
2. GRE-1855: build the full nested part tree from those records, capped at 60 levels and 100,000 nodes.
3. GRE-1801: a background worker converts each file once per SHA-256 and caches the tree.
4. GRE-1785: the gateway route and Library's queued, converting, done and failed states.

The reader takes any `Read`, so it needs no webview file access. GRE-1007 adds no Tauri capability or plugin permission.
