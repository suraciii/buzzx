"""Tests for the documentation gate.

Each case runs the gate against a synthetic document tree and asserts the
exact rule that fires. A gate that fires on nothing is worse than no gate: it
signals coverage that does not exist.

Run: python3 -m unittest discover -s tests -p '*_test.py' -v
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
    "check-docs.py",
)
ROOT_DOCUMENTS = ("README.md", "CONTEXT.md", "CONTRIBUTING.md")


class GateCase(unittest.TestCase):
    def setUp(self):
        self.root = tempfile.mkdtemp(prefix="buzzx-docs-gate-")
        for name in ("docs", "design", "eng"):
            os.makedirs(os.path.join(self.root, name), exist_ok=True)
        for name in ROOT_DOCUMENTS:
            self.write(name, "Root document placeholder.\n")
        self.addCleanup(shutil.rmtree, self.root, True)

    def write(self, relative: str, content: str) -> str:
        path = os.path.join(self.root, relative.replace("/", os.sep))
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w", encoding="utf-8") as handle:
            handle.write(content)
        return path

    def run_gate(self) -> tuple[int, str]:
        result = subprocess.run(
            [sys.executable, GATE, "--root", self.root],
            capture_output=True,
            text=True,
            check=False,
        )
        return result.returncode, result.stdout + result.stderr

    def rules(self) -> list[str]:
        _code, output = self.run_gate()
        found = []
        for line in output.splitlines():
            for rule in (
                "latin-script-prose-only",
                "raw-html-not-allowed",
                "ascii-diagram-only",
                "text-fence-kind-required",
                "markdown-link-reference-exists",
                "relative-link-target-exists",
                "markdown-heading-fragment-exists",
                "required-document-exists",
                "documentation-root-exists",
                "decision-record-status",
                "decision-record-alternatives",
            ):
                if f" {rule}:" in line:
                    found.append(rule)
        return found

    def test_clean_tree_passes(self):
        self.write("docs/manual.md", "# Manual\n\nThe operator sends a message.\n")
        code, output = self.run_gate()
        self.assertEqual(code, 0, output)

    def test_non_latin_prose_is_rejected(self):
        # The offending character is an actual CJK ideograph, written as a
        # Python escape so this file stays ASCII.
        body = "# Manual\n\nA CJK ideograph is not allowed: \u4e2d\n"
        self.write("docs/manual.md", body)
        self.assertEqual(self.rules(), ["latin-script-prose-only"])

    def test_greek_prose_is_rejected(self):
        body = "# Manual\n\nA Greek letter is not allowed: \u03b1\n"
        self.write("docs/manual.md", body)
        self.assertEqual(self.rules(), ["latin-script-prose-only"])

    def test_accented_latin_script_is_accepted(self):
        # Accented Latin letters carry the Unicode name LATIN and are allowed.
        body = "# Manual\n\nA caf\u00e9 with a r\u00e9sum\u00e9 file is fine.\n"
        self.write("docs/manual.md", body)
        code, output = self.run_gate()
        self.assertEqual(code, 0, output)

    def test_raw_html_is_rejected(self):
        self.write("docs/manual.md", "# Manual\n\n<div>raw</div>\n")
        self.assertEqual(self.rules(), ["raw-html-not-allowed"])

    def test_mermaid_fence_is_rejected(self):
        self.write("design/module.md", "# Module\n\n```mermaid\nflowchart TD\nA-->B\n```\n")
        self.assertEqual(self.rules(), ["ascii-diagram-only"])

    def test_bare_text_fence_is_rejected(self):
        self.write("design/module.md", "# Module\n\n```\nstruct Row;\n```\n")
        self.assertEqual(self.rules(), ["text-fence-kind-required"])

    def test_labelled_text_fence_is_accepted(self):
        self.write("design/module.md", "# Module\n\n```text literal\nstruct Row;\n```\n")
        code, output = self.run_gate()
        self.assertEqual(code, 0, output)

    def test_non_ascii_diagram_is_rejected(self):
        # A Unicode arrow inside an ASCII-art fence is a violation.
        body = "# Module\n\n```text diagram\nUI \u2192 relay\n```\n"
        self.write("design/module.md", body)
        self.assertEqual(self.rules(), ["ascii-diagram-only"])

    def test_broken_relative_link_is_rejected(self):
        self.write("docs/manual.md", "# Manual\n\nSee [missing](missing.md).\n")
        self.assertEqual(self.rules(), ["relative-link-target-exists"])

    def test_relative_link_outside_repository_is_rejected(self):
        self.write("docs/manual.md", "# Manual\n\nSee [outside](../outside.md).\n")
        self.assertEqual(self.rules(), ["relative-link-target-exists"])

    def test_undefined_reference_link_is_rejected(self):
        self.write("docs/manual.md", "# Manual\n\nSee [text][missing].\n")
        self.assertEqual(self.rules(), ["markdown-link-reference-exists"])

    def test_defined_reference_link_is_accepted(self):
        self.write("docs/manual.md", "# Manual\n\nSee [text][ref].\n\n[ref]: other.md\n")
        self.write("docs/other.md", "Other\n")
        code, output = self.run_gate()
        self.assertEqual(code, 0, output)

    def test_missing_heading_fragment_is_rejected(self):
        self.write("docs/manual.md", "# Manual\n\nSee [other](other.md#nope).\n")
        self.write("docs/other.md", "# Other\n\n## Real heading\n")
        self.assertEqual(self.rules(), ["markdown-heading-fragment-exists"])

    def test_present_heading_fragment_is_accepted(self):
        self.write("docs/manual.md", "# Manual\n\nSee [other](other.md#real-heading).\n")
        self.write("docs/other.md", "# Other\n\n## Real heading\n")
        code, output = self.run_gate()
        self.assertEqual(code, 0, output)

    def test_missing_root_document_is_rejected(self):
        os.remove(os.path.join(self.root, "CONTEXT.md"))
        self.assertEqual(self.rules(), ["required-document-exists"])

    def test_missing_documentation_directory_is_rejected(self):
        shutil.rmtree(os.path.join(self.root, "design"))
        self.assertEqual(self.rules(), ["documentation-root-exists"])

    def test_decision_record_without_status_is_rejected(self):
        self.write("design/decisions/0001-thing.md", "# Decision\n\n## Context\n\nx\n")
        self.assertEqual(sorted(self.rules()), [
            "decision-record-alternatives",
            "decision-record-status",
        ])

    def test_decision_record_with_status_and_alternatives_is_accepted(self):
        self.write(
            "design/decisions/0001-thing.md",
            "# Decision\n\n## Status\n\naccepted\n\n## Context\n\nx\n\n"
            "## Alternatives considered\n\ny\n",
        )
        code, output = self.run_gate()
        self.assertEqual(code, 0, output)

    def test_code_fence_content_is_exempt_from_prose_rules(self):
        self.write("docs/manual.md", "# Manual\n\n```sh\n# CJK in a fence is text, not prose\necho hi\n```\n")
        code, output = self.run_gate()
        self.assertEqual(code, 0, output)


if __name__ == "__main__":
    unittest.main()
