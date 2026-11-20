# STEP part trees (GRE-1007)

occt-import-js 0.0.23 took 417 s and about 2.7 GB to load the 185 MB Trident STEP file, and returned a part tree only 2 levels deep (its bug #60). Print Partner reads the tree from the file's product structure instead, in Rust, in four steps:

1. GRE-1007: [`pp_domain::step_assembly::product_structure`](../rust/crates/pp-domain/src/step_assembly.rs) reads a STEP file and returns its checked product-structure records. Its module docs state the input contract: the ISO 10303-21 forms it accepts, the typed errors it refuses everything else with, and its limits.
2. GRE-1855: [`pp_domain::step_assembly::part_tree`](../rust/crates/pp-domain/src/step_assembly/tree.rs) builds the full nested part tree from those records. See [The part tree](#the-part-tree).
3. GRE-1801: a background worker converts each file once per SHA-256 and caches the tree.
4. GRE-1785: the gateway route and Library's queued, converting, done and failed states.

The reader takes any `Read`, so it needs no webview file access. GRE-1007 adds no Tauri capability or plugin permission.

## The part tree

`part_tree` turns the checked records into a `PartTree`: a list of root `PartNode`s, each with its `product_id`, `name`, `instance` (the occurrence's name, or its id when the name is empty; `null` for a root) and `children`. Both types serialize with serde, field names as written.

- A definition that no occurrence places is a root. Roots come in entity-number order.
- Each occurrence is one child, in entity-number order. A sub-assembly placed twice appears twice, with its whole subtree each time.
- A definition that contains itself, directly or through its children, is refused as `StepError::Cycle { entity }`, whether or not a root reaches it. A file with no `PRODUCT_DEFINITION` is refused as `StepError::NoProducts`.
- The build is iterative. Before each node is created it checks the cancel flag and `TreeLimits::default()`, and refuses with `StepError::TooLarge(Limit::..)`:

| Limit | Default | Variant |
| --- | --- | --- |
| Nodes in the tree | 100,000 | `Nodes` |
| Levels; a root is level 1 | 60 | `Levels` |
| Bytes of every node's product id, name and instance | 32 MiB | `NameBytes` |

60 levels keeps the tree's JSON at 122 nesting levels, under serde_json's limit of 128, so the cached tree reads back. The cycle search before the build visits each definition and occurrence once and checks the cancel flag once per definition.
