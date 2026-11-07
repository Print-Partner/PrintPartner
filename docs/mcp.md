# MCP

Print Partner exposes product operations to AI assistants over the Model Context Protocol. This page will describe the MCP server in the Rust core. It is a stub until the API milestone ports MCP from Node.

[MCP setup](assistant-mcp.md) describes how to connect a client to the current Node server.

## Rules the Rust port keeps

- MCP runs over streamable HTTP at `/api/v1/mcp`.
- Every request needs an API key, including requests from the same machine.
- Read tools return current data. Write tools return a proposal and change nothing until the client calls `confirm_apply`.
- Tool names and schemas stay the same, so existing clients keep working.

## Ported tools

- `get_build_workflow` reads the Build workflow with the request's API key, the same projection as `GET /plans/{id}/workflow` and the task feed ([Build workflow](build-workflow.md)). Errors answer `{"error": "plan_id required"}`, `{"error": "Build not found"}` or `{"error": "Build workflow is unavailable"}`. The last covers a storage or authentication failure, a Build that fails its integrity checks, and a body that can't be serialized. Every error sets `isError`.

- `propose_apply_plan_draft` `{ plan_id, draft_id }` and its `confirm_apply` accept a Working Plan through the same Accept preview and Accept as the Plan tab ([Draft headers](draft-headers.md#accept)). Propose checks the hydrated Build planning brief, reads the preview, writes nothing, and proposes the previewed revision. Confirm checks the brief again and accepts exactly that revision with the action id as the idempotency key; a header saved in between is refused with `draft_changed`. A draft with no Required-unit selection is proposed with `revision: null`, and confirm selects the automatic reconciliation before it previews and accepts. Propose errors: `plan_id and draft_id required`, `Build planning brief not found`, `Selected Working Plan not found`, `Working Plan cannot apply source choices: …`, `Working Plan acceptance requires required-unit decisions`, `Working Plan cannot be accepted` (Accept would refuse, or the preview lists running work or production), `Build not found` and `Working Plan preview is unavailable`. Confirm answers `{ "ok": false, "detail": … }`, including `Working Plan acceptance preparation failed: <kind>`, and keeps the action pending.

Every other product tool still answers that it has no Rust port.

## To be written

- How to create and rotate the API key in the desktop app and the Docker image.
- How MCP access works when network sharing is on.
- The tool list and its schemas.
