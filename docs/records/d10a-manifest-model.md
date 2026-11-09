# D10a manifest model receipt

GRE-604 D10a adds `pp_storage::manifest_options::KitManifest` behind `desktop_rebuild`.
The pure model parses request and stored JSON, normalizes selection variant IDs,
projects typed selections, and emits JavaScript-compatible JSON with UTF-16 text
and ordered objects. Input errors emit lossless JSON detail bodies.
The codec enforces 8 MiB, 64 levels, and 100,000 nodes.
Focused codec unit tests and public model integration tests run in the desktop gates.
