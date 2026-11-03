#!/usr/bin/env python3
"""Claude Code adapter for scripts/agent-guard.py.

A PreToolUse hook with the matcher Bash|Write|Edit|MultiEdit|NotebookEdit runs this
before each of those tools. docs/development/agent-workflow.md has the settings
snippet. Bash maps to check_command(command, cwd); the file tools map to
check_paths. Exit 2 blocks the tool call. Anything this adapter does not
understand, including an unknown tool, a missing field, or an internal error,
also exits 2, so a bad payload never lets a call through.
"""

import importlib.util
import json
from pathlib import Path
import sys

POLICY = Path(__file__).resolve().parents[2] / "scripts" / "agent-guard.py"
PATH_FIELDS = {"Write": "file_path", "Edit": "file_path", "MultiEdit": "file_path", "NotebookEdit": "notebook_path"}


def load_policy():
    spec = importlib.util.spec_from_file_location("agent_guard", POLICY)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def field(data, name, where):
    value = data.get(name)
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f"{where}.{name} must be a non-empty string")
    return value


def decide(payload):
    if not isinstance(payload, dict):
        raise ValueError("expected a JSON object")
    tool, data = field(payload, "tool_name", "payload"), payload.get("tool_input")
    if not isinstance(data, dict):
        raise ValueError("tool_input must be an object")
    policy = load_policy()
    if tool == "Bash":
        cwd = policy.directory(field(payload, "cwd", "payload"))
        return policy.check_command(field(data, "command", "tool_input"), cwd)
    if tool in PATH_FIELDS:
        return policy.check_paths([field(data, PATH_FIELDS[tool], "tool_input")])
    raise ValueError(f"unsupported tool {tool!r}")


def main():
    # Any failure to read the payload or run the policy blocks the call; a crash would otherwise let it through.
    try:
        reason = decide(json.load(sys.stdin))
    except Exception as error:
        print(f"pp guard: refused unreadable or unsupported hook input: {error!r}", file=sys.stderr)
        return 2
    if reason:
        print(f"pp guard: {reason}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
