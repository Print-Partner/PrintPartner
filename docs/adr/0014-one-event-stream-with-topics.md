# One authenticated event stream with topics

Status: accepted on 2026-11-02.

Live updates travel on one authenticated WebSocket with topics, not one socket per resource.

The framing is the same for every topic. On connect the server sends a full snapshot for each subscribed topic, then ordered deltas carrying a per-topic sequence number. On reconnect the client sends its last sequence and receives the missed deltas from a bounded buffer, or a fresh snapshot when the gap is too large. A gap is never silently ignored, because a silently dropped delta is a wrong screen.

Topics are `printers`, `jobs`, `plans`, `queue` and `stock`. The existing jobs socket becomes the `jobs` topic, and the approved printer stream (GRE-1014) becomes the `printers` topic. Printers that support push are pushed; the rest are polled inside their driver, so every client sees one stream shape regardless of protocol.

This continues work that already exists rather than replacing it. The jobs socket already solved authenticated upgrades, authoritative snapshots as the first message, and projection invalidation after a job completes. Generalizing that is cheaper than designing a second socket, and it removes the polling that made a single open Checkoff page issue 10,905 requests.

The rejected alternative was one socket per resource, which saves the multiplexing logic but couples less well in every other direction: each socket needs its own authentication, its own reconnect and its own snapshot rules, and the client has to manage a pool of connections whose failure modes differ. A second alternative, a single shared socket with no topics and no sequence numbers, is simpler still but cannot tell a client what it missed.

Two rules constrain the stream. A printer send is never triggered by a message on it; sends are explicit commands from a user action. And event payloads carry identifiers and typed state, never secrets, customer names or file bytes, because a broadcast summary is the easiest place for a redaction bug to reach every connected client.
