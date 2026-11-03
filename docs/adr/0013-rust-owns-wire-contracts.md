# Rust owns the wire contracts

Status: accepted on 2026-11-02.

Rust is the source of truth for wire types. DTOs carry `schemars` derives, and the existing `web/scripts/rust-contracts/generate.mjs` pipeline emits TypeScript types and Zod validators into `web/packages/api`. CI regenerates and fails on any diff. Nothing hand-edits that package.

The direction is currently inverted. Today Zod schemas are the master, JSON Schema is derived from them, `typify` generates 3,976 lines of Rust into `pp-api-types/src/generated.rs`, and a separate file mirrors the shapes back to TypeScript. With the old screens going away there is no TypeScript master worth keeping: the only remaining consumer of the Zod schemas is request validation, which Rust already does.

`schemars` is chosen over the alternatives for one reason: MCP needs JSON Schema at runtime, because clients read `inputSchema` to call a tool. That derive already exists in the workspace. Using it as the single source means one annotation feeds three consumers, the generated TypeScript client, the MCP tool schemas, and request validation.

The alternatives were considered and rejected on cost. Adding `ts-rs` derives would mean a second annotation on every DTO and would still leave MCP needing `schemars`, so the workspace would carry two derive systems for one job. `utoipa` plus `openapi-typescript` would be a third description of the same types and a second generator, and the project has no external API consumers that need an OpenAPI document.

Two constraints follow. First, the pinned `schemars` version is 0.8.22, while the MCP library uses 1.x, so the upgrade has to land before the derivation is trusted; it is a prerequisite of its own slice rather than a side effect. Second, `pp-api-types/src/generated.rs` does not disappear by itself. Retiring `typify` orphans 3,976 lines unless a slice owns writing the replacements, so that slice is a named ticket rather than an assumption.

On REST paths: only MCP needs stability, because the cursor plugin is MCP-only and the Home Assistant integration is outbound-only. REST shapes are free to change, and this design does not spend effort preserving them.
