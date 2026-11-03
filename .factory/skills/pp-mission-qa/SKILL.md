---
name: pp-mission-qa
description: Start, drive, and stop Print Partner during Mission user testing. Use for local or Factory Environment QA of a migration slice.
---

# Mission user testing

Read [AGENTS.md](../../../AGENTS.md) and the slice's acceptance criteria, or its exported [task brief](../../../docs/development/agent-workflow.md#start-from-a-task-brief). Static checks and unit tests belong to `pp-readiness`. User testing uses the existing launchers in the shared [agent and contributor workflow](../../../docs/development/agent-workflow.md#verify-in-a-running-app), not a second QA platform:

1. [Choose the runtime](../../../docs/development/agent-workflow.md#choose-the-runtime) that owns the operation under test, and write down the HEAD, fixture, expected result, flag modes, time budget, and cleanup before launching.
2. Drive the [Node server and shared UI](../../../docs/development/agent-workflow.md#web-ui) or the [Rust gateway](../../../docs/development/agent-workflow.md#rust-gateway). Assert the slice's expected visible state through the owning runtime, not just a successful click, navigation, or a bare `401`.
3. [Report and clean up](../../../docs/development/agent-workflow.md#report-and-clean-up). Report a missing browser, display, or native tool as BLOCKED, never as a pass.
