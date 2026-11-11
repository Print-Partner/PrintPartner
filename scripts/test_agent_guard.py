import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys
import tempfile
import tracemalloc
import time
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
IN_PROCESS = {}


def in_process(target):
    """Return the module for a guard entry point, loaded once. The adapters share one copy of the policy."""
    if not IN_PROCESS:
        policy = load(ROOT / "scripts/agent-guard.py", "agent_guard_in_process")
        IN_PROCESS[ROOT / "scripts/agent-guard.py"] = policy
        for path, name in ((ROOT / ".claude/hooks/guard.py", "claude_adapter"),):
            adapter = load(path, name)
            adapter.load_policy = lambda policy=policy: policy
            IN_PROCESS[path] = adapter
    return IN_PROCESS.get(target)


def spawn(cmd, input=None, env=None, cwd=None, **kwargs):
    """Run a command like subprocess.run, but run the Claude hook adapter and the guard CLI in this process.

    Spawning Python and recompiling the guard for each of about a thousand calls made the suite outrun the
    CI job. The entry point still reads stdin, the environment, its arguments and the working directory,
    and its exit code and stderr come back as a CompletedProcess. Any other command, such as an adapter
    copied elsewhere, still runs as a real process.
    """
    target = Path(cmd[2]) if len(cmd) >= 3 and cmd[:2] == [sys.executable, "-B"] else None
    module = in_process(target) if target else None
    if module is None or (target.name != "agent-guard.py" and len(cmd) != 3):
        return subprocess.run(cmd, input=input, env=env, cwd=cwd, **kwargs)
    out, err = io.StringIO(), io.StringIO()
    with patch.dict(os.environ, dict(os.environ) if env is None else env, clear=True), \
            patch.object(sys, "stdin", io.StringIO(input or "")), patch.object(sys, "argv", [str(target), *cmd[3:]]), \
            contextlib.redirect_stdout(out), contextlib.redirect_stderr(err), contextlib.chdir(cwd or os.getcwd()):
        try:
            code = module.main()
        except SystemExit as error:
            if isinstance(error.code, str):
                print(error.code, file=err)
            code = error.code if isinstance(error.code, int) else int(error.code is not None)
    return subprocess.CompletedProcess(cmd, code, out.getvalue(), err.getvalue())

CLI = ROOT / "scripts/agent-guard.py"


def tool_request(payload):
    """The neutral request for a tool-call payload: Execute is a command, Create and Edit a file, ApplyPatch a patch."""
    tool, data = payload["tool_name"], payload["tool_input"]
    if tool == "Execute":
        return {"operation": "command", "cwd": payload.get("cwd") or str(ROOT), "command": data["command"]}
    if tool in ("Create", "Edit"):
        return {"operation": "file", "path": data["file_path"]}
    if tool == "ApplyPatch":
        return {"operation": "patch", "patch": data if isinstance(data, str) else data["input"]}
    raise ValueError(f"no neutral operation for {tool}")


def guard_call(payload, coordinator=None, extra=None):
    """Run a tool-call payload through scripts/agent-guard.py json, with hook()'s coordinator environment."""
    return hook(CLI, tool_request(payload), coordinator, extra, None, "json")


def run(tool, data):
    return guard_call({"tool_name": tool, "tool_input": data})


class GuardHookTests(unittest.TestCase):
    def assertBlocked(self, tool, data, reason):
        result = run(tool, data)
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn(reason, result.stderr)

    def assertAllowed(self, tool, data):
        result = run(tool, data)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_merges_are_blocked(self):
        self.assertBlocked("Execute", {"command": "gh pr merge 12 --squash"}, "maintainer")
        for options in ("-X PUT", "-XPUT", "--method PUT", "--method=PUT"):
            self.assertBlocked("Execute", {"command": f"gh api {options} repos/o/r/pulls/12/merge"}, "maintainer")
            self.assertBlocked("Execute", {"command": f"gh api repos/o/r/pulls/12/merge {options}"}, "maintainer")

    def test_merge_status_reads_are_allowed(self):
        for options in ("", "-X GET", "--method GET", "--method=GET"):
            self.assertAllowed("Execute", {"command": f"gh api {options} repos/o/r/pulls/12/merge"})

    def test_shell_continuations_are_normalized(self):
        for newline in ("\n", "\r\n"):
            self.assertBlocked("Execute", {"command": f"git push \\{newline} --force origin topic"}, "force-push")
            self.assertBlocked("Execute", {"command": f"git push \\{newline} origin HEAD:main"}, "main only")
            self.assertAllowed("Execute", {"command": f"git push \\{newline} origin topic"})

    def test_force_pushes_and_main_pushes_are_blocked(self):
        for command in ("git push --force origin topic", "git push -f origin topic",
                        "git push origin +topic", "git push --force-with-lease"):
            self.assertBlocked("Execute", {"command": command}, "force-push")
        self.assertBlocked("Execute", {"command": "git push origin HEAD:main"}, "main only")
        self.assertBlocked("Execute", {"command": "git push origin main"}, "main only")
        for destination in ("heads/main", "refs/heads/main"):
            self.assertBlocked("Execute", {"command": f"git push origin HEAD:{destination}"}, "main only")
        self.assertBlocked("Execute", {"command": 'git -C "/repo with spaces" push origin HEAD:main'}, "main only")
        self.assertBlocked("Execute", {"command": "git -C /repo push --force origin topic"}, "force-push")

    def test_releases_and_hook_bypass_are_blocked(self):
        self.assertBlocked("Execute", {"command": "gh release create v1"}, "authorization")
        self.assertBlocked("Execute", {"command": "git push --tags"}, "authorization")
        self.assertBlocked("Execute", {"command": "git commit --no-verify -m x"}, "hooks")
        self.assertBlocked("Execute", {"command": "git -C /repo push --no-verify origin topic"}, "hooks")

    def test_ordinary_commands_pass(self):
        for command in ("git push -u origin HEAD:chore/topic", "gh pr view 12",
                        "git push origin feature/maintenance", "npm --prefix web ci",
                        "git -C . push origin main-fix",
                        "git log --oneline main"):
            self.assertAllowed("Execute", {"command": command})

    def test_push_rules_stop_at_shell_command_boundaries(self):
        for separator in (" && ", "; ", "\n", " | "):
            for following in ("gh pr create --base main", "echo --force", "echo --tags"):
                self.assertAllowed("Execute", {"command": f"git push -u origin HEAD:topic{separator}{following}"})
            self.assertBlocked("Execute", {"command": f"echo ok{separator}git push origin HEAD:main"}, "main only")
            self.assertBlocked("Execute", {"command": f"git push origin HEAD:main{separator}echo ok"}, "main only")

    def test_command_literals_and_heredoc_data_are_allowed(self):
        for command in ("rg -- '--no-verify' .factory",
                        "printf '%s\\n' 'gh pr merge 12 --squash'",
                        "git log --grep 'gh pr merge'",
                        "gh pr list --search '--no-verify'",
                        "git log --grep --no-verify",
                        "git log --format='%h %s'",
                        'git commit --message="Fix guard"',
                        "git commit -m \"$(cat <<'EOF'\nFix user's settings (part\nEOF\n)\"",
                        "printf '%s\\n' /tmp/a\\ b",
                        "git commit -m 'note; git push --force'",
                        "git log # git push origin main",
                        "python3 - <<'PY'\nprint('git push --force')\nPY\n",
                        "python3 - <<'PY'\nprint(\"unmatched shell quote: '\")\nPY\n"):
            self.assertAllowed("Execute", {"command": command})
        self.assertBlocked("Execute", {"command": "python3 - <<'PY'\nprint('ok')\nPY\ngit push origin main"}, "main only")

    def test_quoted_push_arguments_and_safe_force_option(self):
        self.assertBlocked("Execute", {"command": 'git push origin "HEAD:main"'}, "main only")
        self.assertBlocked("Execute", {"command": "git push '--force' origin topic"}, "force-push")
        self.assertAllowed("Execute", {"command": "git push --force-if-includes origin HEAD:topic"})
        for command in ("git push -fu origin topic", "git push -uvf origin topic", "git push -fuv origin topic"):
            self.assertBlocked("Execute", {"command": command}, "force-push")
        self.assertBlocked("Execute", {"command": "echo $(git push origin main)"}, "main only")
        self.assertBlocked("Execute", {"command": 'output="$(# 1) Push the branch\ngit push origin main\n)"'}, "main only")
        self.assertAllowed("Execute", {"command": "echo \"$(# user's note\ngit log -1\n)\""})
        for command in ("git push -h", "git push --help"):
            self.assertAllowed("Execute", {"command": command})
        self.assertBlocked("Execute", {"command": "git push origin 'main"}, "Cannot parse")

    def test_argument_vectors_classify_git_and_gh(self):
        for command in ('git commit -m "Document --no-verify behavior"',
                        "git commit -m -n", "git merge -n topic", "git commit -mnote",
                        "gh api graphql -f query='{ viewer { login } }'"):
            self.assertAllowed("Execute", {"command": command})
        for command, reason in (("git commit -n -m Change", "hooks"), ("git commit -anm x", "hooks"),
                                ("GIT_TERMINAL_PROMPT=0 git push --force origin topic", "force-push"),
                                ("env GIT_TRACE=1 git push origin main", "main only"),
                                ("/usr/bin/git push origin main", "main only"),
                                ("git -c color.ui=false push --force origin topic", "force-push"),
                                ("git -c color.ui=false push origin HEAD:main", "main only"),
                                ("git --git-dir=.git push origin main", "main only"),
                                ("gh -R o/r pr merge 12", "maintainer"),
                                ("gh api graphql -f query='mutation { mergePullRequest(input: {}) { clientMutationId } }'",
                                 "maintainer")):
            self.assertBlocked("Execute", {"command": command}, reason)

    def test_heredoc_quoting_and_here_strings_keep_later_commands(self):
        for command in ("cat <<EOF\nE\\\nOF\ngit push origin main\n",
                        "cat <<\\EOF\nhello\nEOF\ngit push origin main",
                        "cat <<E'O'F\nhello\nEOF\ngit push origin main",
                        'cat <<< "hello"\ngit push origin main'):
            self.assertBlocked("Execute", {"command": command}, "main only")

    def test_shell_syntax_around_commands(self):
        for command in ("if git push origin main; then echo ok; fi", "! git push origin main",
                        "{ git push origin main; }", "2>/dev/null git push origin main",
                        'output="$(git push origin main)"', "echo `git push origin main`",
                        "cat <<'EOF'\nline \\\nEOF\ngit push origin main",
                        "env -u GIT_TRACE git push origin main", "/usr/bin/time -f %E git push origin main",
                        "sudo -u dev git push origin main", "timeout 60 git push origin main",
                        "timeout -k 5 60 git push origin main", "nice -n 5 git push origin main",
                        "git push origin HEAD:main <<< --dry-run",
                        'output="$(case x in x) git push origin main;; esac)"',
                        "x=$(if true; then case a in a) git push origin main;; esac; fi)",
                        "0<<EOF git push origin HEAD:main\nbody\nEOF", "$'git\\0ignored' push origin main"):
            self.assertBlocked("Execute", {"command": command}, "main only")
        self.assertBlocked("Execute", {"command": "<<< x gh pr merge 12"}, "maintainer")
        self.assertBlocked("Execute", {"command": "0<<< x gh pr merge 12"}, "maintainer")
        for command in ("git push origin topic >main 2>&1", 'echo "$(case x in (y) echo ok;; esac)"',
                        'echo "$(case a in esac)"', 'echo "$(cat <<< hello\n)"', 'echo "$(cat <<EOF\nE\\\nOF\n)"',
                        "git config core.hooksPath",
                        "echo $(echo showcase) && git status"):
            self.assertAllowed("Execute", {"command": command})
        self.assertBlocked("Execute", {"command": "env -S 'git push origin main'"}, "Cannot classify")

    def test_push_modes_and_refspecs(self):
        for command in ("git push --dry-run origin HEAD:main", "git push -n --force origin topic",
                        "git push main HEAD:topic", "git push --repo=origin HEAD:topic",
                        "git push --force-if-includes -u origin HEAD:topic"):
            self.assertAllowed("Execute", {"command": command})
        for command, reason in (("git push --mirror origin", "force-push"), ("git push --all origin", "push options"),
                                ("git push --force-w origin topic", "push options"),
                                ("git push origin", "destination branch"), ("git push", "destination branch"),
                                ("git push origin 'refs/heads/*:refs/heads/*'", "destination branch"),
                                ("git push origin :", "destination branch"),
                                ("git push origin topic:", "destination branch"),
                                ("git push origin :main", "main only"), ("git push --repo origin main", "main only"),
                                ("git push origin refs/tags/v1.0.0", "authorization"),
                                ("git push origin tag v1.0.0", "authorization"),
                                ("git rebase -m --no-verify main", "hooks"),
                                ("git commit --no-veri -m change", "hooks"), ("git rebase --no-veri main", "hooks"),
                                ("git commit --no-v -m change", "hooks")):
            self.assertBlocked("Execute", {"command": command}, reason)

    def test_head_push_destination_uses_checked_out_branch(self):
        with tempfile.TemporaryDirectory() as folder:
            subprocess.run(["git", "init", "-q", "-b", "main", folder], check=True)
            payload = {"tool_name": "Execute", "cwd": folder, "tool_input": {"command": "git push origin HEAD"}}
            for branch, code in (("main", 2), ("topic", 0)):
                subprocess.run(["git", "-C", folder, "symbolic-ref", "HEAD", f"refs/heads/{branch}"], check=True)
                result = guard_call(payload)
                self.assertEqual(result.returncode, code, result.stderr)
            payload["tool_input"]["command"] = f"cd / && git -C {folder} push -u origin HEAD"
            result = guard_call(payload)
            self.assertEqual(result.returncode, 0, result.stderr)
            for command, reason in (("git -c remote.origin.push=refs/heads/topic:refs/heads/main push origin topic",
                                     "destination branch"),
                                    ("git -c push.followTags=true push origin HEAD:topic", "follow-tags"),
                                    ("git -c push.followTags push origin HEAD:topic", "follow-tags"),
                                    ("git -c push.followTags=2 push origin HEAD:topic", "follow-tags"),
                                    ("git config push.followTags true && git push origin HEAD:topic", "follow-tags"),
                                    ("git remote add other /tmp/x && git push origin HEAD:topic", "follow-tags"),
                                    ("git config remote.origin.push refs/heads/topic:refs/heads/main && "
                                     "git push origin topic", "destination branch"),
                                    ("GIT_CONFIG_COUNT=1 git push origin HEAD:topic", "GIT_CONFIG_"),
                                    ("env GIT_CONFIG_GLOBAL=/tmp/c git push origin HEAD:topic", "GIT_CONFIG_"),
                                    ("GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=core.hooksPath GIT_CONFIG_VALUE_0=/dev/null "
                                     "git commit -m change", "GIT_CONFIG_"),
                                    ("export GIT_CONFIG_PARAMETERS=x; git commit -m change", "GIT_CONFIG_")):
                payload["tool_input"]["command"] = command
                result = guard_call(payload)
                self.assertEqual(result.returncode, 2, command)
                self.assertIn(reason, result.stderr)
            subprocess.run(["git", "-C", folder, "config", "push.followTags", "true"], check=True)
            for command, code in (("git push origin HEAD:topic", 2), ("git push --no-follow-tags origin HEAD:topic", 0),
                                  ("git -c push.followTags=false push origin HEAD:topic", 0)):
                payload["tool_input"]["command"] = command
                result = guard_call(payload)
                self.assertEqual(result.returncode, code, command + result.stderr)
            subprocess.run(["git", "-C", folder, "config", "--unset", "push.followTags"], check=True)
            for command in ("git branch --show-current && git push -u origin HEAD",
                            "git config --get push.followTags; git push origin HEAD:topic",
                            "git config --get-color color.diff; git push origin HEAD:topic",
                            "git config push.followTags && git push origin HEAD:topic",
                            "git remote -v && git push origin HEAD:topic",
                            "printf '%s\\n' 'GIT_CONFIG_COUNT=1'; git push origin HEAD:topic",
                            "echo GIT_DIR=.git && git push origin HEAD",
                            "git symbolic-ref --short HEAD && git push origin HEAD",
                            "git worktree list && git push origin HEAD"):
                payload["tool_input"]["command"] = command
                result = guard_call(payload)
                self.assertEqual(result.returncode, 0, command + result.stderr)
            for command in ("git switch main && git push origin HEAD", "gh pr checkout 12 && git push origin @",
                            "env -C /tmp git push origin HEAD", "env --chdir=/tmp git push origin HEAD",
                            "(cd /tmp); git push origin HEAD", "GIT_DIR=/tmp/x/.git git push origin HEAD",
                            "env GIT_DIR=/tmp/x/.git git push origin HEAD",
                            "export GIT_WORK_TREE=/tmp/x; git push origin HEAD", "git branch -m main && git push origin HEAD",
                            "git branch --mov --force main && git push origin HEAD",
                            "git symbolic-ref --del HEAD && git push origin HEAD",
                            "git symbolic-ref HEAD refs/heads/main && git push origin HEAD",
                            "echo $(git checkout main) && git push origin HEAD"):
                payload["tool_input"]["command"] = command
                result = guard_call(payload)
                self.assertEqual(result.returncode, 2, command)
                self.assertIn("destination branch", result.stderr)

    def test_skipped_or_physical_directory_changes_leave_head_unknown(self):
        with tempfile.TemporaryDirectory() as folder:
            subprocess.run(["git", "init", "-q", "-b", "main", folder], check=True)
            subprocess.run(["git", "init", "-q", "-b", "topic", str(Path(folder, "sub"))], check=True)
            for command, code in (("cd sub && git push origin HEAD", 0),
                                  ("{ if false; then cd sub; fi; }; git push origin HEAD", 2),
                                  ("cd -P sub && git push origin HEAD", 2)):
                payload = {"tool_name": "Execute", "cwd": folder, "tool_input": {"command": command}}
                result = guard_call(payload)
                self.assertEqual(result.returncode, code, command + result.stderr)

    def test_shell_expansions_in_push_destinations_are_refused(self):
        for command in ('branch=main; git push origin "HEAD:$branch"', "git push origin HEAD:$branch",
                        "git push origin HEAD:${branch}", 'git push origin "HEAD:$(echo main)"',
                        "git push origin HEAD:`echo main`", "git push origin HEAD:{main,topic}",
                        "git push origin 'HEAD:ma?n'"):
            self.assertBlocked("Execute", {"command": command}, "destination branch")
        for command in ('git push "$remote_and_branch"', 'git push "$remote" HEAD:topic'):
            self.assertBlocked("Execute", {"command": command}, "shell expansion")
        self.assertBlocked("Execute", {"command": 'git commit "$flags" -m x'}, "Cannot classify")
        for command in ('sub=push; git "$sub" --force origin topic', "flag=no-verify; git commit --$flag -m change",
                        'git -c x=1 "$sub" origin topic', 'gh "$group" merge 12', 'gh pr "$action" 12',
                        'gh api "$endpoint" -X PUT', "gh api repos/o/r/pulls --$opt x",
                        'gh api graphql -f "$key=mutation { mergePullRequest(input: {}) { clientMutationId } }"'):
            self.assertBlocked("Execute", {"command": command}, "shell expansion")
        self.assertBlocked("Execute", {"command": 'gh api graphql -f query="mutation { $field }"'}, "Cannot classify")
        for command in ("git push origin 'HEAD:topic'", 'git commit -m "$(git rev-parse HEAD)"',
                        "git push origin '$literal'"):
            self.assertAllowed("Execute", {"command": command})

    def test_gh_options_and_endpoints(self):
        for command in ("gh pr merge --help", "gh release create --help",
                        "gh api repos/o/r/issues/1/comments -f body='Document /pulls/12/merge'",                        "gh api graphql -f query='query($q: String!) { search(query: $q, type: ISSUE, first: 1) "
                        "{ issueCount } }' -f q=mergePullRequest"):
            self.assertAllowed("Execute", {"command": command})
        for command in ("gh --repo=o/r pr merge 12", "gh -Ro/r pr merge 12",
                        "gh api -X PUT 'repos/o/r/pulls/12/merge?merge_method=squash'"):
            self.assertBlocked("Execute", {"command": command}, "maintainer")
        self.assertBlocked("Execute", {"command": "gh --repo=o/r release create v1"}, "authorization")
        for command in ("gh release edit v1.2.3 --draft=false", "gh release delete v1", "gh release upload v1 a.zip"):
            self.assertBlocked("Execute", {"command": command}, "authorization")
        for command in ("gh release view v1", "gh release list", "gh release edit --help", "gh release --help"):
            self.assertAllowed("Execute", {"command": command})

    def test_effective_options_and_hidden_inputs(self):
        for command, reason in (("git push --dry-run --no-dry-run --force origin topic", "force-push"),
                                ("gh pr merge 12 --squash --subject --help", "maintainer"),
                                ("gh api graphql -X POST --input -", "Cannot classify"),
                                ("gh api graphql --input /dev/stdin <<'EOF'\n{}\nEOF", "Cannot classify"),
                                ("gh api graphql -F query=@/proc/self/fd/0", "Cannot classify"),
                                ("gh api graphql -F query=@/dev/fd/0", "Cannot classify"),
                                ("cd /tmp/elsewhere && gh api graphql -F query=@merge.graphql", "separate command"),
                                ("cd $DIR && gh api graphql -F query=@merge.graphql", "separate command"),
                                ("gh api graphql -F query=@/tmp/elsewhere/merge.graphql", "Cannot classify"),
                                ("cat <<EOF\n$(git push origin main)\nEOF", "main only"),
                                ("git -c core.hooksPath=/dev/null commit -m change", "hooks"),
                                ("git config core.hooksPath /dev/null", "hooks"),
                                ("git config set core.hooksPath /dev/null", "hooks"),
                                ("git config core.hooksPath get", "hooks"),
                                ("git config set core.hooksPath list", "hooks"),
                                ("git config -- core.hooksPath --get; git commit -m change", "hooks")):
            self.assertBlocked("Execute", {"command": command}, reason)
        self.assertAllowed("Execute", {"command": "git commit --no-edit --amend"})
        for command in ("cat <<'EOF'\n$(git push origin main)\nEOF",
                        "echo \"$(printf '%s' \"a\\\"b\")\"", "git config --get core.hooksPath",
                        "git config --get core.hooksPath '^/tmp'", "git config get core.hooksPath",
                        "git config --list"):
            self.assertAllowed("Execute", {"command": command})

    def test_graphql_query_files_are_read(self):
        with tempfile.TemporaryDirectory() as folder:
            Path(folder, "merge.graphql").write_text("mutation { mergePullRequest(input: {}) { clientMutationId } }")
            Path(folder, "reply.graphql").write_text("mutation { addPullRequestReviewThreadReply(input: {}) { comment { url } } }")
            payload = {"tool_name": "Execute", "cwd": folder,
                       "tool_input": {"command": "gh api graphql -F query=@merge.graphql"}}
            result = guard_call(payload)
            self.assertEqual(result.returncode, 2, result.stderr)
            payload["tool_input"]["command"] = "gh api graphql -F query=@reply.graphql"
            result = guard_call(payload)
            self.assertEqual(result.returncode, 0, result.stderr)
            payload["tool_input"]["command"] = ("printf '%s' 'mutation { mergePullRequest(input: {}) { clientMutationId } }'"
                                                " > reply.graphql; gh api graphql -F query=@reply.graphql")
            result = guard_call(payload)
            self.assertEqual(result.returncode, 2, result.stderr)
            self.assertIn("separate command", result.stderr)
            Path(folder, "sub").mkdir()
            Path(folder, "sub", "body.json").write_text(
                '{"query": "fragment R on MergePullRequestPayload { clientMutationId } '
                'mutation { mergePullRequest(input: {}) { ...R } }"}')
            # The shell reads the merge mutation from the outer directory; sub holds a harmless file.
            Path(folder, "sub", "merge.graphql").write_text("query { viewer { login } }")
            for command, reason in (("(cd sub); gh api graphql -F query=@merge.graphql", "separate command"),
                                    ("cd sub | cat; gh api graphql -F query=@merge.graphql", "separate command"),
                                    ("env -C sub gh api graphql -F query=@reply.graphql", "Cannot classify"),
                                    ("true || cd sub; gh api graphql -F query=@merge.graphql", "separate command"),
                                    ("{ if false; then cd sub; fi; }; gh api graphql -F query=@merge.graphql",
                                     "separate command"),
                                    ("git checkout other && gh api graphql -F query=@reply.graphql", "separate command"),
                                    ("cd -P sub && gh api graphql -F query=@merge.graphql", "separate command")):
                payload["tool_input"]["command"] = command
                result = guard_call(payload)
                self.assertEqual(result.returncode, 2, command)
                self.assertIn(reason, result.stderr)
            payload.update(cwd=str(Path(folder, "sub")))
            payload["tool_input"]["command"] = "gh api graphql --input body.json"
            result = guard_call(payload)
            self.assertEqual(result.returncode, 2, result.stderr)
            self.assertIn("maintainer", result.stderr)

    def test_gh_api_variants(self):
        with tempfile.TemporaryDirectory() as folder:
            body = json.dumps({"query": "mutation { mergePullRequest\n(input: {}) { clientMutationId } }"})
            Path(folder, "body.json").write_text(body)
            payload = {"tool_name": "Execute", "cwd": folder,
                       "tool_input": {"command": "gh api graphql --input body.json"}}
            result = guard_call(payload)
            self.assertEqual(result.returncode, 2, result.stderr)
        mutation = "query=mutation { mergePullRequest # note\n(input: {}) { clientMutationId } }"
        for endpoint in ("graphql", "/graphql", "https://api.github.com/graphql"):
            self.assertBlocked("Execute", {"command": f"gh api {endpoint} -f '{mutation}'"}, "maintainer")
        for command in ("gh pr -R o/r merge 12 --squash", "gh pr --repo=o/r merge 12",
                        "gh api repos/o/r/releases -f tag_name=v1.2.3",
                        "gh api repos/o/r/merges -f base=main -f head=topic",
                        "gh api -X PATCH repos/o/r/git/refs/heads/topic -F force=true -f sha=abc",
                        "gh api repos/o/r/pulls/12/reviews -f event=APPROVE",
                        "gh api repos/o/r/pulls/12/reviews/5/events -f event=REQUEST_CHANGES",
                        "gh api repos/o/r/pulls/12/reviews -f event=COMMENT -f event=APPROVE",
                        "gh api -X POST repos/o/r/git/refs --input ref.json",
                        "gh api -X DELETE repos/o/r/contents/a.md -f message=remove -f sha=abc",
                        "gh api -X PUT repos/o/r/contents/a.md -f branch=topic -f message=x -f content=eA==",
                        "gh api graphql -f 'query=mutation { mergePullRequest,(input: {}) { clientMutationId } }'",
                        "gh api graphql -f 'query=mutation { ...F } fragment F on Mutation { x }'",
                        "gh api graphql -f $'query=mutation { # note\\r mergePullRequest(input: {}) { clientMutationId } }'",
                        "gh api graphql -f 'query=mutation { addComment(input: {body: \"\"\"a \\\"\"\" }} \"\"\"}) "
                        "{ clientMutationId } mergePullRequest(input: {}) { clientMutationId } }'",
                        "gh api -iX PUT repos/o/r/pulls/12/merge", "gh api -iXPUT repos/o/r/pulls/12/merge",
                        "gh api graphql -f 'query=mutation($input: MergePullRequestInput! = {pullRequestId: \"P\"}) "
                        "{ mergePullRequest(input: $input) { clientMutationId } }'",
                        "gh api graphql -f 'query=query($mutation: ID) { viewer { login } } "
                        "mutation { mergePullRequest(input: {}) { clientMutationId } }'",
                        "gh api graphql -f 'query=mutation { a: addComment(input: {}) { clientMutationId } "
                        "b: mergePullRequest(input: {}) { clientMutationId } }'"):
            self.assertBlocked("Execute", {"command": command}, "maintainer")
        self.assertBlocked("Execute", {"command": "gh api -iz repos/o/r/pulls"}, "Cannot classify")
        for command in ("gh api repos/o/r/releases", "gh api -X GET repos/o/r/pulls -f state=open",
                        "gh api -i repos/o/r/pulls", "gh api -iXGET repos/o/r/pulls",
                        "gh api repos/o/r/pulls/12/comments/34/replies -f body=ok",
                        "gh api -X PATCH repos/o/r/issues/comments/34 -f body=edited",
                        "gh api repos/o/r/pulls/12/reviews -f event=COMMENT -f body=Review",
                        "gh api repos/o/r/pulls/12/reviews -f body=Draft -f 'comments[][path]=a.py'",
                        "gh api repos/o/r/pulls/12/reviews/5/events -f event=comment",
                        "gh api graphql -f 'query=query { viewer { login } } # mergePullRequest('",
                        "gh api graphql -f 'query=query mutation { viewer { login } }'",
                        "gh api graphql -f 'query=mutation($id: ID!) { r: resolveReviewThread(input: {threadId: $id}) "
                        "{ thread { isResolved } } }' -F id=X"):
            self.assertAllowed("Execute", {"command": command})
    def test_actions_variable_writes_are_blocked(self):
        name = "PP_REVIEW_FALLBACK_UNTIL"
        for command in (f"gh variable set {name} --body 2026-12-01T00:00:00Z",
                        f"gh variable set {name} < value.txt", f"gh variable delete {name}",
                        f"gh variable remove {name}", f"gh -R o/r variable set {name} -b x",
                        f"gh variable -R o/r set {name} -b x", f"gh variable set -R o/r {name} -b x",
                        f"gh --repo=o/r variable delete {name}",
                        f"'gh' \"variable\" 'set' {name} -b x", f"/usr/bin/gh variable set {name} -b x",
                        f"env GH_REPO=o/r gh variable set {name} -b x", f"echo $(gh variable delete {name})",
                        f"gh variable list && gh variable set {name} -b x", f"gh variable --help set {name}",
                        f"gh variable set --org o {name} -b x"):
            self.assertBlocked("Execute", {"command": command}, "owner-maintained")
        for command in (f"gh api -X PATCH repos/o/r/actions/variables/{name} -f value=x",
                        f"gh api repos/o/r/actions/variables -f name={name} -f value=x",
                        f"gh api --method DELETE repos/o/r/actions/variables/{name}",
                        f"gh api -XDELETE orgs/o/actions/variables/{name}",
                        f"gh api -X POST repos/o/r/environments/prod/variables -f name={name} -f value=x"):
            self.assertBlocked("Execute", {"command": command}, "need the maintainer")
        self.assertBlocked("Execute", {"command": f"gh variable $ACTION {name}"}, "Cannot classify")
        for command in ("gh variable list", f"gh variable get {name}", "gh variable", "gh variable --help",
                        "gh variable -h", "gh -R o/r variable list --json name,value",
                        f"gh api repos/o/r/actions/variables/{name}", "gh api repos/o/r/actions/variables",
                        f"gh api -X GET orgs/o/actions/variables/{name}"):
            self.assertAllowed("Execute", {"command": command})
        # gh help could run a config alias named help (GRE-1280); gh variable --help stays allowed above.
        self.assertBlocked("Execute", {"command": "gh help variable"}, "gh <command> --help")

    def test_gh_options_before_the_subcommand(self):
        name = "PP_REVIEW_FALLBACK_UNTIL"
        self.assertBlocked("Execute", {"command": f"gh --help=false variable set {name} -b 2026-12-01T00:00:00Z"},
                           "owner-maintained")
        for command, reason in ((f"gh -h=false variable delete {name}", "owner-maintained"),
                                (f"gh variable --help=false set {name} -b x", "owner-maintained"),
                                ("gh -R owner/repo pr merge 12", "maintainer"),
                                ("gh --help=false pr merge 12", "maintainer"),
                                ("gh pr --help=false merge 12", "maintainer"),
                                ("gh pr -R o/r --help=false merge 12", "maintainer"),
                                ("gh --version=false release create v1", "authorization"),
                                ("gh release --help=false create v1", "authorization"),
                                ("gh --repo o/r api -X PUT repos/o/r/pulls/12/merge", "maintainer"),
                                ("gh --help=false api -X PUT repos/o/r/pulls/12/merge", "maintainer"),
                                ("gh --repo=o/r api repos/o/r/actions/variables -f name=x -f value=y",
                                 "need the maintainer"),
                                (f"gh --foo variable set {name} -b x", "after the subcommand"),
                                (f"gh variable --env prod set {name} -b x", "after the subcommand"),
                                ("gh pr --foo merge 12", "after the subcommand"),
                                ("gh -- pr merge 12", "after the subcommand"),
                                ("gh --$flag pr merge 12", "shell expansion"),
                                ("gh --help=$opts", "shell expansion"),
                                ("gh -h=$opts", "shell expansion"),
                                ("gh --version=$(cat words)", "shell expansion"),
                                ("gh --repo=$repo", "shell expansion"),
                                ("gh -R$repo", "shell expansion"),
                                ("gh -R $repo", "shell expansion"),
                                ("gh --repo \"$repo\" pr view 12", "shell expansion"),
                                ("gh pr --help=$opts", "shell expansion"),
                                ("gh variable -R $repo list", "shell expansion"),
                                ("gh --help=false pr checkout 3 && git push origin HEAD", "destination branch")):
            with self.subTest(command=command):
                self.assertBlocked("Execute", {"command": command}, reason)
        for command in ("gh --version", "gh --help", "gh -h", "gh pr --help", "gh -R o/r pr view 12",
                        "gh --help=false pr view 12", "gh --repo o/r issue list", f"gh -R o/r variable --help=false list",
                        'gh pr view 12 -R "$repo"', 'gh variable list --repo="$repo"'):
            with self.subTest(command=command):
                self.assertAllowed("Execute", {"command": command})

    def test_generated_paths_are_normalized(self):
        for path in ("/repo/rust/./bundle-manifest.json", "/repo/rust//bundle-manifest.json"):
            self.assertBlocked("Edit", {"file_path": path}, "generated")
            self.assertBlocked("ApplyPatch", {"input": f"*** Update File: {path}\n"}, "generated")

    def test_protected_files_are_blocked_for_each_edit_tool(self):
        self.assertBlocked("Create", {"file_path": "/repo/.env"}, "environment files")
        self.assertBlocked("Edit", {"file_path": "/repo/web/.env.local"}, "environment files")
        self.assertBlocked("Edit", {"file_path": "/repo/rust/bundle-manifest.json"}, "generated")
        self.assertBlocked("Create", {"file_path": "/repo/.git/config"}, "Git metadata")
        patch = "*** Begin Patch\n*** Update File: /repo/.env.production\n*** End Patch\n"
        self.assertBlocked("ApplyPatch", {"input": patch}, "environment files")
        self.assertBlocked("ApplyPatch", patch, "environment files")

    def test_patch_move_destinations_are_checked(self):
        for destination, reason in ((".env", "environment files"),
                                    (".git/config", "Git metadata"),
                                    ("rust/bundle-manifest.json", "generated")):
            patch = f"*** Update File: /repo/.env.example\n*** Move to: /repo/{destination}\n"
            self.assertBlocked("ApplyPatch", {"input": patch}, reason)
        self.assertAllowed("ApplyPatch", {"input": "*** Update File: /repo/a.py\n*** Move to: /repo/b.py\n"})

    def test_templates_and_ordinary_files_pass(self):
        self.assertAllowed("Create", {"file_path": "/repo/.env.example"})
        self.assertAllowed("Edit", {"file_path": "/repo/web/.env.local.example"})
        self.assertAllowed("Edit", {"file_path": "/repo/docs/development/missions.md"})
        patch = "*** Begin Patch\n*** Add File: /repo/src/lib.rs\n*** End Patch\n"
        self.assertAllowed("ApplyPatch", {"input": patch})



def neutral(*args, stdin=""):
    return spawn([sys.executable, "-B", str(CLI), *args], input=stdin,
                          capture_output=True, text=True, check=False)


def request(body):
    return neutral("json", stdin=json.dumps(body))


class NeutralGuardCliTests(unittest.TestCase):
    def assertRefused(self, result, reason):
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn(reason, result.stderr)

    def assertPasses(self, result):
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr, "")

    def test_json_operations_allow_and_refuse(self):
        with tempfile.TemporaryDirectory() as folder:
            self.assertPasses(request({"operation": "command", "cwd": folder,
                                       "command": "git push -u origin HEAD:chore/topic"}))
            self.assertRefused(request({"operation": "command", "cwd": folder,
                                        "command": "git push origin HEAD:main"}), "main only")
            self.assertRefused(request({"operation": "command", "cwd": folder,
                                        "command": "gh pr merge 12 --squash"}), "maintainer")
        self.assertPasses(request({"operation": "file", "path": "/repo/docs/guide.md"}))
        self.assertRefused(request({"operation": "file", "path": "/repo/.env"}), "environment files")
        self.assertPasses(request({"operation": "patch", "patch": "*** Update File: /repo/a.py\n"}))
        self.assertRefused(request({"operation": "patch",
                                    "patch": "*** Update File: /repo/a.py\n*** Move to: /repo/.git/config\n"}),
                           "Git metadata")

    def test_argument_operations_allow_and_refuse(self):
        self.assertPasses(neutral("command", "--cwd", str(ROOT), "git log --oneline -1"))
        self.assertRefused(neutral("command", "--cwd", str(ROOT), "--", "git push --force origin topic"), "force-push")
        script = "python3 - <<'PY'\nprint('git push --force')\nPY\ngit push origin main\n"
        self.assertRefused(neutral("command", "-", stdin=script), "main only")
        self.assertPasses(neutral("command", "-", stdin=script.rsplit("git push", 1)[0]))
        self.assertPasses(neutral("file", "/repo/a.py", "/repo/.env.example"))
        self.assertRefused(neutral("file", "/repo/a.py", "/repo/rust/bundle-manifest.json"), "generated")
        self.assertRefused(neutral("patch", "-", stdin="*** Delete File: /repo/web/.env.local\n"), "environment files")
        with tempfile.TemporaryDirectory() as folder:
            Path(folder, "ok.patch").write_text("*** Add File: /repo/src/lib.rs\n")
            self.assertPasses(neutral("patch", str(Path(folder, "ok.patch"))))

    def test_cwd_with_spaces_resolves_request_files(self):
        with tempfile.TemporaryDirectory() as parent:
            folder = Path(parent, "repo with spaces")
            folder.mkdir()
            (folder / "merge.graphql").write_text("mutation { mergePullRequest(input: {}) { clientMutationId } }")
            (folder / "reply.graphql").write_text("mutation { addPullRequestReviewThreadReply(input: {}) { comment { url } } }")
            self.assertRefused(neutral("command", "--cwd", str(folder), "gh api graphql -F query=@merge.graphql"),
                               "maintainer")
            self.assertPasses(neutral("command", "--cwd", str(folder), "gh api graphql -F query=@reply.graphql"))
            self.assertRefused(request({"operation": "command", "cwd": str(folder),
                                        "command": "gh api graphql -F query=@merge.graphql"}), "maintainer")
            self.assertRefused(request({"operation": "command", "cwd": parent,
                                        "command": 'git -C "repo with spaces" push origin HEAD:main'}), "main only")

    def test_cwd_resolves_parent_after_symlink_like_the_kernel(self):
        with tempfile.TemporaryDirectory() as tmp:
            folder = Path(tmp).resolve()
            Path(folder, "a/b").mkdir(parents=True)
            Path(folder, "link").symlink_to("a/b")
            # link/../repo is a/repo on disk; a lexical collapse would pick the repo on main instead.
            subprocess.run(["git", "init", "-q", "-b", "topic", str(folder / "a/repo")], check=True)
            subprocess.run(["git", "init", "-q", "-b", "main", str(folder / "repo")], check=True)
            cwd, push = str(folder / "link/../repo"), "git push origin HEAD"
            self.assertPasses(neutral("command", "--cwd", cwd, push))
            self.assertPasses(spawn([sys.executable, "-B", str(CLI), "command", "--cwd", "link/../repo", push],
                                             cwd=folder, capture_output=True, text=True, check=False))
            self.assertPasses(request({"operation": "command", "cwd": cwd, "command": push}))
            self.assertPasses(hook(CLAUDE, claude_call("Bash", cwd, command=push)))
            self.assertRefused(neutral("command", "--cwd", cwd, "cd ../../repo && " + push), "main only")
            # Without "..", a bare cd stays lexical, as the shell's logical cd does.
            self.assertRefused(neutral("command", "--cwd", str(folder / "link"), "cd ../repo && " + push), "main only")

    def test_malformed_and_unsupported_input_is_refused(self):
        with tempfile.TemporaryDirectory() as folder:
            Path(folder, "file").write_text("")
            for stdin in ("not json", "[]", "null", '"git status"',
                          json.dumps({"tool_name": "Execute", "tool_input": {"command": "git status"}}),
                          json.dumps({"operation": "Bash", "command": "git push origin main", "cwd": folder}),
                          json.dumps({"operation": "execute", "command": "git status", "cwd": folder}),
                          json.dumps({"operation": "command", "command": "git status"}),
                          json.dumps({"operation": "command", "command": "git status", "cwd": folder, "shell": "zsh"}),
                          json.dumps({"operation": "command", "command": ["git", "status"], "cwd": folder}),
                          json.dumps({"operation": "command", "command": "git status", "cwd": "relative/dir"}),
                          json.dumps({"operation": "command", "command": "git status", "cwd": folder + "/missing"}),
                          json.dumps({"operation": "command", "command": "git status", "cwd": folder + "/file"}),
                          json.dumps({"operation": "file", "path": " "}),
                          json.dumps({"operation": "file", "paths": ["/repo/a.py"]}),
                          json.dumps({"operation": "patch", "patch": "--- a/.env\n+++ b/.env\n@@ -1 +1 @@\n-A=1\n+A=2\n"})):
                with self.subTest(stdin=stdin):
                    self.assertRefused(neutral("json", stdin=stdin), "pp guard: invalid input")
            for args in ((), ("bash", "git status"), ("command",), ("command", "git", "status"),
                         ("command", "--cwd", folder + "/missing", "git status"), ("file",), ("file", ""),
                         ("patch", folder + "/missing.patch")):
                with self.subTest(args=args):
                    result = neutral(*args)
                    self.assertEqual(result.returncode, 2, result.stderr)

    def test_unclassifiable_commands_are_refused(self):
        for command, reason in (("git push origin 'main", "Cannot parse"),
                                ("env -S 'git push origin main'", "Cannot classify"),
                                ("git push origin HEAD:$BRANCH", "destination branch")):
            self.assertRefused(neutral("command", command), reason)


COMMANDS = ("git push -u origin HEAD:chore/topic", "git push origin HEAD:main", "git push --force origin topic",
            "git push --tags", "git commit --no-verify -m x", "gh pr merge 12", "gh release create v1",
            "gh api repos/o/r/pulls/12/reviews -f event=APPROVE", "gh api repos/o/r/pulls/12/comments/34/replies -f body=ok",
            "git config core.hooksPath /tmp/x", "GIT_CONFIG_COUNT=1 git commit -m x", "git push origin topic",
            "cd sub && gh api graphql -F query=@merge.graphql", "printf '%s' 'git push --force'",
            "python3 - <<'PY'\nprint(1)\nPY\ngit push origin main", "git push origin 'main", "env -S 'git push'",
            "npm --prefix web ci", "gh variable set PP_REVIEW_FALLBACK_UNTIL -b x",
            "gh variable get PP_REVIEW_FALLBACK_UNTIL", "")


CLAUDE = ROOT / ".claude/hooks/guard.py"
SHA = "0123456789abcdef0123456789abcdef01234567"
MERGE_FORM = f"gh pr merge 12 --squash --match-head-commit {SHA}"
# The command the retired lane-rules.yaml merge_gate told lanes to run.
OLD_MERGE_GATE = f"gh pr merge 12 -R Print-Partner/PrintPartner --squash --match-head-commit {SHA}"
CLAUDE_TOOLS = {"Bash", "Write", "Edit", "MultiEdit", "NotebookEdit"}


def hook(adapter, payload, coordinator=None, extra=None, cwd=None, *args):
    env = {key: value for key, value in os.environ.items() if key != "PP_COORDINATOR_WORKTREE"}
    env.update(extra or {})
    if coordinator is not None:
        env["PP_COORDINATOR_WORKTREE"] = coordinator
    text = payload if isinstance(payload, str) else json.dumps(payload)
    return spawn([sys.executable, "-B", str(adapter), *args], input=text, env=env, cwd=cwd,
                          capture_output=True, text=True, check=False)


def execute_call(command, cwd):
    return {"hook_event_name": "PreToolUse", "tool_name": "Execute", "cwd": cwd, "tool_input": {"command": command}}


def claude_call(tool, cwd="/", **data):
    return {"session_id": "s", "hook_event_name": "PreToolUse", "tool_name": tool, "cwd": cwd, "tool_input": data}


def load(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def git_repo(path):
    subprocess.run(["git", "init", "-q", str(path)], check=True)
    return str(Path(path).resolve())


class ClaudeAdapterTests(unittest.TestCase):
    def assertRefused(self, result, reason="pp guard"):
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn(reason, result.stderr)

    def assertAllowed(self, result):
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_documented_hook_config_covers_exactly_the_adapter_tools(self):
        guide = (ROOT / "docs/development/agent-workflow.md").read_text(encoding="utf-8")
        blocks = [json.loads(block) for block in re.findall(r"```json\n(.*?)```", guide, re.S)
                  if ".claude/hooks/guard.py" in block]
        self.assertEqual(len(blocks), 2)
        adapter = load(CLAUDE, "claude_guard")
        self.assertEqual(set(adapter.PATH_FIELDS) | {"Bash"}, CLAUDE_TOOLS)
        for config in blocks:
            (group,) = config["hooks"]["PreToolUse"]
            self.assertEqual(set(group["matcher"].split("|")), CLAUDE_TOOLS)
            self.assertIn('"$CLAUDE_PROJECT_DIR"/.claude/hooks/guard.py', group["hooks"][0]["command"])
        self.assertIn(".claude/settings.local.json", (ROOT / ".gitignore").read_text(encoding="utf-8").splitlines())

    def test_bash_maps_to_the_command_policy(self):
        with tempfile.TemporaryDirectory() as folder:
            Path(folder, "merge.graphql").write_text("mutation { mergePullRequest(input: {}) { clientMutationId } }")
            for command in filter(None, COMMANDS):
                with self.subTest(command=command):
                    result = hook(CLAUDE, claude_call("Bash", folder, command=command, description="d"))
                    cli = request({"operation": "command", "cwd": folder, "command": command})
                    self.assertEqual((result.returncode, result.stderr), (cli.returncode, cli.stderr))

    def test_file_tools_refuse_protected_paths(self):
        protected = {"/repo/.env": "environment files", "/repo/.git/config": "Git metadata",
                     "/repo/rust/bundle-manifest.json": "generated",
                     "/repo/.claude/settings.local.json": "Claude Code settings",
                     "/repo/.claude/settings.json": "Claude Code settings",
                     "/repo/.claude/hooks/guard.py": "Claude Code settings",
                     "/repo/.claude/agents/../settings.local.json": "Claude Code settings",
                     "/repo/.claude/agents/../hooks/guard.py": "Claude Code settings",
                     "/repo/docs/../.git/config": "Git metadata"}
        for path, reason in protected.items():
            for tool in ("Write", "Edit", "MultiEdit"):
                with self.subTest(tool=tool, path=path):
                    self.assertRefused(hook(CLAUDE, claude_call(tool, file_path=path, content="x")), reason)
            with self.subTest(tool="NotebookEdit", path=path):
                self.assertRefused(hook(CLAUDE, claude_call("NotebookEdit", notebook_path=path, new_source="x")), reason)
            with self.subTest(tool="neutral file", path=path):
                self.assertRefused(request({"operation": "file", "path": path}), reason)
        for path in ("/repo/docs/guide.md", "/repo/.env.example", "/repo/.claude/agents/review.md",
                     "/repo/.claude/worktrees/lane/docs/guide.md"):
            with self.subTest(path=path):
                self.assertAllowed(hook(CLAUDE, claude_call("Write", file_path=path, content="x")))
        self.assertAllowed(hook(CLAUDE, claude_call("NotebookEdit", notebook_path="/repo/a.ipynb", new_source="x")))

    def test_malformed_payloads_are_refused(self):
        with tempfile.TemporaryDirectory() as folder:
            bash = claude_call("Bash", folder, command="git status")
            without = lambda key: json.dumps({name: value for name, value in bash.items() if name != key})
            payloads = ["not json", "[]", "null", '"git status"', "{}", json.dumps({**bash, "tool_name": 7}),
                        without("tool_name"), without("tool_input"), without("cwd"),
                        json.dumps({**bash, "tool_input": ["git status"]}), json.dumps({**bash, "tool_input": {}}),
                        json.dumps({**bash, "tool_input": {"command": ["git"]}}),
                        json.dumps({**bash, "cwd": "relative"}), json.dumps({**bash, "cwd": folder + "/missing"}),
                        json.dumps(claude_call("Write", content="x")), json.dumps(claude_call("Edit", file_path=" ")),
                        json.dumps(claude_call("MultiEdit", file_path=["/repo/a.py"])),
                        json.dumps(claude_call("NotebookEdit", file_path="/repo/a.ipynb")),
                        json.dumps(claude_call("Read", file_path="/repo/a.py")),
                        json.dumps(claude_call("Execute", folder, command="git status")),
                        json.dumps(claude_call("Task", prompt="merge it"))]
            for payload in payloads:
                with self.subTest(payload=payload):
                    self.assertRefused(hook(CLAUDE, payload), "refused unreadable or unsupported")

    def test_adapter_refuses_when_policy_is_missing(self):
        with tempfile.TemporaryDirectory() as folder:
            copy = Path(folder, ".claude/hooks/guard.py")
            copy.parent.mkdir(parents=True)
            copy.write_text(CLAUDE.read_text())
            self.assertRefused(hook(copy, claude_call("Write", file_path="/repo/a.py", content="x")), "FileNotFoundError")


class CoordinatorMergeTests(unittest.TestCase):
    def setUp(self):
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        self.coordinator = git_repo(Path(folder.name, "coordinator"))
        self.lane = git_repo(Path(folder.name, "lane"))
        Path(self.coordinator, "sub").mkdir()

    def results(self, command, cwd, coordinator, extra=None):
        return (guard_call(execute_call(command, cwd), coordinator, extra),
                hook(CLAUDE, claude_call("Bash", cwd, command=command), coordinator, extra))

    def test_lane_worktree_merges_are_refused(self):
        # OLD_MERGE_GATE is exactly what a lane following the retired merge_gate would run.
        for command in (MERGE_FORM, OLD_MERGE_GATE, "gh pr merge 12 --squash"):
            for coordinator in (None, self.coordinator, "relative/path"):
                for result in self.results(command, self.lane, coordinator):
                    with self.subTest(command=command, coordinator=coordinator):
                        self.assertEqual(result.returncode, 2, result.stderr)
                        self.assertIn("Only the coordinator worktree merges", result.stderr)

    def test_coordinator_merge_is_allowed_only_in_its_worktree(self):
        for command in (MERGE_FORM, f"gh pr merge --squash --match-head-commit={SHA} 12",
                        f"gh pr merge --match-head-commit {SHA} --squash"):
            for cwd in (self.coordinator, str(Path(self.coordinator, "sub"))):
                for result in self.results(command, cwd, self.coordinator):
                    with self.subTest(command=command, cwd=cwd):
                        self.assertEqual(result.returncode, 0, result.stderr)
        for result in self.results(MERGE_FORM, self.coordinator, None):
            self.assertEqual(result.returncode, 2, result.stderr)
        for command in (OLD_MERGE_GATE, f"{MERGE_FORM} --admin", f"{MERGE_FORM} --auto", f"{MERGE_FORM} --delete-branch",
                        f"gh pr merge 12 --merge --match-head-commit {SHA}", "gh pr merge 12 --squash",
                        f"gh pr merge 12 --squash --match-head-commit {SHA[:12]}",
                        f"gh pr merge 12 --squash --match-head-commit {SHA.upper()}",
                        f"gh pr merge 12 13 --squash --match-head-commit {SHA}",
                        "gh pr merge 12 --squash --match-head-commit $SHA", f"GH_REPO=o/r {MERGE_FORM}",
                        f"env {MERGE_FORM}", f"cd {self.lane} && {MERGE_FORM}", f"{MERGE_FORM}; echo done",
                        f"({MERGE_FORM})", f"echo $({MERGE_FORM})", f"{MERGE_FORM} &",
                        f"gh -R o/r pr merge 12 --squash --match-head-commit {SHA}",
                        "gh api -X PUT repos/o/r/pulls/12/merge"):
            for result in self.results(command, self.coordinator, self.coordinator):
                with self.subTest(command=command):
                    self.assertEqual(result.returncode, 2, result.stderr)

    def test_coordinator_root_inside_lane_worktrees_is_refused(self):
        guard = load(CLI, "agent_guard_lanes")
        args = MERGE_FORM.split()
        with patch.dict(os.environ, {"PP_COORDINATOR_WORKTREE": self.coordinator}):
            self.assertTrue(guard.coordinator_merge(args, self.coordinator))
            with patch.object(guard, "LANE_WORKTREES", str(Path(self.coordinator).parent)):
                self.assertFalse(guard.coordinator_merge(args, self.coordinator))
                self.assertIn("Only the coordinator", guard.check_command(MERGE_FORM, self.coordinator))
            with patch.object(guard, "LANE_WORKTREES", self.coordinator):
                self.assertFalse(guard.coordinator_merge(args, self.coordinator))


MARKER = f"<!-- pp-review-gate head={SHA} reviewer=gpt-5.6-sol findings=0 -->"


class ReviewMarkerTests(unittest.TestCase):
    make_worktrees, results = CoordinatorMergeTests.setUp, CoordinatorMergeTests.results

    def setUp(self):
        self.make_worktrees()
        for folder in (self.coordinator, self.lane):
            Path(folder, "marker.md").write_text(f"Reviewed.\n{MARKER}\n")
            Path(folder, "marker.json").write_text(json.dumps({"body": MARKER}))
            Path(folder, "plain.md").write_text("Fixed in the next commit.\n")
            Path(folder, "plain.json").write_text(json.dumps({"body": "Fixed in the next commit."}))

    POSTS = (f"gh pr comment 12 --body '{MARKER}'", f"gh pr comment 12 -b '{MARKER}'",
             f"gh pr comment --body='{MARKER}' 12", f"gh -R o/r pr comment 12 -b '{MARKER}'",
             f"gh pr comment 12 --edit-last -b '{MARKER}'", "gh pr comment 12 --body-file marker.md",
             "gh pr comment 12 -F marker.md", "gh pr comment 12 --body-file=marker.md",
             f"gh pr comment 12 --body-file - <<'EOF'\n{MARKER}\nEOF",
             f"gh pr comment 12 -F - <<< '{MARKER}'",
             f"gh issue comment 12 -b '{MARKER}'", f"gh issue -R o/r comment 12 --body-file marker.md",
             f"gh api repos/o/r/issues/12/comments -f body='{MARKER}'",
             f"gh api -X PATCH repos/o/r/issues/comments/34 -f body='{MARKER}'",
             "gh api repos/o/r/issues/12/comments -F body=@marker.md",
             "gh api repos/o/r/issues/12/comments --input marker.json",
             f"gh api graphql -f query='mutation {{ addComment(input: {{subjectId: \"X\", body: \"{MARKER}\"}}) "
             "{ clientMutationId } }'",
             f"echo ok && gh pr comment 12 -b '{MARKER.upper()}'",
             f"gh pr comment 12 -b '<!--pp-review-gate head={SHA}-->'")

    def test_lane_worktrees_cannot_post_markers(self):
        for command in self.POSTS:
            for coordinator in (None, self.coordinator):
                for result in self.results(command, self.lane, coordinator):
                    with self.subTest(command=command, coordinator=coordinator):
                        self.assertEqual(result.returncode, 2, result.stderr)
                        self.assertIn("Only the coordinator worktree posts pp-review-gate", result.stderr)

    def test_coordinator_worktree_posts_markers(self):
        for command in self.POSTS:
            for result in self.results(command, self.coordinator, self.coordinator):
                with self.subTest(command=command):
                    self.assertEqual(result.returncode, 0, result.stderr)
        # Without the variable, or after leaving the worktree, the coordinator is a lane.
        for command, coordinator in ((self.POSTS[0], None), (f"cd {self.lane} && {self.POSTS[0]}", self.coordinator)):
            for result in self.results(command, self.coordinator, coordinator):
                with self.subTest(command=command):
                    self.assertIn("Only the coordinator worktree posts", result.stderr)

    def test_other_comments_stay_allowed(self):
        for command in ("gh pr comment 12 -b 'Fixed in abc123.'", "gh pr comment 12 --body-file plain.md",
                        "gh issue comment 12 -F plain.md", "gh pr comment 12 -b 'The review gate check failed.'",
                        "gh api repos/o/r/issues/12/comments -f body='The review-gate check is red.'",
                        "gh api repos/o/r/issues/12/comments --input plain.json",
                        "gh pr view 12 --comments", "gh pr create --body-file marker.md",
                        "gh api graphql -f query='mutation($id: ID!) { resolveReviewThread(input: {threadId: $id}) "
                        "{ thread { isResolved } } }' -f id=\"$THREAD\""):
            for result in self.results(command, self.lane, None):
                with self.subTest(command=command):
                    self.assertEqual(result.returncode, 0, result.stderr)


class EncodedMarkerTests(unittest.TestCase):
    make_worktrees, results = CoordinatorMergeTests.setUp, CoordinatorMergeTests.results
    FILES = {"json-escape.json": '{"body": "\\u003c!-- pp-review-gate head=x reviewer=y findings=0 --\\u003e"}',
             "letter-escape.json": '{"body": "<!-- \\u0070p-review-gate head=x -->"}',
             "graphql.json": json.dumps({"query": "mutation($b: String!) { addComment(input: {subjectId: \"X\", "
                                                  "body: $b}) { clientMutationId } }",
                                         "variables": {"b": "<!-- pp-review-gate head=x -->"}}).replace("pp-", "\\u0070p-"),
             "body.md": "\\u003c!-- \\u0070p-review-gate head=x --\\u003e\n",
             "html.md": "&lt;!-- pp&#45;review&#45;gate head=x --&gt;\n",
             "broken.json": '{"body": "unterminated'}
    ESCAPED = ("gh api repos/o/r/issues/12/comments --input json-escape.json",
               "gh api repos/o/r/issues/12/comments --input letter-escape.json",
               "gh api graphql --input graphql.json",
               "gh api graphql -f query='mutation { addComment(input: {subjectId: \"X\", "
               "body: \"\\u003c!-- \\u0070p-review-gate head=x --\\u003e\"}) { clientMutationId } }'",
               "gh api repos/o/r/issues/12/comments -f body='&lt;!-- pp&#45;review&#45;gate head=x --&gt;'",
               "gh api repos/o/r/issues/12/comments -f body='%3C!--%20pp%2Dreview%2Dgate%20head=x%20--%3E'",
               "gh api repos/o/r/issues/12/comments -F body=@body.md",
               "gh pr comment 12 --body-file body.md", "gh pr comment 12 -F html.md",
               "gh pr comment 12 -b 'Recorded: PP-REVIEW-GATE head=x'",
               "gh issue comment 12 --body '%26lt%3B!-- pp-review%2Dgate'")

    def setUp(self):
        self.make_worktrees()
        for folder in (self.coordinator, self.lane):
            for name, text in self.FILES.items():
                Path(folder, name).write_text(text)

    def test_lane_worktrees_refuse_every_encoding_of_the_marker(self):
        for command in self.ESCAPED:
            for result in self.results(command, self.lane, self.coordinator):
                with self.subTest(command=command):
                    self.assertEqual(result.returncode, 2, result.stderr)
                    self.assertIn("Only the coordinator worktree posts pp-review-gate", result.stderr)

    def test_lane_worktrees_refuse_a_rest_body_that_is_not_json(self):
        for result in self.results("gh api repos/o/r/issues/12/comments --input broken.json", self.lane, None):
            self.assertEqual(result.returncode, 2, result.stderr)
            self.assertIn("read a comment body literally", result.stderr)

    def test_coordinator_worktree_sends_encoded_markers(self):
        for command in self.ESCAPED:
            for result in self.results(command, self.coordinator, self.coordinator):
                with self.subTest(command=command):
                    self.assertEqual(result.returncode, 0, result.stderr)


class MarkerAnywhereTests(unittest.TestCase):
    make_worktrees, results = CoordinatorMergeTests.setUp, CoordinatorMergeTests.results
    ANYWHERE = (f"bash -c 'gh pr comment 12 -b \"{MARKER}\"'", f"sh -c 'gh pr comment 12 -b \"{MARKER}\"'",
                f"zsh -c 'gh pr comment 12 -b \"{MARKER}\"'", f"env bash -lc 'gh pr comment 12 -b \"{MARKER}\"'",
                f"python3 -c 'import subprocess; subprocess.run([\"gh\", \"pr\", \"comment\", \"12\", \"-b\", "
                f"\"{MARKER}\"])'",
                f"echo '{MARKER}' > m.md", f"cat > m.md <<'EOF'\n{MARKER}\nEOF",
                "printf '%s' '\\u003c!-- \\u0070p-review-gate head=x -->' > m.md",
                "printf '%s' '&lt;!-- pp&#45;review&#45;gate head=x --&gt;' > m.md",
                "grep -rn PP-Review-Gate docs", "git commit -m 'note pp%2Dreview%2Dgate'")

    def setUp(self):
        self.make_worktrees()

    def test_lane_worktrees_refuse_any_command_naming_the_marker(self):
        for command in self.ANYWHERE:
            for result in self.results(command, self.lane, self.coordinator):
                with self.subTest(command=command):
                    self.assertEqual(result.returncode, 2, result.stderr)
                    self.assertIn("runs any command that names pp-review-gate", result.stderr)

    def test_coordinator_worktree_may_name_the_marker(self):
        for command in self.ANYWHERE:
            for result in self.results(command, self.coordinator, self.coordinator):
                with self.subTest(command=command):
                    self.assertEqual(result.returncode, 0, result.stderr)


class ShellWrapperTests(unittest.TestCase):
    def test_shell_c_scripts_are_classified_like_top_level_commands(self):
        for shell in ("bash", "sh", "zsh", "dash", "/bin/bash", "env bash", "nohup sh"):
            for command, reason in ((f"{shell} -c 'gh pr merge 12 --squash'", "Only the coordinator worktree merges"),
                                    (f"{shell} -ec 'git push --force origin HEAD:topic'", "Never force-push"),
                                    (f"{shell} -lc \"gh variable set X -b y\"", "owner-maintained"),
                                    (f"{shell} -c 'bash -c \"gh pr merge 12\"'", "Only the coordinator worktree merges")):
                with self.subTest(command=command):
                    result = hook(CLAUDE, claude_call("Bash", "/", command=command))
                    self.assertEqual(result.returncode, 2, result.stderr)
                    self.assertIn(reason, result.stderr)
        for command in ("bash -c 'echo hi'", "sh -c 'gh pr view 12'", "bash script.sh", "bash -n script.sh"):
            with self.subTest(command=command):
                self.assertEqual(hook(CLAUDE, claude_call("Bash", "/", command=command)).returncode, 0)

    def test_shell_options_before_c_are_parsed_or_refused(self):
        merge = "Only the coordinator worktree merges"
        for command, reason in (("bash --rcfile x -c 'gh pr merge 12'", merge),
                                ("bash --init-file x -c 'gh pr merge 12'", merge),
                                ("bash -o posix -c 'gh pr merge 12'", merge), ("bash +o posix -c 'gh pr merge 12'", merge),
                                ("bash -eo pipefail -c 'gh pr merge 12'", merge),
                                ("bash -O extglob -c 'gh pr merge 12'", merge), ("sh -e -c 'gh pr merge 12'", merge),
                                ("bash --login --norc -c 'gh pr merge 12'", merge),
                                ("zsh -o shwordsplit -c 'git push --force origin HEAD:topic'", "Never force-push"),
                                ("bash --unknown-option x -c 'gh pr merge 12'", "Cannot tell which argument"),
                                ("bash --rcfile=x -c 'gh pr merge 12'", "Cannot tell which argument"),
                                ("dash -oe x -c 'gh pr merge 12'", "Cannot tell which argument"),
                                ("bash -%x -c 'git push origin HEAD:main'", "Cannot tell which argument")):
            with self.subTest(command=command):
                result = hook(CLAUDE, claude_call("Bash", "/", command=command))
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertIn(reason, result.stderr)
        for command in ("bash --rcfile x -c 'echo hi'", "bash --unknown-option x -c 'echo hi'",
                        "bash -o posix -c 'gh pr view 12'"):
            with self.subTest(command=command):
                self.assertEqual(hook(CLAUDE, claude_call("Bash", "/", command=command)).returncode, 0)

    def test_c_scripts_inherit_the_outer_lines_git_context(self):
        pairs = (("GIT_CONFIG_GLOBAL=x git push origin HEAD:topic",
                  "GIT_CONFIG_GLOBAL=x bash -c 'git push origin HEAD:topic'"),
                 ("env GIT_DIR=/tmp/other git push origin HEAD:topic",
                  "env GIT_DIR=/tmp/other bash -c 'git push origin HEAD:topic'"),
                 ("git checkout other && git push origin HEAD:topic",
                  "git checkout other && bash -c 'git push origin HEAD:topic'"),
                 ("git config push.followTags true && git push origin HEAD:topic",
                  "git config push.followTags true && sh -c 'git push origin HEAD:topic'"))
        with tempfile.TemporaryDirectory() as folder:
            repo = git_repo(Path(folder, "repo"))
            for flat, wrapped in pairs:
                with self.subTest(wrapped=wrapped):
                    expected = hook(CLAUDE, claude_call("Bash", repo, command=flat))
                    result = hook(CLAUDE, claude_call("Bash", repo, command=wrapped))
                    self.assertEqual(expected.returncode, 2, expected.stderr)
                    self.assertEqual((result.returncode, result.stderr), (2, expected.stderr))
            self.assertEqual(hook(CLAUDE, claude_call("Bash", repo, command="bash -c 'git push origin HEAD:topic'"))
                             .returncode, 0)

    def test_wrapped_coordinator_merge_is_refused(self):
        with tempfile.TemporaryDirectory() as folder:
            coordinator = git_repo(Path(folder, "coordinator"))
            result = hook(CLAUDE, claude_call("Bash", coordinator, command=f"bash -c '{MERGE_FORM}'"), coordinator)
            self.assertEqual(result.returncode, 2, result.stderr)
            self.assertIn("Only the coordinator worktree merges", result.stderr)

    def test_coordinator_may_use_unknown_shell_options(self):
        with tempfile.TemporaryDirectory() as folder:
            coordinator = git_repo(Path(folder, "coordinator"))
            command = "bash --unknown-option x -c 'gh pr view 12'"
            self.assertEqual(hook(CLAUDE, claude_call("Bash", coordinator, command=command), coordinator).returncode, 0)
            self.assertEqual(hook(CLAUDE, claude_call("Bash", coordinator, command=command)).returncode, 2)

    def test_eval_and_source_of_git_or_gh_are_refused(self):
        with tempfile.TemporaryDirectory() as folder:
            Path(folder, "post.sh").write_text("gh pr comment 12 -b hi\n")
            Path(folder, "env.sh").write_text("export A=1\n")
            for command in ("eval gh pr view 12", "eval 'git push origin HEAD:topic'", "eval \"$CMD\"",
                            "source post.sh", ". post.sh", "source missing.sh"):
                with self.subTest(command=command):
                    result = hook(CLAUDE, claude_call("Bash", folder, command=command))
                    self.assertEqual(result.returncode, 2, result.stderr)
                    self.assertIn("not through eval or source", result.stderr)
            for command in ("eval echo hi", "source env.sh", ". env.sh"):
                with self.subTest(command=command):
                    self.assertEqual(hook(CLAUDE, claude_call("Bash", folder, command=command)).returncode, 0)


class UnreadableCommentBodyTests(unittest.TestCase):
    make_worktrees, results = CoordinatorMergeTests.setUp, CoordinatorMergeTests.results
    UNREAD = ("gh pr comment 12 -b \"$BODY\"", "gh pr comment 12 --body=\"$(cat marker.md)\"",
              "p='<!--'; gh pr comment 12 -b \"$p pp-review-gate head=x reviewer=y findings=0 -->\"",
              "printf '\\x3c!-- pp-review-gate' | gh pr comment 12 -F -", "gh pr comment 12 -F - < plain.md",
              "gh pr comment 12 --body-file - < marker.md", "gh issue comment 12 -F - < marker.md",
              "cat plain.md | gh pr comment 12 --body-file -", "gh pr comment 12 --body-file=-",
              "cp marker.md b.md && gh pr comment 12 -F b.md", "gh pr comment 12 -F missing.md",
              "echo hi && gh pr comment 12 -F plain.md", "cd sub && gh pr comment 12 -F ../plain.md",
              "gh pr comment $PR -b hi", "gh issue comment 12 -b \"$X\"",
              "A=pp-review; B=gate; gh pr comment 12 --body \"<!-- $A-$B head=0123456789abcdef0123456789abcdef01234567 "
              "reviewer=m findings=0 -->\"",
              "gh pr comment 12 -b \"${X}\"", "gh pr comment 12 -b \"`cat plain.md`\"",
              "gh pr comment 12 -b $'Fixed\\tthanks'", "gh issue comment 12 --body $'ok'",
              "gh api repos/o/r/issues/12/comments -f body=$'ok'",
              "gh api repos/o/r/pulls/12/comments/34/replies -f body=\"$X\"",
              "gh api repos/o/r/issues/12/comments -f body=\"$X\"",
              "gh api -X PATCH repos/o/r/issues/comments/34 -f body=\"$(cat marker.md)\"",
              "gh api repos/o/r/issues/12/comments -F body=@-", "gh api repos/o/r/issues/12/comments --input -",
              "gh api repos/o/r/pulls/12/comments/34/replies -F body=@missing.md",
              "gh api graphql -f query='mutation($b: String!) { addComment(input: {subjectId: \"X\", body: $b}) "
              "{ clientMutationId } }' -f b=\"$X\"")

    def setUp(self):
        self.make_worktrees()
        for folder in (self.coordinator, self.lane):
            Path(folder, "marker.md").write_text(f"{MARKER}\n")
            Path(folder, "plain.md").write_text("Fixed.\n")
        Path(self.lane, "sub").mkdir()

    def test_lane_worktrees_refuse_bodies_the_guard_cannot_read(self):
        for command in self.UNREAD:
            for result in self.results(command, self.lane, self.coordinator):
                with self.subTest(command=command):
                    self.assertEqual(result.returncode, 2, result.stderr)
                    # A command that also names the marker word is refused for that first.
                    self.assertRegex(result.stderr, "read a comment body literally|Only the coordinator worktree posts")

    def test_coordinator_worktree_may_send_any_body(self):
        for command in self.UNREAD:
            for result in self.results(command, self.coordinator, self.coordinator):
                with self.subTest(command=command):
                    self.assertEqual(result.returncode, 0, result.stderr)


def nested_shells(command, levels):
    """Wrap command in sh -c that many times, quoting each level for the shell."""
    for _ in range(levels):
        command = "sh -c " + shlex.quote(command)
    return command


class HiddenMergeTests(unittest.TestCase):
    """Merges spelled so the shell, not the guard, assembles them. Lanes never merge (GRE-1280)."""

    make_worktrees, results = CoordinatorMergeTests.setUp, CoordinatorMergeTests.results
    SHA_ARG = f"--squash --match-head-commit {SHA}"
    # Each was a probe that reached gh pr merge, or a gh api merge, without the guard refusing it.
    REFUSED = (f"printf '%s\\n' --admin | xargs gh pr merge 12 {SHA_ARG}",
               f"find . -maxdepth 0 -exec gh pr merge 12 {SHA_ARG} --admin \\;",
               f"alias m='gh pr merge 12 {SHA_ARG} --admin'", f"alias m='gh pr merge 12 {SHA_ARG} --admin'; m",
               f"C='gh pr merge 12 {SHA_ARG} --admin'; $C", f"git -c alias.m='!gh pr merge 12 {SHA_ARG} --admin' m",
               f"git config alias.m '!gh pr merge 12 {SHA_ARG}'", f"setsid gh pr merge 12 {SHA_ARG}",
               f"flock /tmp/x gh pr merge 12 {SHA_ARG}", f"watch -n 1 gh pr merge 12 {SHA_ARG}",
               f"script -c 'gh pr merge 12 {SHA_ARG}' /dev/null", f"timeout 5 gh pr merge 12 {SHA_ARG}",
               f"python3 -c 'import os; os.system(\"gh pr merge 12 {SHA_ARG} --admin\")'",
               "gh pr  'merge' 12", "gh -R o/r pr merge 12", "bash -c 'gh pr merge 12'",
               "find . -exec sh -c 'gh pr merge 12' \\;", "ssh host 'gh pr merge 12'",
               f"gh alias set m 'pr merge 12 {SHA_ARG}'",
               # Quoting and escapes the shell removes after the guard reads the line.
               "printf '%s\\n' --admin | xargs gh pr me''rge 12", "find . -maxdepth 0 -exec gh pr mer''ge 12 --admin \\;",
               "xargs gh pr m\\erge 12 --admin", 'xargs gh pr "mer"ge 12 --admin', "xargs g''h pr merge 12",
               "xargs gh pr $'m\\x65rge' 12 --admin", "script -c \"gh pr mer''ge 12 --admin\" /dev/null",
               "setsid gh pr 'mer'ge 12 --admin", "flock /tmp/x gh pr me\\rge 12 --admin",
               "watch -n 1 gh pr me''rge 12 --admin", "parallel gh pr me''rge ::: 12", "tmux new-session gh pr me''rge 12",
               "xargs gh api -X PUT repos/o/r/pulls/12/merge", "find . -exec gh api -X PUT repos/o/r/pulls/12/mer''ge \\;",
               "xargs gh api graphql -f query='mutation { mergePullRequest(input: {}) { clientMutationId } }'",
               "xargs gh $(printf pr) view 12", "setsid gh pr `echo x` 12",
               # Split variables: the line sets a variable to gh and expands variables.
               "G=gh; P=pr; M=merge; $G $P $M 12", "G=gh P=pr; $G $P view 12", "export G=gh && $G pr view 12",
               "G=gh; xargs $G pr merge 12", "G=gh; find . -exec $G pr view 12 \\;",
               # Runners whose options and operands come before the program (Crouton at c6f8a958).
               "G=gh; flock /tmp/x $G pr merge 12", "G=gh; watch -n 1 $G pr view 12", "G=gh; xargs -n 1 $G pr view 12",
               "G=gh; ssh host $G pr merge 12",
               # A shell -c script inside an alias value, a git alias value, or a runner.
               "alias m='sh -c \"gh pr merge 12\"'", "alias m='env X=1 bash -c \"gh pr merge 12\"'",
               "git config alias.m '!sh -c \"gh pr merge 12\"'", "git -c alias.m='!sh -c \"gh pr merge 12\"' m",
               "xargs env bash -c 'gh pr merge 12'",
               # An expansion or substitution in program position, followed by pr merge.
               "$(echo gh) pr merge 12", "`echo gh` pr merge 12", "$X pr merge 12", "$X -R o/r pr merge 12",
               # Interpreter code with gh options between pr and merge (Sol at c6f8a958).
               "python3 -c 'import os; os.system(\"gh pr --repo=o/r merge 1620 --squash\")'",
               "node -e 'require(\"child_process\").execSync(\"gh pr --help=false merge 1620 --squash\")'",
               "python3 -c 'import subprocess; subprocess.run([\"gh\", \"pr\", \"-R\", \"o/r\", \"merge\", \"1620\"])'",
               # Nested shell strings under a runner, and gh api merges inside them.
               "xargs sh -c 'sh -c \"gh pr merge 1620\"'", "xargs sh -c 'gh api -X PUT repos/o/r/pulls/1620/merge'",
               "xargs sh -c \"gh api graphql -f query='mutation { enablePullRequestAutoMerge(input: {}) "
               "{ clientMutationId } }'\"",
               "alias m='gh api -X PUT repos/o/r/pulls/1620/merge'",
               "G=gh; flock /tmp/pp-lock $G pr merge 1620", "G=gh; watch -n 1 $G pr merge 1620",
               # Help is decided per simple command, so a merge next to a help request is still refused.
               "gh pr merge 12 && gh pr merge --help", "gh pr merge --help; gh pr merge 12",
               # A shell's -c under an argv runner, and a command string under a runner that runs one.
               "xargs sh -c 'gh pr merge 1'", "find . -exec sh -c 'gh pr merge 1' \\;", "watch 'gh pr merge 1'",
               "flock /tmp/x -c 'gh pr merge 1'", "entr -s 'gh pr merge 1'",
               # A gh alias or extension, and a line that moves gh's config to one (r4519428620).
               "GH_CONFIG_DIR=/tmp/c gh m", "XDG_CONFIG_HOME=/tmp gh m", "gh m", "gh co 12", "sh -c 'gh m'",
               "export GH_CONFIG_DIR=/tmp/c; gh pr view 12", "env XDG_CONFIG_HOME=/tmp gh pr view 12",
               "env -u GH_CONFIG_DIR gh pr view 12", "env -i gh pr view 12", "HOME=/tmp gh pr view 12",
               "sh -c 'GH_CONFIG_DIR=/tmp/c gh m'", "env --unset=HOME gh pr view 12",
               # gh subcommands that run or change an extension, an agent skill, a codespace, or the Copilot CLI
               # (r4519498648 at edac4614).
               "gh extension exec x", "gh ext exec x", "gh extensions exec x", "gh -R o/r extension exec x",
               "gh extension --help=false exec x", "gh ext browse", "gh ext create foo", "gh ext remove foo",
               "gh codespace ssh", "gh codespace cp a b", "gh copilot", "gh copilot -p hi", "gh skill install o/r",
               "gh skills update", "gh skill publish", "gh agent-task create 'fix it'", "gh agent-task -R o/r create x",
               "xargs gh agent-task create x",
               # Sol at edac4614: gh classified fully wherever it runs, and config redirects the line still missed.
               "GH_CONFIG_DIR=/tmp/c xargs gh m </dev/null", "xargs gh m </dev/null", "find . -maxdepth 0 -exec gh m \\;",
               "flock /tmp/x gh m", "setsid gh ext exec x", "xargs gh alias set m 'pr view'",
               "python3 -c 'import os; os.system(\"gh m\")'",
               "node -e 'require(\"child_process\").execSync(\"gh m\")'",
               "python3 -c 'import subprocess; subprocess.run([\"gh\", \"ext\", \"exec\", \"x\"])'",
               "unset GH_CONFIG_DIR; gh pr view 12", "unset HOME; gh pr view 12",
               "env -C /tmp -u GH_CONFIG_DIR gh pr view 12", "env -C /tmp -i gh pr view 12",
               "env --chdir=/tmp -u HOME gh pr view 12", "env -vu HOME gh pr view 12", "env -vu FOO gh pr merge 12",
               "GH_CONFIG_DIR=/tmp/c sh -c 'gh pr view 12'", "export HOME=/tmp; bash -c 'gh pr view 12'",
               "declare -x HOME=/tmp; gh pr view 12", "typeset -x XDG_CONFIG_HOME=/tmp; gh pr view 12",
               "set -a; HOME=/tmp; gh pr view 12", "env HOME=/tmp gh pr view 12",
               # Wrapper bundles read letter by letter: an attached value does not use up the program word.
               "timeout -k1s 5 gh pr merge 12", "env -uFOOu gh pr merge 12", "env -uXC gh pr merge 12",
               # Final round at 10689550: unknown wrapper options and long-option abbreviations, gh help, and the
               # redirects that still count.
               "env -a ignored gh pr merge 12", "env --argv0 x gh pr merge 12", "env --un FOO gh pr merge 12",
               "timeout --kill-a 1 5 gh pr merge 12", "nice --adju 5 gh pr merge 12", "/usr/bin/time --fo x gh pr merge 12",
               "env -a ignored gh pr view 12", "gh help", "gh help pr", "HOME=/tmp; gh pr view 12",
               "GH_CONFIG_DIR=/tmp/c xargs gh pr view", "gh cs ssh", "gh agent create x",
               # gh's own aliases resolve to their canonical commands before any check (19ad103f threads).
               "gh agents create x", "gh agent-tasks create x", "gh cs ls --help=false ssh", "gh skills install o/r",
               "gh extensions exec x", "xargs sh -c 'gh m'",
               # Redirect counterparts that stay refused, and runners whose program position holds gh.
               "export HOME=/tmp; gh pr view 12", "env -uGH_CONFIG_DIR gh pr view 12", "(export HOME=/tmp; gh pr view 12)",
               "(export HOME=/tmp; (gh pr view 12))", "(export HOME=/tmp; gh pr view 12 | cat)",
               "HOME=/tmp find . -exec gh pr view 12 \\;",
               # xargs options read the way getopt reads them (Sol at 4ed57bd3); an unknown one falls back to
               # trying every word. Every find -exec is a program position, even as another option's value.
               "printf '12\\n' | xargs -rI {} gh pr merge {}", "xargs --replace gh pr merge 12", "xargs -i gh pr merge 12",
               "xargs -0rn1 gh pr merge", "xargs -I{} gh pr merge {}", "xargs --replace={} gh pr merge {}",
               "xargs -a f.txt gh pr merge", "xargs -Z gh pr merge 12", "xargs --rep gh pr merge 12",
               "xargs -l gh pr merge 12", "find . -name -exec gh pr merge {} \\;",
               "find . -exec echo ';' -exec gh pr merge {} \\;", "env -S 'gh pr view' 12",
               "HOME=/tmp flock /tmp/x gh pr view 12", "env -C/tmp gh pr merge 12", "timeout -k0.5s 5 gh pr merge 12",
               "/usr/bin/time -f%E gh pr merge 12", "env -x/tmp gh pr view 12",
               "xargs find . -exec sh -c 'gh pr merge 1' \\;", "find . -exec xargs gh pr merge 1 \\;",
               "sudo -nu bob gh pr merge 12", "env -xu FOO gh pr merge 12", "env -vuFOO gh pr merge 12",
               "nice -n5 gh pr merge 12", "timeout -s9 5 gh pr merge 12", "sudo -uroot gh pr merge 12",
               "time -fx gh pr merge 12", "exec -abash gh pr merge 12", "stdbuf -oL gh pr merge 12",
               "ionice -c2 gh pr merge 12", "taskset -c0 gh pr merge 12",
               # Nested shells (Sol at 7a96c572); past the nesting limit the guard fails closed.
               "xargs sh -c \"sh -c \\\"sh -c 'gh pr merge 1'\\\"\"",
               "setsid sh -c \"sh -c \\\"sh -c 'gh pr merge 1'\\\"\"",
               "xargs " + nested_shells("gh pr merge 1", 8),
               # Accepted edge: under a runner other than find or xargs each later word is tried as its program.
               "setsid grep -e gh -e pr -e merge docs/x.md",
               # Quotes and backslashes are removed before interpreter code is matched.
               "python3 -c 'import os; os.system(\"gh pr m\\\\erge 12\")'",
               "python3 -c \"import os; os.system('gh pr me''rge 12')\"", "entr -cs 'gh pr merge 1'",
               "G=/usr/bin/gh; \"$G\" api -X PUT repos/o/r/pulls/12/merge", "declare G='gh pr'; $G view 12",
               # gh aliases and extensions run commands the guard never sees.
               "gh alias set m 'pr view 12'", "gh alias set --shell m 'gh pr view 12'", "gh alias import aliases.yml",
               "gh extension install owner/repo", "gh extensions install owner/repo", "gh ext install ./local",
               "gh extension upgrade --all",
               # Options between gh, the group, and the action (Sol at ea18d1aa).
               'gh alias --help=false set m "api -X PUT repos/o/r/pulls/12/merge"', "gh extension --help=false install o/r",
               "gh alias -R x set m 'pr view 12'", "gh alias --repo=x set m 'pr view 12'", "gh ext -R x install o/r",
               "gh extension --repo=o/r upgrade --all", "gh pr --help=false merge 12", "gh --repo=x pr merge 12",
               # A GraphQL merge under a runner, with the query's literal $id in single quotes.
               "xargs gh api graphql -f query='mutation($id: ID!) { mergePullRequest(input: {pullRequestId: $id}) "
               "{ clientMutationId } }'")
    ALLOWED = (# A quoted argument that only mentions the command is data, not a merge (r4515518236).
               f"gh pr comment 1650 --body \"gh pr merge 1650 {SHA_ARG}\"",
               f"git commit -m 'Merged with gh pr merge 1650 {SHA_ARG}'", 'grep -rn "gh pr merge" docs/',
               "echo 'gh pr merge 12 --squash' > notes.md",
               # An expansion used as data, not as the program, on a line that sets a variable to gh.
               "G=gh; echo $HOME", "G=gh; git status; echo $X", "G=gh; X=$HOME",
               "echo $(date)", "$EDITOR notes.md", "$(which gh) pr view 12",
               # Help on the normalized words is help, as on main (Sol P3), and code that never runs gh pr merge.
               "gh pr --repo=o/r merge --help", "gh -R o/r pr merge -h", "python3 -c 'print(\"merge sort\")'",
               # Help in a compound line or behind a wrapper, and an ordinary program's data arguments (cd78c5f3).
               "gh pr merge --help && echo ok", "gh pr merge --help; echo done", "gh pr --repo=o/r merge --help | less",
               "command gh pr merge --help", "echo gh pr merge 12", "grep -e gh -e pr -e merge docs/x.md",
               # An argv runner's program arguments are data (r4518829993).
               "find docs -name '*.md' -exec grep -l 'gh pr merge' {} +", "printf 'a.md\\n' | xargs grep -n 'gh pr merge'",
               "xargs echo 'gh pr merge 1'", "entr -s 'make test'",
               # Built-in gh commands, and a config variable on a line without gh.
               "gh run list --limit 5", "gh api repos/o/r/pulls/12", "gh version", "gh ext list",
               "gh repo view", "gh auth status", "GH_CONFIG_DIR=/tmp/c echo hi", "export FOO=1; gh pr view 12",
               # Read-only and help forms of those groups, and env -u of a variable that does not move gh's config.
               "gh extension search foo", "gh extension exec --help", "gh ext", "gh codespace", "gh codespace --help",
               "gh copilot --help", "gh skill list", "gh skill preview o/r x", "gh alias list", "gh alias delete m",
               "gh agent-task list", "gh agent-task view 1", "gh agent-task create --help",
               "gh attestation verify f --owner o", "env -u PATH gh pr view 12", "env --unset=FOO gh pr view 12",
               # Words that only look like an assignment, and env options that leave gh's config alone.
               "gh pr comment 12 --body HOME=/tmp", "echo HOME=/tmp; gh pr view 12", "env -u FOO gh pr view 12",
               "env -C /tmp gh pr view 12", "env -C /tmp -u PATH gh pr view 12", "env -vu FOO gh pr view 12",
               "unset FOO; gh pr view 12", "HOME=/tmp echo hi", "find . -maxdepth 0 -exec gh pr view 12 \\;",
               "timeout -k1s 5 gh pr view 12", "env -uFOO gh pr view 12", "nice -19 gh pr view 12",
               # A redirect counts only for the command it is attached to, or after export, declare or unset, and
               # only for a gh program; gh's own command aliases are built in.
               "HOME=/tmp echo gh", "HOME=/tmp python3 -c 'print(\"gh pr view 12\")'", "HOME=/tmp npm test; gh pr view 12",
               "HOME=/tmp npm test; gh pr comment 12 --body \"Verified.\"", "gh at verify artifact --owner o",
               "gh rs list", "gh agent list", "gh agent-tasks list", "gh agents view 1", "gh extension ls", "gh skill ls",
               "gh skill show o/r x", "gh pr --help", "gh pr view --help", "gh --help", "env --chdir=/tmp gh pr view 12",
               "env --unset FOO gh pr view 12", "timeout --kill-after=1 5 gh pr view 12",
               "nice --adjustment 5 gh pr view 12", "/usr/bin/time --format x gh pr view 12",
               "sudo --preserve-env=PATH gh pr view 12", "nice --help", "env --version",
               "gh at download artifact", "gh rs ls", "gh rs view 1", "gh rs check main", "gh agent view 1",
               "gh extensions ls",
               # Sol at 19ad103f: attached wrapper operands, runner data, redirects kept inside a subshell or a
               # pipeline stage, and every group's ls alias.
               "env -C/tmp gh pr view 12", "timeout -k0.5s 5 gh pr view 12",
               "/usr/bin/time -f%E gh pr view 12", "env -uGH_TOKEN npm test", "HOME=/tmp xargs echo gh",
               "HOME=/tmp find docs -name gh -print",
               "gh variable ls", "gh release ls", "gh pr ls", "gh secret ls",
               "gh repo ls", "gh alias ls", "find docs -exec grep -e gh -e pr -e merge {} +",

               "env -uSSH_AUTH_SOCK gh pr view 12", "env -uSSH_AUTH_SOCK npm test", "xargs grep -e gh -e pr -e merge",
               "printf 'x\\n' | xargs -rI {} echo {}", "xargs -0 -r -n 1 -P 4 echo",
               "sudo -nu bob gh pr view 12", "env -vuFOO gh pr view 12", "nice -n5 gh pr view 12",
               "sudo -uroot gh pr view 12", "time -fx gh pr view 12", "stdbuf -oL gh pr view 12",
               "ionice -c2 gh pr view 12", "taskset -c0 gh pr view 12",
               "python3 -c 'import os; os.system(\"gh pr view 12\")'", "xargs " + nested_shells("echo hi", 8),
               # entr runs its arguments as a shell command line only with -s.
               "entr echo 'gh pr merge 1'",
               "node -e 'require(\"child_process\").execSync(\"gh pr view 12\")'",
               "sh -c 'gh api repos/o/r/pulls/12/merge'",
               "gh pr view 12", "printf 12 | xargs gh pr view", "xargs gh pr view 12 --json mergeable",
               "find . -name '*.md' -exec grep -l merge {} \\;", "setsid gh pr checks 12", "N=12; gh pr view $N",
               "X=1; echo $X", "gh pr merge --help", "gh pr comment 12 -b 'Ready to merge once CI is green.'",
               "gh alias list", "gh alias set --help", "gh alias delete m", "gh extension list",
               "gh extension install --help", "gh alias --help",
               # A literal $ inside single quotes is data, not an expansion.
               "xargs gh api graphql -f query='query($id: ID!) { node(id: $id) { id } }'")

    def setUp(self):
        self.make_worktrees()

    def run_all(self, commands, cwd, coordinator):
        return [(command, self.results(command, cwd, coordinator)) for command in commands]

    def test_background_separator_keeps_following_config_changes_in_current_shell(self):
        commands = ("true & export GH_CONFIG_DIR=/tmp/evil; gh pr view 12",
                    "true & HOME=/tmp; gh pr view 12",
                    "true & declare -x HOME=/tmp; gh pr view 12",
                    "echo \\|& export GH_CONFIG_DIR=/tmp/evil; gh pr view 12")
        for command, results in self.run_all(commands, self.lane, None):
            for result in results:
                with self.subTest(command=command):
                    self.assertEqual(result.returncode, 2, result.stderr)
                    self.assertIn("gh config redirected", result.stderr)

    def test_pipeline_after_background_separator_keeps_each_stage_scoped(self):
        guard = in_process(CLI)
        parsed = guard.shell_commands("a & b | c")
        self.assertEqual([args for args, _, _ in parsed], [["a"], ["b"], ["c"]])
        stages = [scoped[1] for _, scoped, _ in parsed[1:]]
        self.assertTrue(all(stage is not None for stage in stages))
        self.assertEqual(len(set(stages)), 2)
        for command in ("true & echo hi | export HOME=/tmp; gh pr view 12",
                        "true & export HOME=/tmp | cat; gh pr view 12",
                        "export HOME=/tmp & gh pr view 12"):
            for result in self.results(command, self.lane, None):
                with self.subTest(command=command):
                    self.assertEqual(result.returncode, 2, result.stderr)

    def test_stderr_pipe_keeps_cd_and_exports_in_pipeline_scope(self):
        with tempfile.TemporaryDirectory() as folder:
            main, lane = Path(folder, "main-wt"), Path(folder, "lane-wt")
            for path, branch in ((main, "main"), (lane, "topic")):
                subprocess.run(["git", "init", "-q", "-b", branch, str(path)], check=True)
            for operator in ("|&", "|&\n", "|\\\n&", "|\n", "|\\\r\n&"):
                command = f"true {operator} cd ../lane-wt; git push origin HEAD"
                for result in self.results(command, str(main), None):
                    with self.subTest(command=command):
                        self.assertEqual(result.returncode, 2, result.stderr)
                for declaration in ("export GH_CONFIG_DIR=/tmp/evil", "export HOME=/tmp"):
                    command = f"true {operator} {declaration}; gh pr view 12"
                    for result in self.results(command, str(main), None):
                        with self.subTest(command=command):
                            self.assertEqual(result.returncode, 2, result.stderr)
        guard = in_process(CLI)
        self.assertTrue(all(scope for _, scope, _ in guard.shell_commands("a |& b")))

    def test_python_executable_suffixes_are_recognized(self):
        for executable in ("python3-dbg", "python3.14T", "python3.12-dbg-x86_64", "python3.14t", "/usr/bin/python3.14t-dbg"):
            command = f"{executable} -c 'import os; os.system(\"gh pr merge 12\")'"
            for cwd, coordinator in ((self.lane, None), (self.coordinator, self.coordinator)):
                for result in self.results(command, cwd, coordinator):
                    with self.subTest(command=command, cwd=cwd):
                        self.assertEqual(result.returncode, 2, result.stderr)
            for result in self.results(f"{executable} -c 'print(1)'", self.lane, None):
                self.assertEqual(result.returncode, 0, result.stderr)

    def test_daily_lane_runners_refuse_merges(self):
        commands = ("xvfb-run -a gh pr merge 12", "pp-netns gh pr merge 12",
                    "direnv exec . gh pr merge 12", "xvfb-run -a pp-netns gh pr merge 12",
                    "xvfb-run --server-args='-screen 0 800x600x24' gh pr merge 12")
        for cwd, coordinator in ((self.lane, None), (self.coordinator, self.coordinator)):
            for command, results in self.run_all(commands, cwd, coordinator):
                for result in results:
                    with self.subTest(command=command, cwd=cwd):
                        self.assertEqual(result.returncode, 2, result.stderr)
        for command in ("xvfb-run -a gh pr view 12", "pp-netns gh pr view 12",
                        "direnv exec . gh pr view 12", "xvfb-run --server-args='-screen 0 800x600x24' gh pr view 12"):
            for result in self.results(command, self.lane, None):
                with self.subTest(command=command):
                    self.assertEqual(result.returncode, 0, result.stderr)

    def test_daily_wrappers_expose_gh_config_redirects(self):
        commands = ("export GH_CONFIG_DIR=/tmp/evil; direnv exec . gh pr view 12",
                    "HOME=/tmp direnv exec . gh pr comment 12 -b x",
                    "export GH_CONFIG_DIR=/tmp/evil; xvfb-run -e /tmp/err -f /tmp/auth gh pr view 12")
        for command, results in self.run_all(commands, self.lane, None):
            for result in results:
                with self.subTest(command=command):
                    self.assertEqual(result.returncode, 2, result.stderr)
                    self.assertIn("gh config redirected", result.stderr)

    def test_daily_wrappers_expose_git_refusals(self):
        commands = ("direnv exec . git push -f", "xvfb-run -a git push --force origin topic",
                    "pp-netns git push origin HEAD:main", "direnv exec . git commit --no-verify -m x")
        for command, results in self.run_all(commands, self.lane, None):
            for result in results:
                with self.subTest(command=command):
                    self.assertEqual(result.returncode, 2, result.stderr)

    def test_daily_wrapper_options_resolve_the_real_program(self):
        guard = in_process(CLI)
        for option in ("-a", "-n 90", "-s '-screen 0 800x600x24'", "-e /tmp/err", "-f /tmp/auth",
                       "-p MIT-MAGIC-COOKIE-1", "-w 2", "--auto-servernum", "--server-num=90",
                       "--server-args='-screen 0 800x600x24'", "--error-file=/tmp/err", "--auth-file=/tmp/auth",
                       "--xauth-protocol=MIT-MAGIC-COOKIE-1", "--wait=2"):
            words, *_ = guard.shell_commands(f"xvfb-run {option} gh pr view 12")[0]
            with self.subTest(option=option):
                self.assertEqual(guard.program(words)[:2], ("gh", ["pr", "view", "12"]))
        self.assertEqual(guard.program(["pp-netns", "--", "git", "status"])[:2], ("git", ["status"]))
        self.assertEqual(guard.program(["direnv", "exec", ".", "git", "status"])[:2], ("git", ["status"]))

    def test_background_and_or_lists_do_not_move_parent_cwd(self):
        with tempfile.TemporaryDirectory() as folder:
            main, lane = Path(folder, "main-wt"), Path(folder, "lane-wt")
            for path, branch in ((main, "main"), (lane, "topic")):
                subprocess.run(["git", "init", "-q", "-b", branch, str(path)], check=True)
            for operator in ("&&", "||", "&&\n"):
                command = f"cd ../lane-wt {operator} git status & git push origin HEAD"
                for result in self.results(command, str(main), None):
                    with self.subTest(command=command):
                        self.assertEqual(result.returncode, 2, result.stderr)
        guard = in_process(CLI)
        parsed = guard.shell_commands("a; b && c & d")
        self.assertFalse(parsed[0][1])
        self.assertFalse(parsed[1][1])
        self.assertTrue(parsed[2][1])
        self.assertFalse(parsed[3][1])

    def test_routine_wrapped_builds_and_background_jobs_stay_allowed(self):
        for command in ("xvfb-run -a cargo test", "direnv exec . cargo build", "pp-netns npx vite --port 5173",
                        "cargo build & npm run dev; wait"):
            for result in self.results(command, self.lane, None):
                with self.subTest(command=command):
                    self.assertEqual(result.returncode, 0, result.stderr)

    def test_background_list_exports_reach_only_their_own_subshell(self):
        for command in ("export GH_CONFIG_DIR=/tmp/evil && gh pr view 12 &",
                        "(export GH_CONFIG_DIR=/tmp/evil; gh pr view 12) &"):
            for result in self.results(command, self.lane, None):
                with self.subTest(command=command):
                    self.assertEqual(result.returncode, 2, result.stderr)
                    self.assertIn("gh config redirected", result.stderr)
        for result in self.results("export GH_CONFIG_DIR=/tmp/x && true & gh pr view 12", self.lane, None):
            self.assertEqual(result.returncode, 2, result.stderr)

    def test_direnv_file_targets_are_programs(self):
        for command in ("direnv exec /usr/bin/gh pr merge 12",
                        "export GH_CONFIG_DIR=/tmp/evil; direnv exec /usr/bin/gh pr view 12",
                        "direnv exec /usr/bin/git push -f"):
            for result in self.results(command, self.lane, None):
                with self.subTest(command=command):
                    self.assertEqual(result.returncode, 2, result.stderr)
        guard = in_process(CLI)
        for command in ("direnv allow", "direnv status", "direnv reload",
                        "direnv exec . git push origin topic"):
            for result in self.results(command, self.lane, None):
                with self.subTest(command=command):
                    self.assertEqual(result.returncode, 0, result.stderr)
        readings = guard.program_readings(["direnv", "exec", "environment", "git", "status"])
        self.assertEqual({(name, tuple(rest)) for name, rest, *_ in readings},
                         {("git", ("status",)), ("environment", ("git", "status"))})

    def test_direnv_checks_both_readings_without_stat(self):
        commands = ('direnv exec "$PWD" gh pr merge 12', "direnv exec $PWD gh pr merge 12",
                    "direnv exec ~ git push -f origin topic", "mkdir -p d && direnv exec d gh pr merge 12",
                    "env -C / direnv exec usr/bin gh pr merge 12", "cd / && direnv exec usr/bin gh pr merge 12",
                    "git worktree add ../w2 && direnv exec ../w2 git push origin HEAD:main")
        for command, results in self.run_all(commands, self.lane, None):
            for result in results:
                with self.subTest(command=command):
                    self.assertEqual(result.returncode, 2, result.stderr)
        guard = in_process(CLI)
        with patch.object(guard.os.path, "isdir", side_effect=AssertionError("direnv must not stat PATH")):
            self.assertIsNone(guard.check_command("direnv exec . cargo build", self.lane))

    def test_background_and_directory_changes_make_the_whole_line_unknown(self):
        with tempfile.TemporaryDirectory() as folder:
            main, lane = Path(folder, "main-wt"), Path(folder, "lane-wt")
            for path, branch in ((main, "main"), (lane, "topic")):
                subprocess.run(["git", "init", "-q", "-b", branch, str(path)], check=True)
            commands = ("cd ../main-wt && git push origin HEAD | cat &",
                        "cd ../main-wt && git push origin HEAD 2>&1 | tee /tmp/guard-test.log &",
                        "cd ../main-wt && git push origin HEAD |& cat &",
                        "cd ../main-wt && true | git push origin HEAD &",
                        "(cd ../main-wt && (git push origin HEAD | cat)) &",
                        "env -C / git push origin topic &", "git -C / push origin topic &",
                        "pushd /; git push origin topic &", "cd / & gh pr comment 12 -b x")
            for command in commands:
                for result in self.results(command, str(lane), None):
                    with self.subTest(command=command):
                        self.assertEqual(result.returncode, 2, result.stderr)
            command = "{ cd ../lane-wt; } & git push origin HEAD"
            for result in self.results(command, str(main), None):
                self.assertEqual(result.returncode, 2, result.stderr)

    def test_redirects_taint_gh_across_all_scopes(self):
        commands = ("(export GH_CONFIG_DIR=/tmp/evil; gh pr view 12 &)",
                    "(export GH_CONFIG_DIR=/tmp/evil; (gh pr view 12) &)",
                    "export GH_CONFIG_DIR=/tmp/x && true & gh pr view 12",
                    "env -uGH_TOKEN gh pr view 12", "(export HOME=/tmp); gh pr view 12",
                    "export HOME=/tmp | cat; gh pr view 12", "(export HOME=/tmp); (gh pr view 12)",
                    "export HOME=/tmp | gh pr view 12", "(declare GH_TOKEN=x); gh pr view 12",
                    "env GH_TOKEN=x echo hi; gh pr view 12")
        for command, results in self.run_all(commands, self.lane, None):
            for result in results:
                with self.subTest(command=command):
                    self.assertEqual(result.returncode, 2, result.stderr)
        for command in ("HOME=/tmp npm test; gh pr view 12", "GH_TOKEN=x echo hi; gh pr view 12"):
            for result in self.results(command, self.lane, None):
                self.assertEqual(result.returncode, 0, result.stderr)

    def test_coproc_with_git_or_gh_is_unclear(self):
        for command in ("coproc { export GH_CONFIG_DIR=/tmp/evil; gh pr view 12; }", "coproc git status"):
            for result in self.results(command, self.lane, None):
                with self.subTest(command=command):
                    self.assertEqual(result.returncode, 2, result.stderr)
                    self.assertIn("Cannot classify", result.stderr)

    def test_required_routine_commands_stay_allowed(self):
        commands = ("xvfb-run -a cargo test", "direnv exec . cargo build", "pp-netns npx vite --port 5173",
                    "cargo build & npm run dev; wait", "direnv allow", "direnv status",
                    "xvfb-run -a gh pr view 12 --json mergeable", "gh pr view 12 2>&1",
                    "git push origin topic", "direnv exec . git push origin topic")
        for command, results in self.run_all(commands, self.lane, None):
            for result in results:
                with self.subTest(command=command):
                    self.assertEqual(result.returncode, 0, result.stderr)

    def test_hidden_merges_are_refused_everywhere(self):
        for cwd, coordinator in ((self.lane, None), (self.coordinator, self.coordinator)):
            for command, results in self.run_all(self.REFUSED, cwd, coordinator):
                for result in results:
                    with self.subTest(command=command, cwd=cwd):
                        self.assertEqual(result.returncode, 2, result.stderr)

    def test_runner_words_are_classified_quickly(self):
        # A runner tries every later word as its program; runner names as data once took 18 s for 40 words and
        # 22.9 s for 800 (Sol at c545852a), past the hook's 10-second limit.
        guard = in_process(ROOT / "scripts/agent-guard.py")
        for words, limit in ((40, 1.0), (800, 2.5)):
            flood = "xargs echo " + " ".join(["find"] * words)
            started = time.monotonic()
            reason = guard.check_command(flood, self.lane)
            with self.subTest(words=words):
                self.assertLess(time.monotonic() - started, limit)
                self.assertTrue(reason is None or "too complex" in reason, reason)
                if words == 40:
                    self.assertIsNone(reason)
        self.assertIsNotNone(guard.check_command("setsid " + " ".join(["find"] * 40) + " gh pr merge 1", self.lane))
        # An ordinary long line stays allowed quickly.
        started = time.monotonic()
        self.assertIsNone(guard.check_command("echo " + " ".join(f"word{i}" for i in range(5000)), self.lane))
        self.assertLess(time.monotonic() - started, 1.0)

    def test_the_check_refuses_a_line_past_its_budget_or_deadline(self):
        guard = in_process(ROOT / "scripts/agent-guard.py")
        self.assertIn("too complex", guard.check_command("setsid echo " + " ".join(["find"] * 6000), self.lane))
        with patch.object(guard, "CHECK_SECONDS", 0):
            self.assertIn("too complex", guard.check_command("xargs echo find find", self.lane))
        self.assertIsNone(guard.check_command("xargs echo find find", self.lane))

    def test_whole_line_limits(self):
        # Sol at f7f4141d: nested sh -c scripts each started fresh limits, so a 112 KB line ran past the hook's
        # 10-second limit, and a 5 MB python3 -c took 3.6 s.
        guard = in_process(ROOT / "scripts/agent-guard.py")
        inner = "xargs echo " + " ".join(['watch " "'] * 700)
        sol = "; ".join("sh -c " + shlex.quote(inner) for _ in range(16))
        for command in (sol + "; xargs gh pr merge 1", sol, "python3 -c '" + "pass;" * 1_000_000 + "'"):
            started = time.monotonic()
            reason = guard.check_command(command, self.lane)
            with self.subTest(size=len(command)):
                self.assertLess(time.monotonic() - started, 1.0)
                self.assertIn("too long", reason)
        # An ordinary long line under the cap stays allowed.
        with patch.object(guard, "CHECK_SECONDS", 60):  # the step budget, not the clock, decides this line
            self.assertIsNone(guard.check_command("xargs echo " + " ".join(f"file{i}" for i in range(2000)),
                                                  self.lane))
        # Thousands of nested substitutions are refused at the deadline, not after the hook's limit. A 0.2 s
        # deadline makes the deadline, not the machine's speed, end the parse.
        started = time.monotonic()
        with patch.object(guard, "CHECK_SECONDS", 0.2):
            self.assertIn("too complex", guard.check_command("echo " + "$(" * 3000 + "x" + ")" * 3000, self.lane))
        self.assertLess(time.monotonic() - started, 1.0)

    def test_memory_stays_small_on_a_long_line(self):
        # r4519290573 at f45f9f5c: the memo copied every suffix it visited; 32K one-letter words peaked at 535 MB.
        guard = in_process(ROOT / "scripts/agent-guard.py")
        tracemalloc.start()
        try:
            with patch.object(guard, "CHECK_SECONDS", 60):  # the step budget, not the clock, ends this line
                reason = guard.check_command("xargs echo " + " ".join(["a"] * 32000), self.lane)
            peak = tracemalloc.get_traced_memory()[1]
        finally:
            tracemalloc.stop()
        # xargs runs echo, so the words are data; the line is allowed or, past the budget, refused as too complex.
        self.assertTrue(reason is None or "too complex" in reason, reason)
        self.assertLess(peak, 50_000_000)

    def test_deep_recursion_in_a_nested_script_is_too_complex(self):
        guard = in_process(ROOT / "scripts/agent-guard.py")
        nested = "sh -c " + shlex.quote("echo " + "$(" * 600 + "x" + ")" * 600)
        # A low recursion limit and a long deadline make the parser's RecursionError, not the clock, end the check.
        limit = sys.getrecursionlimit()
        sys.setrecursionlimit(400)
        try:
            with patch.object(guard, "CHECK_SECONDS", 60):
                self.assertIn("too complex", guard.check_command(nested, self.lane))
        finally:
            sys.setrecursionlimit(limit)

    def test_git_lookups_are_cached_and_bounded_per_line(self):
        # Sol at f45f9f5c: each git push ran git config again, so 1,000 pushes took 20 s and both adapters timed
        # out at 10 s with no decision.
        pushes = "; ".join(["git push origin HEAD:review-topic"] * 1000)
        for command, expect in ((pushes + "; gh pr merge 1", 2), (pushes, 0)):
            started = time.monotonic()
            for result in self.results(command, self.lane, None):
                with self.subTest(merge=bool(expect)):
                    self.assertEqual(result.returncode, expect, result.stderr)
            self.assertLess(time.monotonic() - started, 2.5)
        # One line runs each distinct git lookup once.
        guard = in_process(ROOT / "scripts/agent-guard.py")
        real, calls = guard.subprocess.run, []
        with patch.object(guard.subprocess, "run", lambda *a, **k: calls.append(a[0]) or real(*a, **k)):
            self.assertIsNone(guard.check_command(pushes, self.lane))
        self.assertLessEqual(len(calls), 3, calls)
        self.assertIsNone(guard.LINE_GIT)
        # Lookups the cache cannot share still stop at the line's deadline. git is stubbed to take 10 ms and the
        # deadline set to 0.2 s, so the timeout path runs on any machine; each git call gets at most the time left.
        # Keep parsing below the synthetic deadline so this case exercises Git lookup timeouts.
        distinct = "; ".join(f"git -c pp.k{k}=1 push origin HEAD:review-topic" for k in range(100))
        timeouts = []

        def slow_git(args, **kwargs):
            timeouts.append(kwargs["timeout"])
            time.sleep(0.01)
            return subprocess.CompletedProcess(args, 1, "", "")

        with patch.object(guard, "CHECK_SECONDS", 0.2), patch.object(guard.subprocess, "run", slow_git):
            self.assertIn("too complex", guard.check_command(distinct, self.lane))
        self.assertTrue(timeouts and max(timeouts) <= 0.2, timeouts[:3])

    def test_a_marker_line_that_runs_out_of_time_is_too_complex(self):
        # r4519380166 at 25d70ecf: coordinator_worktree runs git through run_git, which can raise TooComplex; the
        # marker check now sits inside check_command's handler, so the result is a refusal, not an exception.
        guard = in_process(ROOT / "scripts/agent-guard.py")
        marker = "pp-review" + "-gate"
        with patch.dict(os.environ, {"PP_COORDINATOR_WORKTREE": self.coordinator}), \
                patch.object(guard, "CHECK_SECONDS", 0):
            self.assertIn("too complex", guard.check_command(f"echo '{marker}'", self.coordinator))
        # When git only times out before the line's deadline, the coordinator check is false and the marker rule
        # refuses as before.
        with patch.dict(os.environ, {"PP_COORDINATOR_WORKTREE": self.coordinator}), \
                patch.object(guard.subprocess, "run", lambda *a, **k: (_ for _ in ()).throw(
                    subprocess.TimeoutExpired(a[0], 1))):
            self.assertIn("Only the coordinator worktree posts", guard.check_command(f"echo '{marker}'", self.coordinator))

    def test_nested_scripts_share_one_line_state(self):
        guard = in_process(ROOT / "scripts/agent-guard.py")
        made = []
        real = guard.merge_state
        with patch.object(guard, "merge_state", lambda: made.append(1) or real()):
            self.assertIsNone(guard.check_command("sh -c 'echo a'; bash -c 'sh -c \"echo b\"'", self.lane))
        self.assertEqual(len(made), 1)
        self.assertIsNone(guard.LINE_DEADLINE)

    def test_top_level_shell_scripts_keep_their_own_cwd_and_input_rules(self):
        # 19ad103f thread: a top-level sh -c script is checked by check_command with its own cwd and alone, so a
        # body or query file read by the script's only command is allowed, as on main.
        Path(self.lane, "body.md").write_text("Verified.\n")
        Path(self.lane, "q.graphql").write_text("query { viewer { login } }\n")
        for command in ("bash -c 'gh pr comment 12 --body-file body.md'", "sh -c 'gh api graphql -F query=@q.graphql'"):
            for result in self.results(command, self.lane, None):
                with self.subTest(command=command):
                    self.assertEqual(result.returncode, 0, result.stderr)

    def test_gh_aliases_and_config_redirects_name_the_reason(self):
        guard = in_process(ROOT / "scripts/agent-guard.py")
        for command in ("GH_CONFIG_DIR=/tmp/c gh m", "XDG_CONFIG_HOME=/tmp gh m", "env -i gh pr view 12"):
            with self.subTest(command=command):
                self.assertIn("gh config redirected", guard.check_command(command, self.lane))
        for command, name in (("gh extension exec x", "extension exec"), ("gh copilot", "copilot")):
            with self.subTest(command=command):
                self.assertIn(f"`gh {name}` runs or changes code", guard.check_command(command, self.lane))
        for command, name in (("gh m", "m"), ("gh co 12", "co"), ("gh -R o/r my-ext run", "my-ext")):
            with self.subTest(command=command):
                reason = guard.check_command(command, self.lane)
                self.assertIn(f"`gh {name}` is not a built-in gh command", reason)
                self.assertIn("alias or an extension", reason)

    def test_split_variables_name_the_reason(self):
        for command, results in self.run_all(("G=gh; P=pr; M=merge; $G $P $M 12", "G=gh; $G pr view 12"),
                                              self.lane, None):
            for result in results:
                with self.subTest(command=command):
                    self.assertIn("built from variables", result.stderr)

    def test_ordinary_commands_stay_allowed(self):
        for command, results in self.run_all(self.ALLOWED, self.lane, None):
            for result in results:
                with self.subTest(command=command):
                    self.assertEqual(result.returncode, 0, result.stderr)

    def test_coordinator_merge_is_refused_with_git_location_env(self):
        for name, value in (("GIT_DIR", str(Path(self.lane, ".git"))), ("GIT_WORK_TREE", self.lane)):
            for result in self.results(MERGE_FORM, self.coordinator, self.coordinator, {name: value}):
                with self.subTest(name=name):
                    self.assertEqual(result.returncode, 2, result.stderr)
        for result in self.results(MERGE_FORM, self.coordinator, self.coordinator):
            self.assertEqual(result.returncode, 0, result.stderr)

    def test_gh_config_files_are_protected(self):
        with tempfile.TemporaryDirectory() as folder:
            custom = str(Path(folder, "ghconf"))
            env = {"GH_CONFIG_DIR": custom, "XDG_CONFIG_HOME": str(Path(folder, "xdg"))}
            paths = (str(Path.home() / ".config/gh/config.yml"), str(Path.home() / ".config/gh/hosts.yml"),
                     f"{custom}/config.yml", str(Path(folder, "xdg/gh/config.yml")), "~/.config/gh/config.yml")
            calls = [claude_call(tool, file_path=path, **fields) for path in paths
                     for tool, fields in (("Write", {"content": "aliases: {}"}),
                                          ("Edit", {"old_string": "a", "new_string": "b"}))]
            results = [hook(CLAUDE, call, None, env) for call in calls]
            for call, result in zip(calls, results):
                with self.subTest(call=call["tool_input"]["file_path"]):
                    self.assertEqual(result.returncode, 2, result.stderr)
                    self.assertIn("gh config", result.stderr)
            # Relative paths resolve against the hook's working directory, and so does a relative GH_CONFIG_DIR.
            repo = Path(folder, "home/repo")
            repo.mkdir(parents=True)
            home = {"HOME": str(Path(folder, "home")), "XDG_CONFIG_HOME": "", "GH_CONFIG_DIR": ""}
            for path, extra in (("../.config/gh/config.yml", home), ("ghconf/hosts.yml", {**home, "GH_CONFIG_DIR": "ghconf"}),
                                ("./../.config/gh/hosts.yml", home)):
                result = hook(CLAUDE, claude_call("Write", str(repo), file_path=path, content="x"), None, extra, cwd=repo)
                with self.subTest(path=path, extra=extra):
                    self.assertEqual(result.returncode, 2, result.stderr)
                    self.assertIn("gh config", result.stderr)
            lookalike = hook(CLAUDE, claude_call("Write", str(repo), file_path="docs/.config/gh/example.md", content="x"), None,
                             home, cwd=repo)
            self.assertEqual(lookalike.returncode, 0, lookalike.stderr)
            # The Claude adapter passes the payload's cwd, which wins over the hook process's own directory.
            for tool in CLAUDE_TOOLS - {"Bash"}:
                key = "notebook_path" if tool == "NotebookEdit" else "file_path"
                payload = claude_call(tool, str(repo), **{key: "../.config/gh/config.yml"})
                result = hook(CLAUDE, payload, None, home, cwd="/")
                with self.subTest(tool=tool):
                    self.assertEqual(result.returncode, 2, result.stderr)
                    self.assertIn("gh config", result.stderr)
            # A repository folder that only looks like the config path is not the gh config.
            for path in (str(Path(folder, "notes/config.yml")), str(Path(folder, "repo/docs/.config/gh/example.md"))):
                allowed = hook(CLAUDE, claude_call("Write", file_path=path, content="x"), None, env)
                with self.subTest(path=path):
                    self.assertEqual(allowed.returncode, 0, allowed.stderr)


class RetiredFleetIntakeTests(unittest.TestCase):
    def test_lane_rules_no_longer_drive_intake_or_merges(self):
        text = (ROOT / "scripts/fleet/lane-rules.yaml").read_text(encoding="utf-8")
        live = "\n".join(line.split(" #")[0] for line in text.splitlines() if not line.lstrip().startswith("#"))
        for key in ("picker", "claim", "linear_access", "merge_gate", "close_out", "end_of_wake", "author_overflow",
                    "scorer_deadline", "seat_rule", "scorer_policy", "complex_seat", "classes", "pick_rule", "wake"):
            with self.subTest(key=key):
                self.assertNotRegex(live, rf"(?m)^\s*{key}\s*:")
        self.assertEqual(sorted(path.name for path in (ROOT / "scripts/fleet").iterdir()), ["lane-rules.yaml"])


if __name__ == "__main__":
    unittest.main()
