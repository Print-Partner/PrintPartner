# API-key HTTP parity

The [auth-key fixture](../../fixtures/parity/auth-key/README.md) compares the complete Node and Rust response bodies for disposable API keys in the default and `desktop_rebuild` auth clients. It covers revocation, regeneration, stored expiry, and tenant isolation.

Unknown and foreign key IDs return HTTP 404. Node answers `{"detail":"API key not found","title":"Not Found","status":404}`. Rust answers every auth failure as a [problem response](../problem-responses.md); this one is `{"type":"urn:print-partner:problem:not-found","title":"Not Found","status":404,"detail":"API key not found"}`.

Run the fixture's check commands in both feature modes when changing API-key response serialization. The fixture guide documents normalization, secret-redaction controls, and regeneration.
