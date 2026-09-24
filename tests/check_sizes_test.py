"""Tests for the documentation file size gate.

The gate is a ceiling, not a ratchet: any active Markdown file over 1000 lines
fails. Run it: python3 -m unittest discover -s tests -p '*_test.py'
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tempfile
import unittest

GATE = os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
    "scripts",
    "check-file-sizes.py",
)


class SizeGateCase(unittest.TestCase):
    def setUp(self):
        self.root = tempfile.mkdtemp(prefix="buzzx-size-gate-")
        self.addCleanup(shutil.rmtree, self.root, True)
        for name in ("docs", "design", "eng"):
            os.makedirs(os.path.join(self.root, name), exist_ok=True)

    def write(self, relative: str, total_lines: int) -> None:
        path = os.path.join(self.root, relative.replace("/", os.sep))
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w", encoding="utf-8") as handle:
            for index in range(total_lines):
                handle.write("line %d\n" % index)

    def run_gate(self) -> tuple[int, str]:
        result = subprocess.run(
            [sys.executable, GATE, "--root", self.root],
            capture_output=True,
            text=True,
            check=False,
        )
        return result.returncode, result.stdout + result.stderr

    def test_document_at_ceiling_passes(self):
        self.write("docs/manual.md", 1000)
        code, output = self.run_gate()
        self.assertEqual(code, 0, output)

    def test_document_over_ceiling_fails(self):
        self.write("docs/manual.md", 1001)
        code, output = self.run_gate()
        self.assertEqual(code, 1)
        self.assertIn("docs/manual.md", output)

    def test_final_line_without_newline_counts(self):
        path = os.path.join(self.root, "docs", "no-newline.md")
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w", encoding="utf-8") as handle:
            for index in range(1000):
                handle.write("line %d\n" % index)
            handle.write("final line without newline")
        code, output = self.run_gate()
        self.assertEqual(code, 1)
        self.assertIn("no-newline.md", output)

    def test_empty_document_counts_zero(self):
        self.write("docs/empty.md", 0)
        code, output = self.run_gate()
        self.assertEqual(code, 0, output)


if __name__ == "__main__":
    unittest.main()
