#!/usr/bin/env python3
"""Documentation file size gate.

Checks that every active Markdown document stays at or below 1000 lines, so
that a file remains readable in one pass. A file that trips the ceiling must be
split. Never raise the limit.

Exit code 0 means every file is within the ceiling. Exit code 1 means
violations exist.
"""

from __future__ import annotations

import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ACTIVE_DOCUMENT_DIRECTORIES = ("docs", "design", "eng")
ACTIVE_ROOT_DOCUMENTS = ("core.md", "AGENTS.md", "CONTEXT.md", "CONTRIBUTING.md", "README.md")
LIMIT = 1000


def resolve_root(argv: list[str]) -> None:
    """`--root <dir>` overrides the repository root, so a test can run the gate
    against a synthetic tree. The default is the repository root."""
    global ROOT
    for index, argument in enumerate(argv):
        if argument == "--root" and index + 1 < len(argv):
            ROOT = os.path.abspath(argv[index + 1])
            return
    ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def collect_active_documents() -> list[str]:
    files: list[str] = []
    for name in ACTIVE_ROOT_DOCUMENTS:
        path = os.path.join(ROOT, name)
        if os.path.isfile(path):
            files.append(path)
    for directory in ACTIVE_DOCUMENT_DIRECTORIES:
        base = os.path.join(ROOT, directory)
        if not os.path.isdir(base):
            continue
        for current, _dirs, names in os.walk(base):
            for entry in sorted(names):
                if entry.endswith(".md"):
                    files.append(os.path.join(current, entry))
    return sorted(set(files))


def count_lines(path: str) -> int:
    with open(path, "rb") as handle:
        content = handle.read()
    if not content:
        return 0
    return content.count(b"\n") + (1 if not content.endswith(b"\n") else 0)


def main() -> int:
    resolve_root(sys.argv[1:])
    violations = []
    for path in collect_active_documents():
        total = count_lines(path)
        if total > LIMIT:
            relative = os.path.relpath(path, ROOT)
            violations.append(f"{relative}: file size check: {total} lines exceeds the {LIMIT} line ceiling")
    if not violations:
        print("docs:size: checked %d active Markdown files" % len(collect_active_documents()))
        return 0
    for line in violations:
        print(line)
    print("docs:size: found %d violation(s)" % len(violations))
    return 1


if __name__ == "__main__":
    sys.exit(main())
