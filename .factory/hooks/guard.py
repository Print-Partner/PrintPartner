#!/usr/bin/env python3
"""Factory Droid adapter for scripts/agent-guard.py.

Droid runs this before Execute, Create, Edit, and ApplyPatch calls, including
inside Missions. It maps Droid's PreToolUse payload onto the shared policy in
scripts/agent-guard.py, which other runners and people call directly. Exit 2
blocks the tool call.
"""

import importlib.util
import json
import os
from pathlib import Path
import sys

POLICY = Path(__file__).resolve().parents[2] / "scripts" / "agent-guard.py"


def load_policy():
    spec = importlib.util.spec_from_file_location("agent_guard", POLICY)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def decide(payload):
    policy = load_policy()
    if not isinstance(payload, dict):
        return "Expected a JSON object."
    tool = payload.get("tool_name", "")
    data = payload.get("tool_input", {})
    if not isinstance(data, dict) and not (tool == "ApplyPatch" and isinstance(data, str)):
        return "Expected object tool input, or a patch string for ApplyPatch."
    if tool == "Execute":
        return policy.check_command(str(data.get("command", "")), str(payload.get("cwd") or os.getcwd()))
    if tool in ("Create", "Edit"):
        return policy.check_paths([str(data.get("file_path", ""))])
    if tool == "ApplyPatch":
        text = data if isinstance(data, str) else str(data.get("input") or data.get("patch") or "")
        return policy.check_paths(policy.patch_paths(text))
    return None


def main():
    try:
        payload = json.load(sys.stdin)
    except json.JSONDecodeError:
        print("pp guard: unreadable hook input", file=sys.stderr)
        return 2
    # Any failure to load or run the policy blocks the call; a crash would otherwise let it through.
    try:
        reason = decide(payload)
    except Exception as error:
        print(f"pp guard: internal error: {error!r}", file=sys.stderr)
        return 2
    if reason:
        print(f"pp guard: {reason}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
