#!/usr/bin/env python3
"""Claude Code adapter for scripts/agent-guard.py.

A PreToolUse hook with the matcher Bash|Write|Edit|MultiEdit|NotebookEdit runs this
before each of those tools. docs/development/agent-workflow.md has the settings
snippet. Bash maps to check_command(command, cwd); the file tools map to
check_paths. Grok and Cursor read the same settings and match Bash to their own
shell tools, run_terminal_command and Shell, which map to check_command too. Exit 2 blocks the tool call. Anything this adapter does not
understand, including an unknown tool, a missing field, or an internal error,
also exits 2, so a bad payload never lets a call through.
"""

import importlib.util
import json
import os
from pathlib import Path
import sys

POLICY = Path(__file__).resolve().parents[2] / "scripts" / "agent-guard.py"
PATH_FIELDS = {"Write": "file_path", "Edit": "file_path", "MultiEdit": "file_path", "NotebookEdit": "notebook_path"}
# Grok's and Cursor's shell tools. Both send tool_input.command; either may name its own directory in tool_input.cwd.
OTHER_SHELLS = {"run_terminal_command", "Shell"}


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


def shell_directory(policy, payload, data):
    """The directory a Grok or Cursor shell call runs in, or None when it is unknown.

    Both CLIs can keep one shell across calls, so a call runs wherever an earlier cd left it unless it
    names an absolute tool_input.cwd. The payload cwd is only the session's directory, so it never
    stands in for the shell's. None applies the policy's rules for an unknown directory, which refuse
    coordinator merges and pushes it cannot place.
    """
    base, own = payload.get("cwd", ""), data.get("cwd", "")
    if not isinstance(base, str) or not isinstance(own, str):
        raise ValueError("cwd and tool_input.cwd must be strings")
    if base.strip():
        policy.directory(base)  # A malformed session directory still refuses the payload.
    return policy.directory(own) if os.path.isabs(own) else None


def decide(payload):
    if not isinstance(payload, dict):
        raise ValueError("expected a JSON object")
    tool, data = field(payload, "tool_name", "payload"), payload.get("tool_input")
    if not isinstance(data, dict):
        raise ValueError("tool_input must be an object")
    # Grok cuts long tool input short and says so; a shortened command could hide what follows the cut.
    if payload.get("toolInputTruncated") not in (None, False):
        raise ValueError("tool input was truncated")
    policy = load_policy()
    if tool == "Bash":
        cwd = policy.directory(field(payload, "cwd", "payload"))
        return policy.check_command(field(data, "command", "tool_input"), cwd)
    if tool in OTHER_SHELLS:
        return policy.check_command(field(data, "command", "tool_input"), shell_directory(policy, payload, data))
    if tool in PATH_FIELDS:
        cwd = policy.directory(field(payload, "cwd", "payload"))
        return policy.check_paths([field(data, PATH_FIELDS[tool], "tool_input")], cwd)
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
