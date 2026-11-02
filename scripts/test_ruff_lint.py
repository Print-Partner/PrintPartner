"""Negative controls for the repository Ruff lint rules in ruff.toml."""

from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
RUFF = shutil.which("ruff")


def function_with_complexity(complexity):
    branches = "".join(f"    if value == {n}:\n        return {n}\n" for n in range(complexity - 1))
    return f"def pick(value):\n{branches}    return -1\n"


@unittest.skipUnless(RUFF, "ruff is not on PATH; install requirements-dev.txt")
class RuffLintTest(unittest.TestCase):
    def check(self, source):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "sample.py"
            path.write_text(source)
            result = subprocess.run(
                [RUFF, "check", "--no-cache", "--config", str(ROOT / "ruff.toml"), str(path)],
                capture_output=True,
                text=True,
            )
        return result.returncode, result.stdout + result.stderr

    def test_an_unused_import_fails(self):
        code, output = self.check("import os\n")
        self.assertEqual(code, 1, output)
        self.assertIn("F401", output)

    def test_a_used_import_passes(self):
        code, output = self.check("import os\n\nprint(os.sep)\n")
        self.assertEqual(code, 0, output)

    def test_complexity_above_the_eslint_limit_fails(self):
        code, output = self.check(function_with_complexity(21))
        self.assertEqual(code, 1, output)
        self.assertIn("C901", output)
        self.assertIn("(21 > 20)", output)

    def test_complexity_at_the_limit_passes(self):
        code, output = self.check(function_with_complexity(20))
        self.assertEqual(code, 0, output)

    def test_a_line_exemption_covers_only_its_function(self):
        exempt = function_with_complexity(21).replace(
            "def pick(value):", "def pick(value):  # noqa: C901"
        )
        code, output = self.check(exempt)
        self.assertEqual(code, 0, output)
        code, output = self.check(exempt + "\n\n" + function_with_complexity(21).replace("pick", "other"))
        self.assertEqual(code, 1, output)
        self.assertIn("`other` is too complex", output)


if __name__ == "__main__":
    unittest.main()
