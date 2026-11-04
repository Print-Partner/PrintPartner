import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
GUARD = ROOT / ".factory/hooks/guard.py"


def run(tool, data):
    payload = json.dumps({"hook_event_name": "PreToolUse", "tool_name": tool, "tool_input": data})
    return subprocess.run([sys.executable, "-B", str(GUARD)], input=payload,
                          capture_output=True, text=True, check=False)


class GuardHookTests(unittest.TestCase):
    def assertBlocked(self, tool, data, reason):
        result = run(tool, data)
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn(reason, result.stderr)

    def assertAllowed(self, tool, data):
        result = run(tool, data)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_hook_config_runs_guard_for_mutating_tools(self):
        config = json.loads((ROOT / ".factory/hooks.json").read_text())
        (group,) = config["PreToolUse"]
        self.assertEqual(set(group["matcher"].split("|")), {"Execute", "Create", "Edit", "ApplyPatch"})
        self.assertIn(".factory/hooks/guard.py", group["hooks"][0]["command"])

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
                result = subprocess.run([sys.executable, "-B", str(GUARD)], input=json.dumps(payload),
                                        capture_output=True, text=True, check=False)
                self.assertEqual(result.returncode, code, result.stderr)
            payload["tool_input"]["command"] = f"cd / && git -C {folder} push -u origin HEAD"
            result = subprocess.run([sys.executable, "-B", str(GUARD)], input=json.dumps(payload),
                                    capture_output=True, text=True, check=False)
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
                result = subprocess.run([sys.executable, "-B", str(GUARD)], input=json.dumps(payload),
                                        capture_output=True, text=True, check=False)
                self.assertEqual(result.returncode, 2, command)
                self.assertIn(reason, result.stderr)
            subprocess.run(["git", "-C", folder, "config", "push.followTags", "true"], check=True)
            for command, code in (("git push origin HEAD:topic", 2), ("git push --no-follow-tags origin HEAD:topic", 0),
                                  ("git -c push.followTags=false push origin HEAD:topic", 0)):
                payload["tool_input"]["command"] = command
                result = subprocess.run([sys.executable, "-B", str(GUARD)], input=json.dumps(payload),
                                        capture_output=True, text=True, check=False)
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
                result = subprocess.run([sys.executable, "-B", str(GUARD)], input=json.dumps(payload),
                                        capture_output=True, text=True, check=False)
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
                result = subprocess.run([sys.executable, "-B", str(GUARD)], input=json.dumps(payload),
                                        capture_output=True, text=True, check=False)
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
                result = subprocess.run([sys.executable, "-B", str(GUARD)], input=json.dumps(payload),
                                        capture_output=True, text=True, check=False)
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
            result = subprocess.run([sys.executable, "-B", str(GUARD)], input=json.dumps(payload),
                                    capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, 2, result.stderr)
            payload["tool_input"]["command"] = "gh api graphql -F query=@reply.graphql"
            result = subprocess.run([sys.executable, "-B", str(GUARD)], input=json.dumps(payload),
                                    capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, 0, result.stderr)
            payload["tool_input"]["command"] = ("printf '%s' 'mutation { mergePullRequest(input: {}) { clientMutationId } }'"
                                                " > reply.graphql; gh api graphql -F query=@reply.graphql")
            result = subprocess.run([sys.executable, "-B", str(GUARD)], input=json.dumps(payload),
                                    capture_output=True, text=True, check=False)
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
                result = subprocess.run([sys.executable, "-B", str(GUARD)], input=json.dumps(payload),
                                        capture_output=True, text=True, check=False)
                self.assertEqual(result.returncode, 2, command)
                self.assertIn(reason, result.stderr)
            payload.update(cwd=str(Path(folder, "sub")))
            payload["tool_input"]["command"] = "gh api graphql --input body.json"
            result = subprocess.run([sys.executable, "-B", str(GUARD)], input=json.dumps(payload),
                                    capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, 2, result.stderr)
            self.assertIn("maintainer", result.stderr)

    def test_gh_api_variants(self):
        with tempfile.TemporaryDirectory() as folder:
            body = json.dumps({"query": "mutation { mergePullRequest\n(input: {}) { clientMutationId } }"})
            Path(folder, "body.json").write_text(body)
            payload = {"tool_name": "Execute", "cwd": folder,
                       "tool_input": {"command": "gh api graphql --input body.json"}}
            result = subprocess.run([sys.executable, "-B", str(GUARD)], input=json.dumps(payload),
                                    capture_output=True, text=True, check=False)
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
                        "gh variable -h", "gh -R o/r variable list --json name,value", "gh help variable",
                        f"gh api repos/o/r/actions/variables/{name}", "gh api repos/o/r/actions/variables",
                        f"gh api -X GET orgs/o/actions/variables/{name}"):
            self.assertAllowed("Execute", {"command": command})

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

    def test_unreadable_input_fails_closed(self):
        for payload in ("not json", "[]", "null", '{"tool_name":"Execute","tool_input":[]}'):
            result = subprocess.run([sys.executable, "-B", str(GUARD)], input=payload,
                                    capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, 2, payload)


CLI = ROOT / "scripts/agent-guard.py"


def neutral(*args, stdin=""):
    return subprocess.run([sys.executable, "-B", str(CLI), *args], input=stdin,
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
            self.assertPasses(subprocess.run([sys.executable, "-B", str(CLI), "command", "--cwd", "link/../repo", push],
                                             cwd=folder, capture_output=True, text=True, check=False))
            self.assertPasses(request({"operation": "command", "cwd": cwd, "command": push}))
            self.assertPasses(hook(GUARD, droid_command(push, cwd)))
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


class FactoryAdapterTests(unittest.TestCase):
    """The Factory hook must decide exactly as the neutral CLI does for the same operation."""

    COMMANDS = ("git push -u origin HEAD:chore/topic", "git push origin HEAD:main", "git push --force origin topic",
                "git push --tags", "git commit --no-verify -m x", "gh pr merge 12", "gh release create v1",
                "gh api repos/o/r/pulls/12/reviews -f event=APPROVE", "gh api repos/o/r/pulls/12/comments/34/replies -f body=ok",
                "git config core.hooksPath /tmp/x", "GIT_CONFIG_COUNT=1 git commit -m x", "git push origin topic",
                "cd sub && gh api graphql -F query=@merge.graphql", "printf '%s' 'git push --force'",
                "python3 - <<'PY'\nprint(1)\nPY\ngit push origin main", "git push origin 'main", "env -S 'git push'",
                "npm --prefix web ci", "gh variable set PP_REVIEW_FALLBACK_UNTIL -b x",
                "gh variable get PP_REVIEW_FALLBACK_UNTIL", "")
    PATHS = ("/repo/.env", "/repo/.env.example", "/repo/.git/config", "/repo/rust/bundle-manifest.json", "/repo/a.py")
    PATCHES = ("*** Update File: /repo/a.py\n*** Move to: /repo/.env\n", "*** Add File: /repo/src/lib.rs\n",
               "*** Delete File: /repo/rust//bundle-manifest.json\n")

    def assertSame(self, hook, cli):
        self.assertEqual((hook.returncode, hook.stderr), (cli.returncode, cli.stderr))

    def test_commands_match(self):
        with tempfile.TemporaryDirectory() as folder:
            Path(folder, "sub").mkdir()
            Path(folder, "merge.graphql").write_text("mutation { mergePullRequest(input: {}) { clientMutationId } }")
            for command in self.COMMANDS:
                with self.subTest(command=command):
                    hook = subprocess.run([sys.executable, "-B", str(GUARD)], capture_output=True, text=True, check=False,
                                          input=json.dumps({"tool_name": "Execute", "cwd": folder,
                                                            "tool_input": {"command": command}}))
                    self.assertSame(hook, request({"operation": "command", "cwd": folder, "command": command}))
                    self.assertSame(hook, neutral("command", "--cwd", folder, "-", stdin=command))

    def test_files_and_patches_match(self):
        for path in self.PATHS:
            for tool in ("Create", "Edit"):
                with self.subTest(tool=tool, path=path):
                    self.assertSame(run(tool, {"file_path": path}), request({"operation": "file", "path": path}))
        for patch in self.PATCHES:
            with self.subTest(patch=patch):
                self.assertSame(run("ApplyPatch", {"input": patch}), request({"operation": "patch", "patch": patch}))
                self.assertSame(run("ApplyPatch", patch), neutral("patch", "-", stdin=patch))

    def test_adapter_refuses_when_policy_is_missing(self):
        with tempfile.TemporaryDirectory() as folder:
            copy = Path(folder, ".factory/hooks/guard.py")
            copy.parent.mkdir(parents=True)
            copy.write_text(GUARD.read_text())
            result = subprocess.run([sys.executable, "-B", str(copy)], capture_output=True, text=True, check=False,
                                    input=json.dumps({"tool_name": "Edit", "tool_input": {"file_path": "/repo/a.py"}}))
            self.assertEqual(result.returncode, 2, result.stderr)
            self.assertIn("internal error", result.stderr)


CLAUDE = ROOT / ".claude/hooks/guard.py"
SHA = "0123456789abcdef0123456789abcdef01234567"
MERGE_FORM = f"gh pr merge 12 --squash --match-head-commit {SHA}"
# The command the retired lane-rules.yaml merge_gate told lanes to run.
OLD_MERGE_GATE = f"gh pr merge 12 -R poitee/PrintPartner --squash --match-head-commit {SHA}"
CLAUDE_TOOLS = {"Bash", "Write", "Edit", "MultiEdit", "NotebookEdit"}


def hook(adapter, payload, coordinator=None):
    env = {key: value for key, value in os.environ.items() if key != "PP_COORDINATOR_WORKTREE"}
    if coordinator is not None:
        env["PP_COORDINATOR_WORKTREE"] = coordinator
    text = payload if isinstance(payload, str) else json.dumps(payload)
    return subprocess.run([sys.executable, "-B", str(adapter)], input=text, env=env,
                          capture_output=True, text=True, check=False)


def droid_command(command, cwd):
    return {"hook_event_name": "PreToolUse", "tool_name": "Execute", "cwd": cwd, "tool_input": {"command": command}}


def claude_call(tool, cwd="/", **data):
    return {"session_id": "s", "hook_event_name": "PreToolUse", "tool_name": tool, "cwd": cwd, "tool_input": data}


def load(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def workflow_triggers(text):
    """Return the trigger names under a workflow's top-level on: key, read without a YAML library."""
    lines = [line.split(" #")[0].rstrip() for line in text.splitlines() if not line.lstrip().startswith("#")]
    for i, line in enumerate(lines):
        match = re.match(r"""^["']?on["']?:\s*(.*)$""", line)
        if not match:
            continue
        if match.group(1):
            return set(re.findall(r"[A-Za-z_]+", match.group(1)))
        found, indent = set(), None
        for nested in lines[i + 1:]:
            if not nested.strip():
                continue
            if not nested[0].isspace():
                break
            depth = len(nested) - len(nested.lstrip())
            indent = depth if indent is None else indent
            if depth == indent:
                found.add(nested.strip().lstrip("- ").split(":")[0])
        return found
    return set()


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
            for command in filter(None, FactoryAdapterTests.COMMANDS):
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
            with self.subTest(tool="Droid Edit", path=path):
                self.assertRefused(hook(GUARD, {"tool_name": "Edit", "tool_input": {"file_path": path}}), reason)
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

    def results(self, command, cwd, coordinator):
        return (hook(GUARD, droid_command(command, cwd), coordinator),
                hook(CLAUDE, claude_call("Bash", cwd, command=command), coordinator))

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

    def test_other_comments_and_marker_reads_stay_allowed(self):
        for command in ("gh pr comment 12 -b 'Fixed in abc123.'", "gh pr comment 12 --body-file plain.md",
                        "gh issue comment 12 -F plain.md", "gh pr comment 12 -b 'The pp-review-gate check failed.'",
                        "gh api repos/o/r/issues/12/comments -f body='The review-gate check is red.'",
                        "gh api repos/o/r/issues/12/comments --input plain.md",
                        "gh api repos/o/r/issues/12/comments --jq '.[] | select(.body | test(\"<!-- pp-review-gate\"))'",
                        "gh pr view 12 --comments", "gh pr create --body-file marker.md",
                        "gh api graphql -f query='mutation($id: ID!) { resolveReviewThread(input: {threadId: $id}) "
                        "{ thread { isResolved } } }' -f id=\"$THREAD\""):
            for result in self.results(command, self.lane, None):
                with self.subTest(command=command):
                    self.assertEqual(result.returncode, 0, result.stderr)


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
                    self.assertIn("read a comment body literally", result.stderr)

    def test_coordinator_worktree_may_send_any_body(self):
        for command in self.UNREAD:
            for result in self.results(command, self.coordinator, self.coordinator):
                with self.subTest(command=command):
                    self.assertEqual(result.returncode, 0, result.stderr)


class RetiredFleetIntakeTests(unittest.TestCase):
    def test_lane_rules_no_longer_drive_intake_or_merges(self):
        text = (ROOT / "scripts/fleet/lane-rules.yaml").read_text(encoding="utf-8")
        live = "\n".join(line.split(" #")[0] for line in text.splitlines() if not line.lstrip().startswith("#"))
        for key in ("picker", "claim", "linear_access", "merge_gate", "close_out", "end_of_wake", "author_overflow",
                    "scorer_deadline", "seat_rule", "scorer_policy", "complex_seat", "classes", "pick_rule", "wake"):
            with self.subTest(key=key):
                self.assertNotRegex(live, rf"(?m)^\s*{key}\s*:")
        self.assertEqual(sorted(path.name for path in (ROOT / "scripts/fleet").iterdir()), ["lane-rules.yaml"])

    def test_lane_dispatch_stays_disabled(self):
        text = (ROOT / ".github/workflows/lane-dispatch.yml").read_text(encoding="utf-8")
        self.assertEqual(workflow_triggers(text), {"workflow_dispatch"})
        self.assertIn("disabled until the owner re-approves it", text)
        self.assertRegex(text, r"(?m)^\s+exit 1$")
        self.assertNotIn("secrets.", text)
        # Re-enabling the push or schedule trigger, in block or inline form, must fail the first check.
        for variant in (text.replace("on:\n", "on:\n  push:\n    branches: [main]\n"),
                        text.replace("on:\n", 'on:\n  schedule:\n    - cron: "*/10 * * * *"\n'),
                        text.replace("on:\n  workflow_dispatch:\n", "on: [push, workflow_dispatch]\n"),
                        text.replace("on:\n  workflow_dispatch:\n", '"on":\n  workflow_dispatch:\n  schedule:\n'
                                                                   '    - cron: "0 * * * *"\n')):
            with self.subTest(variant=variant):
                self.assertNotEqual(variant, text)
                self.assertNotEqual(workflow_triggers(variant), {"workflow_dispatch"})


if __name__ == "__main__":
    unittest.main()
