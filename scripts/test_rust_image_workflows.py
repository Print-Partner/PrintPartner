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
from pathlib import Path
import re
import shutil
import subprocess
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
        by_runner = {job["runs-on"]: job for job in jobs.values()}
        self.assertEqual(set(by_runner), set(NATIVE_RUNNERS.values()))
        # The amd64 check keeps its name and cache, so the existing check and warm cache stay.
        self.assertEqual(by_runner["ubuntu-latest"]["name"], "rust image")
        self.assertEqual(by_runner["ubuntu-24.04-arm"]["name"], "rust image (arm64)")
        draft_guard = "github.event_name != 'pull_request' || !github.event.pull_request.draft"
        for arch, runner in NATIVE_RUNNERS.items():
            with self.subTest(arch=arch):
                job = by_runner[runner]
                self.assertEqual(job["if"], draft_guard)
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
        push = step_index(steps, lambda s: builds(s) and s["with"].get("push") == "true", "pushes")
        same = step_index(steps, lambda s: ".rootfs.diff_ids" in s.get("run", ""),
                          "compares the pushed layers with the tested ones")
        self.assertLess(load_build, smoke)
        self.assertLess(smoke, push)
        self.assertLess(push, same)
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
        self.assertNotIn(":latest", json.dumps(self.jobs["rust-image"]))


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
