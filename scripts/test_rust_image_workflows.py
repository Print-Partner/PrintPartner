"""The Rust-only image's arm64 CI job and its release jobs (GRE-1295).

    python3 -B -m unittest discover -s scripts -p test_rust_image_workflows.py

The release jobs run only on a version tag, with the release key, so a pull
request cannot run them. These tests pin what they promise instead: each
architecture is built, smoke-tested and pushed on its own native runner, the
pushed image carries SBOM and provenance attestations, and the publish job
refuses a candidate without them. The publish job's jq check runs here against
image indexes shaped like the ones buildx 0.38 writes.
"""

import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import unittest

import yaml


ROOT = Path(__file__).resolve().parents[1]
WORKFLOWS = ROOT / ".github/workflows"
NATIVE_RUNNERS = {"amd64": "ubuntu-latest", "arm64": "ubuntu-24.04-arm"}
SMOKE = "python3 scripts/docker-rust-smoke.py"


def load(name):
    return yaml.load((WORKFLOWS / name).read_text(), Loader=yaml.BaseLoader)


def step_index(steps, predicate, what):
    matches = [i for i, step in enumerate(steps) if predicate(step)]
    if len(matches) != 1:
        raise AssertionError(f"expected one step that {what}, found {len(matches)}")
    return matches[0]


def builds(step):
    return step.get("uses", "").startswith("docker/build-push-action@")


class PullRequestImageTest(unittest.TestCase):
    def test_each_architecture_builds_and_smoke_tests_on_its_own_runner(self):
        jobs = load("rust-image.yml")["jobs"]
        self.assertIn("changes", jobs)
        image_jobs = {k: v for k, v in jobs.items() if k != "changes"}
        by_runner = {job["runs-on"]: job for job in image_jobs.values()}
        self.assertEqual(set(by_runner), set(NATIVE_RUNNERS.values()))
        # The amd64 check keeps its name and cache, so the existing check and warm cache stay.
        self.assertEqual(by_runner["ubuntu-latest"]["name"], "rust image")
        self.assertEqual(by_runner["ubuntu-24.04-arm"]["name"], "rust image (arm64)")
        draft_guard = "github.event_name != 'pull_request' || !github.event.pull_request.draft"
        self.assertEqual(jobs["changes"]["if"], draft_guard)
        for arch, runner in NATIVE_RUNNERS.items():
            with self.subTest(arch=arch):
                job = by_runner[runner]
                self.assertEqual(job["needs"], ["changes"])
                self.assertTrue(job["if"].startswith(f"({draft_guard}) && ("))
                self.assertIn("needs.changes.outputs.relevant != 'false'", job["if"])
                steps = job["steps"]
                build = step_index(steps, builds, "builds the image")
                smoke = step_index(steps, lambda s: s.get("run", "").startswith(SMOKE), "runs the smoke test")
                self.assertLess(build, smoke)
                options = steps[build]["with"]
                self.assertEqual(options["file"], "Dockerfile.rust")
                self.assertEqual(options["load"], "true")
                self.assertEqual(options["push"], "false")
                self.assertEqual(options.get("platforms", "linux/amd64"), f"linux/{arch}")
        arm = by_runner["ubuntu-24.04-arm"]["steps"]
        arch_check = step_index(arm, lambda s: "{{.Os}}/{{.Architecture}}" in s.get("run", ""),
                                "checks the loaded image's architecture")
        self.assertIn("= linux/arm64", arm[arch_check]["run"])
        self.assertEqual(arm[step_index(arm, builds, "builds")]["with"]["cache-from"],
                         "type=gha,scope=dockerfile-rust-arm64")


class ReleaseImageTest(unittest.TestCase):
    def setUp(self):
        self.jobs = load("release.yml")["jobs"]

    def test_the_node_release_does_not_wait_on_the_rust_image(self):
        self.assertEqual(self.jobs["release"]["needs"], ["quality", "manifests"])
        self.assertEqual(self.jobs["rust-image"]["needs"], "release")
        self.assertEqual(self.jobs["rust-image-publish"]["needs"], ["release", "rust-image"])

    def test_each_architecture_is_smoke_tested_before_its_attested_push(self):
        job = self.jobs["rust-image"]
        matrix = {entry["arch"]: entry["runner"] for entry in job["strategy"]["matrix"]["include"]}
        self.assertEqual(matrix, NATIVE_RUNNERS)
        self.assertEqual(job["runs-on"], "${{ matrix.runner }}")
        steps = job["steps"]
        load_build = step_index(steps, lambda s: builds(s) and s["with"].get("load") == "true", "loads")
        smoke = step_index(steps, lambda s: SMOKE in s.get("run", ""), "runs the smoke test")
        push = step_index(steps, lambda s: builds(s) and "push-by-digest=true" in s["with"].get("outputs", ""),
                          "pushes by digest")
        tag = step_index(steps, lambda s: '--tag "$IMAGE:commit-$COMMIT-$ARCH"' in s.get("run", ""),
                         "tags the architecture candidate")
        self.assertLess(load_build, smoke)
        self.assertLess(smoke, push)
        self.assertLess(push, tag)
        # The tag is the proof a rerun trusts, so the push writes none and the
        # tag follows the layer comparison in the same step.
        self.assertNotIn("tags", steps[push]["with"])
        tag_run = steps[tag]["run"]
        self.assertLess(tag_run.index('test "$pushed" = "$tested"'), tag_run.index("imagetools create"))
        self.assertIn("${{ steps.push.outputs.digest }}", steps[tag]["env"]["DIGEST"])
        pushed = steps[push]["with"]
        self.assertEqual(pushed["sbom"], "true")
        self.assertEqual(pushed["provenance"], "mode=max")
        self.assertEqual(pushed["build-args"], steps[load_build]["with"]["build-args"])
        self.assertEqual(pushed["platforms"], steps[load_build]["with"]["platforms"])
        for index in (load_build, push):
            self.assertNotIn("cache-from", steps[index]["with"])
            self.assertNotIn("cache-to", steps[index]["with"])

    def test_publish_verifies_attestations_before_signing_and_never_moves_latest(self):
        steps = self.jobs["rust-image-publish"]["steps"]
        verify = step_index(steps, lambda s: s.get("id") == "image", "verifies the candidate")
        sign = step_index(steps, lambda s: "cosign sign " in s.get("run", ""), "signs")
        alias = step_index(steps, lambda s: '--tag "$IMAGE:$VERSION"' in s.get("run", ""), "creates the version alias")
        self.assertLess(verify, sign)
        self.assertLess(sign, alias)
        for arch in NATIVE_RUNNERS:
            self.assertIn(f'"$IMAGE:commit-$COMMIT-{arch}"', "".join(s.get("run", "") for s in steps))
        self.assertIn(".SPDX.spdxVersion", steps[verify]["run"])
        self.assertIn(".SLSA.buildDefinition", steps[verify]["run"])
        self.assertNotIn(":latest", json.dumps(self.jobs["rust-image-publish"]))

    def test_existence_checks_fail_closed(self):
        runs = [step.get("run", "") for job in ("rust-image", "rust-image-publish")
                for step in self.jobs[job]["steps"]]
        self.assertFalse([run for run in runs if ">/dev/null 2>&1" in run],
                         "an inspect that hides its error treats a registry failure as a missing tag")
        checks = [run for run in runs if "scripts/registry-tag-state.sh" in run]
        self.assertEqual(len(checks), 3)
        self.assertNotIn(":latest", json.dumps(self.jobs["rust-image"]))


class RegistryTagStateTest(unittest.TestCase):
    """scripts/registry-tag-state.sh against a fake docker on PATH."""

    REF = "ghcr.io/print-partner/pp-server:1.2.3"

    def state(self, exit_code, stderr):
        with tempfile.TemporaryDirectory() as bin_dir:
            fake = Path(bin_dir) / "docker"
            fake.write_text('#!/bin/sh\nprintf "%s" "$FAKE_STDERR" >&2\nexit "$FAKE_EXIT"\n')
            fake.chmod(0o755)
            env = {**os.environ, "PATH": f"{bin_dir}:{os.environ['PATH']}",
                   "FAKE_EXIT": str(exit_code), "FAKE_STDERR": stderr}
            result = subprocess.run(["bash", str(ROOT / "scripts/registry-tag-state.sh"), self.REF],
                                    env=env, capture_output=True, text=True)
        return result.returncode, result.stdout.strip()

    def test_present(self):
        self.assertEqual(self.state(0, ""), (0, "present"))

    def test_absent_only_on_buildx_not_found_for_that_reference(self):
        self.assertEqual(self.state(1, f"ERROR: {self.REF}: not found\n"), (0, "absent"))

    def test_registry_errors_fail_closed(self):
        for stderr in ('ERROR: failed to do request: Head "https://ghcr.io/v2/x": dial tcp: i/o timeout\n',
                       "ERROR: denied: requested access to the resource is denied\n",
                       "ERROR: ghcr.io/print-partner/pp-server:9.9.9: not found\n",
                       ""):
            with self.subTest(stderr=stderr):
                self.assertEqual(self.state(1, stderr), (1, ""))


@unittest.skipUnless(shutil.which("jq"), "jq is not installed")
class CandidateIndexCheckTest(unittest.TestCase):
    """Run the publish job's jq program on image indexes."""

    COMMIT = "1" * 40

    @classmethod
    def setUpClass(cls):
        run = next(step["run"] for step in load("release.yml")["jobs"]["rust-image-publish"]["steps"]
                   if step.get("id") == "image")
        cls.program = re.search(r"'(\.annotations\[.*?)'", run, re.DOTALL)[1]

    def index(self, arches=("amd64", "arm64"), attested=("amd64", "arm64")):
        manifests = []
        for n, arch in enumerate(arches):
            digest = f"sha256:{n + 1:064x}"
            manifests.append({"digest": digest, "platform": {"os": "linux", "architecture": arch}})
            if arch in attested:
                manifests.append({
                    "digest": f"sha256:{n + 101:064x}",
                    "platform": {"os": "unknown", "architecture": "unknown"},
                    "annotations": {"vnd.docker.reference.type": "attestation-manifest",
                                    "vnd.docker.reference.digest": digest},
                })
        return {"annotations": {"org.opencontainers.image.version": "9.9.9",
                                "org.opencontainers.image.revision": self.COMMIT,
                                "org.opencontainers.image.ref.name": "v9.9.9"},
                "manifests": manifests}

    def check(self, index):
        result = subprocess.run(
            ["jq", "-e", "--arg", "version", "9.9.9", "--arg", "commit", self.COMMIT,
             "--arg", "tag", "v9.9.9", self.program],
            input=json.dumps(index), capture_output=True, text=True)
        return result.returncode

    def test_accepts_two_architectures_each_with_an_attestation(self):
        self.assertEqual(self.check(self.index()), 0)

    def test_refuses_a_missing_architecture(self):
        self.assertEqual(self.check(self.index(arches=("amd64",), attested=("amd64",))), 1)

    def test_refuses_an_architecture_without_attestations(self):
        self.assertEqual(self.check(self.index(attested=("amd64",))), 1)
        self.assertEqual(self.check(self.index(attested=())), 1)

    def test_refuses_wrong_release_annotations(self):
        index = self.index()
        index["annotations"]["org.opencontainers.image.revision"] = "2" * 40
        self.assertEqual(self.check(index), 1)


if __name__ == "__main__":
    unittest.main()
