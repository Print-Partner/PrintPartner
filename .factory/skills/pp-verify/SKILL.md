---
name: pp-verify
description: Run Print Partner and capture screenshot or video evidence for a web UI or desktop change. Use before opening or updating a PR that touches web/apps/web or rust/crates/pp-desktop.
---

# Verify a Print Partner change

The Evidence lint fails a PR that changes the web UI or desktop app unless Evidence has an HTTPS screenshot or video. Run the real app at the PR head and look at it.

The steps live in the shared [agent and contributor workflow](../../../docs/development/agent-workflow.md#verify-in-a-running-app), so every agent and person uses the same ones. Follow the section that matches the change:

- [Web UI](../../../docs/development/agent-workflow.md#web-ui): launch the Node server and React UI with the repository helper on fresh data and free loopback ports, then screenshot each affected screen.
- [Dialogs and other client-only state](../../../docs/development/agent-workflow.md#dialogs-and-other-client-only-state): interact and capture in one Playwright program. The helper's separate `click` and `screenshot` commands lose an open dialog. The `agent-browser` skill also works if one session covers the interaction and the capture.
- [Rust gateway](../../../docs/development/agent-workflow.md#rust-gateway): use the authenticated launcher and its one-use bootstrap credential in a private temporary directory. Never print the credential.
- [Desktop app](../../../docs/development/agent-workflow.md#desktop-app): capture the native app built at the PR head. A browser screenshot is not native evidence.
- [Check and attach evidence](../../../docs/development/agent-workflow.md#check-and-attach-evidence): open every file, check it shows only the app and the change, then attach it with the head SHA.
