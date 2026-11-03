set positional-arguments

# Run the required CI checks locally before you push. The steps live in
# scripts/local-ci.py; see docs/development/ci.md. Example: just ci --skip nextest-on
ci *args:
    python3 scripts/local-ci.py "$@"

# Offline checks for agent guidance, the agent safety guard, and staged Rust snapshots.
guidance:
    python3 -B -m unittest discover -s scripts -p test_agent_guidance.py -v
    python3 -B scripts/check-agent-guidance.py
    python3 -B -m unittest discover -s scripts -p test_factory_hooks.py -v
    python3 -B -m unittest discover -s scripts -p test_pre_commit_sparse_snapshot.py -v

# Opt in to the staged-change checks in .githooks/pre-commit.
hooks:
    git config core.hooksPath .githooks
