set positional-arguments

# Run the required CI checks locally before you push. The steps live in
# scripts/local-ci.py; see docs/development/ci.md. Example: just ci --skip nextest-on
ci *args:
    python3 scripts/local-ci.py "$@"

# Print the maintainability measures read from one revision (default origin/main).
# A report, never a check. See docs/development/ci.md#maintainability-report.
# Example: just report --rev 044e7706d --ci 1626
# PR evidence: just report --rev HEAD --base origin/main   (only the rows the branch changes)
report *args:
    python3 -I scripts/maintainability-report.py "$@"

# Offline checks for agent guidance, the agent safety guard, and staged Rust snapshots.
guidance:
    python3 -B -m unittest discover -s scripts -p test_agent_guidance.py -v
    python3 -B scripts/check-agent-guidance.py
    python3 -B -m unittest discover -s scripts -p test_agent_guard.py -v
    python3 -B -m unittest discover -s scripts -p test_pre_commit_sparse_snapshot.py -v

# Opt in to the staged-change checks in .githooks/pre-commit.
hooks:
    git config core.hooksPath .githooks
