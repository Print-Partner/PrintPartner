import contextlib
import importlib.util
import io
from pathlib import Path
import unittest

SCRIPT = Path(__file__).with_name("maintainability-report.py")
SPEC = importlib.util.spec_from_file_location("maintainability_report", SCRIPT)
REPORT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(REPORT)

GATE = " " * 20 + '#[cfg(feature = "desktop_rebuild")]'
ARM = " " * 20


class EnvelopeArmTests(unittest.TestCase):
    def test_body_lines_exclude_the_braces_of_a_braced_arm(self):
        block = "\n".join([
            GATE,
            ARM + "Envelope::OneLine { command, reply } => reply.send(one(command)),",
            GATE,
            ARM + "Envelope::OneStatement { command, reply } => {",
            ARM + "    let _ = reply.send(one(command));",
            ARM + "}",
            GATE,
            ARM + "Envelope::Wrapped { command, reply } => {",
            ARM + "    let _ = reply.send(one(",
            ARM + "        command,",
            ARM + "    ));",
            ARM + "}",
            ARM + "Envelope::Ungated { command } => {",
            ARM + "    first(command);",
            ARM + "    second();",
            ARM + "}",
        ])
        self.assertEqual(REPORT.envelope_arms(block), [
            ("OneLine", 1, True),
            ("OneStatement", 1, True),
            ("Wrapped", 3, True),
            ("Ungated", 2, False),
        ])

    def test_rows_escape_pipes_and_keep_three_cells(self):
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            REPORT.row("a | b", "1 | 2", "grep x | wc -l")
        line = output.getvalue().strip()
        self.assertEqual(line, "| a \\| b | 1 \\| 2 | `grep x \\| wc -l` |")
        self.assertEqual(len(REPORT.UNESCAPED_PIPE.findall(line)), 4)


if __name__ == "__main__":
    unittest.main()
