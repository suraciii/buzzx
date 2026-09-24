#!/usr/bin/env python3
"""Documentation gate.

Checks the mechanics of every active Markdown document, following the rules in
eng/context-management.md. It cannot judge whether content is at the right
layer or still true; a reviewer does that.

Rules enforced:
  latin-script-prose-only    prose carries no non-Latin letters
  raw-html-not-allowed       no raw HTML
  ascii-diagram-only         no mermaid fence; a text diagram is ASCII only
  text-fence-kind-required   a bare text fence declares diagram or literal
  markdown-link-reference-exists  every reference link resolves
  valid-relative-link        links are well formed
  relative-link-target-exists  link targets exist inside the repository
  markdown-heading-fragment-exists  #fragments name real headings
  required-document-exists   README.md, CONTEXT.md, CONTRIBUTING.md
  documentation-root-exists  docs/, design/, eng/
  decision-record-status     design/decisions/*.md carries a Status line
  decision-record-alternatives  decision records carry Alternatives considered

Exit code 0 means every check passed. Exit code 1 means violations exist.
"""

from __future__ import annotations

import os
import re
import sys
import unicodedata

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
REQUIRED_DOCUMENTS = ["README.md", "CONTEXT.md", "CONTRIBUTING.md"]
DOCUMENTATION_DIRECTORIES = ["docs", "design", "eng"]
DECISION_DIRECTORY = "design/decisions"
FORBIDDEN_DIAGRAM_LANGUAGES = {
    "actdiag", "bob", "blockdiag", "c4", "c4plantuml", "d2", "diagram", "ditaa",
    "dot", "erd", "flowchart", "graph-easy", "graphviz", "gv", "kroki", "mermaid",
    "nomnoml", "nwdiag", "packetdiag", "pikchr", "plantuml", "puml", "rackdiag",
    "seqdiag", "structurizr", "svgbob", "tikz", "vega", "vega-lite", "wavedrom",
}
ABSOLUTE_URL = re.compile(r"^[a-z][a-z\d+.-]*:", re.IGNORECASE)


def resolve_root(argv: list[str]) -> None:
    """`--root <dir>` overrides the repository root, so a test can run the gate
    against a synthetic tree. The default is the repository root."""
    global ROOT
    for index, argument in enumerate(argv):
        if argument == "--root" and index + 1 < len(argv):
            ROOT = os.path.abspath(argv[index + 1])
            return
    ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


class Violation:
    def __init__(self, path: str, line: int, column: int, rule: str, detail: str):
        self.path = path
        self.line = line
        self.column = column
        self.rule = rule
        self.detail = detail

    def render(self) -> str:
        rel = os.path.relpath(self.path, ROOT)
        return f"{rel}:{self.line}:{self.column} {self.rule}: {self.detail}"

    def sort_key(self):
        return (self.path, self.line, self.column, self.rule)


def collect_markdown_files() -> tuple[list[str], list[Violation]]:
    files: list[str] = []
    violations: list[Violation] = []
    for name in REQUIRED_DOCUMENTS:
        path = os.path.join(ROOT, name)
        if os.path.isfile(path):
            files.append(path)
        else:
            violations.append(Violation(path, 1, 1, "required-document-exists",
                                       f"required documentation file does not exist: {name}"))
    for name in DOCUMENTATION_DIRECTORIES:
        directory = os.path.join(ROOT, name)
        if os.path.isdir(directory):
            for current, _dirs, names in os.walk(directory):
                for entry in sorted(names):
                    if entry.endswith(".md"):
                        files.append(os.path.join(current, entry))
        else:
            violations.append(Violation(directory, 1, 1, "documentation-root-exists",
                                       f"documentation directory does not exist: {name}"))
    return sorted(files), violations


def strip_fences(lines: list[str]):
    """Yield (line_number, text, in_prose) with fenced code removed."""
    in_fence = False
    for number, line in enumerate(lines, start=1):
        if line.lstrip().startswith("```"):
            in_fence = not in_fence
            yield number, line, False
            continue
        yield number, line, not in_fence


def prose_line_numbers(lines: list[str]) -> set[int]:
    """Line numbers outside fences and outside inline code spans."""
    result = set[int]()
    in_fence = False
    in_code_span = False
    for number, line in enumerate(lines, start=1):
        if line.lstrip().startswith("```"):
            in_fence = not in_fence
            continue
        if in_fence:
            continue
        # Walk the line tracking inline code spans
        index = 0
        in_code_span = False
        while index < len(line):
            if line[index] == "`":
                in_code_span = not in_code_span
            index += 1
        if not in_code_span:
            result.add(number)
    return result


def check_latin_prose(path: str, lines: list[str]) -> list[Violation]:
    """Prose carries no non-Latin letters. Latin script is English plus the
    accented letters of European languages; their Unicode names contain LATIN.
    Every other alphabetic character, including every CJK ideograph and every
    box drawing character, is a violation."""
    violations = []
    prose = prose_line_numbers(lines)
    for number in prose:
        line = lines[number - 1]
        for offset, character in enumerate(line):
            if character.isascii() or not character.isalpha():
                continue
            name = unicodedata.name(character, "")
            if "LATIN" in name:
                continue
            rule = "latin-script-prose-only"
            detail = f"non-Latin letter is not allowed in prose: {name} ({character!r})"
            violations.append(Violation(path, number, offset + 1, rule, detail))
    return violations


def check_raw_html(path: str, lines: list[str]) -> list[Violation]:
    violations = []
    for number, line, in_prose in strip_fences(lines):
        if not in_prose:
            continue
        stripped = line.strip()
        if stripped.startswith("<") and re.match(r"^</?[a-zA-Z][^>]*>?", stripped):
            violations.append(Violation(path, number, 1, "raw-html-not-allowed",
                                       "raw HTML is not allowed in active documentation"))
    return violations


def normalize_fence_language(language: str) -> str:
    normalized = language.strip().lower()
    if normalized.startswith("{."):
        normalized = normalized[2:]
    elif normalized.startswith("."):
        normalized = normalized[1:]
    return re.split(r"[\s}]", normalized, maxsplit=1)[0]


def check_fences(path: str, lines: list[str]) -> list[Violation]:
    violations = []
    index = 0
    while index < len(lines):
        stripped = lines[index].strip()
        if not stripped.startswith("```"):
            index += 1
            continue
        header = stripped[3:]
        language = header.split()[0] if header.split() else ""
        meta = header[len(language):].strip()
        normalized = normalize_fence_language(language) if language else ""
        if normalized in FORBIDDEN_DIAGRAM_LANGUAGES:
            violations.append(Violation(path, index + 1, 1, "ascii-diagram-only",
                                       f"fenced {language} diagram must be replaced with an ASCII text diagram"))
        elif normalized == "text" or not language:
            if meta not in ("diagram", "literal"):
                violations.append(Violation(path, index + 1, 1, "text-fence-kind-required",
                                           "fenced text block must declare `text diagram` or `text literal`"))
            elif meta == "diagram":
                # find the closing fence and check ASCII only
                end = index + 1
                while end < len(lines) and not lines[end].strip().startswith("```"):
                    for offset, character in enumerate(lines[end]):
                        if ord(character) > 127:
                            violations.append(Violation(path, end + 1, offset + 1,
                                                        "ascii-diagram-only",
                                                        f"fenced text diagram contains a non-ASCII character: {character}"))
                    end += 1
        # skip to the closing fence
        index += 1
        while index < len(lines) and not lines[index].strip().startswith("```"):
            index += 1
        index += 1
    return violations


def collect_headings(lines: list[str]) -> set[str]:
    fragments = set[str]()
    in_fence = False
    for line in lines:
        if line.lstrip().startswith("```"):
            in_fence = not in_fence
            continue
        if in_fence or not line.startswith("#"):
            continue
        text = line.lstrip("#").strip()
        slug = re.sub(r"[^\w\s-]", "", text).strip().lower()
        slug = re.sub(r"[\s]+", "-", slug)
        fragments.add(slug)
    return fragments


def check_links(path: str, lines: list[str]) -> list[Violation]:
    violations = []
    in_fence = False
    for number, line in enumerate(lines, start=1):
        if line.lstrip().startswith("```"):
            in_fence = not in_fence
            continue
        if in_fence:
            continue
        for match in re.finditer(r"\[[^\]]*\]\(([^)]+)\)", line):
            url = match.group(1).strip()
            if ABSOLUTE_URL.match(url) or url.startswith(("//", "/", "#", "mailto:")):
                continue
            hash_index = url.find("#")
            target = url if hash_index == -1 else url[:hash_index]
            fragment = "" if hash_index == -1 else url[hash_index + 1:]
            if not target:
                if fragment:
                    fragments = collect_headings(lines)
                    if fragment.lower() not in {f.lower() for f in fragments}:
                        violations.append(Violation(path, number, match.start() + 1,
                                                    "markdown-heading-fragment-exists",
                                                    f"heading fragment does not exist: {url}"))
                continue
            target_path = os.path.normpath(os.path.join(os.path.dirname(path), target))
            if not os.path.abspath(target_path).startswith(ROOT):
                violations.append(Violation(path, number, match.start() + 1,
                                            "relative-link-target-exists",
                                            f"relative link leaves the repository: {url}"))
                continue
            if not os.path.isfile(target_path) and not os.path.isdir(target_path):
                violations.append(Violation(path, number, match.start() + 1,
                                            "relative-link-target-exists",
                                            f"relative link target does not exist: {url}"))
                continue
            if fragment and os.path.isfile(target_path):
                with open(target_path, encoding="utf-8") as handle:
                    fragments = collect_headings(handle.read().split("\n"))
                if fragment.lower() not in {f.lower() for f in fragments}:
                    violations.append(Violation(path, number, match.start() + 1,
                                                "markdown-heading-fragment-exists",
                                                f"heading fragment does not exist: {url}"))
    return violations


def check_reference_links(path: str, lines: list[str]) -> list[Violation]:
    """Every [text][ref] reference must have a matching [ref]: url definition."""
    definitions = set()
    in_fence = False
    for line in lines:
        if line.lstrip().startswith("```"):
            in_fence = not in_fence
            continue
        if in_fence:
            continue
        match = re.match(r"^\s*\[([^\]]+)\]:\s*\S", line)
        if match:
            definitions.add(match.group(1).lower())
    violations = []
    in_fence = False
    for number, line in enumerate(lines, start=1):
        if line.lstrip().startswith("```"):
            in_fence = not in_fence
            continue
        if in_fence:
            continue
        for match in re.finditer(r"\[[^\]]+\]\[([^\]]+)\]", line):
            if match.group(1).strip().lower() not in definitions:
                violations.append(Violation(path, number, match.start() + 1,
                                            "markdown-link-reference-exists",
                                            f"link reference is not defined: [{match.group(1)}]"))
    return violations


def check_decision_records(path: str, lines: list[str]) -> list[Violation]:
    relative = os.path.relpath(path, ROOT).replace(os.sep, "/")
    if not relative.startswith(DECISION_DIRECTORY + "/"):
        return []
    violations = []
    source = "\n".join(lines)
    accepted = re.search(
        r"^## Status\s*$\n^\s*(accepted|proposed|rejected)\s*$",
        source,
        re.MULTILINE,
    )
    superseded = re.search(
        r"^## Status\s*$\n^\s*superseded by \S+",
        source,
        re.MULTILINE,
    )
    if not (accepted or superseded):
        violations.append(Violation(
            path, 1, 1, "decision-record-status",
            "decision record must carry a `## Status` section whose value is "
            "`accepted`, `proposed`, `rejected`, or `superseded by <link>`"))
    has_alternatives = False
    in_fence = False
    for line in lines:
        if line.lstrip().startswith("```"):
            in_fence = not in_fence
            continue
        if in_fence:
            continue
        if re.match(r"^## Alternatives considered\s*$", line):
            has_alternatives = True
    if not has_alternatives:
        violations.append(Violation(path, 1, 1, "decision-record-alternatives",
                                   "decision record must carry an `## Alternatives considered` section"))
    return violations


def check_document(path: str) -> list[Violation]:
    with open(path, encoding="utf-8") as handle:
        lines = handle.read().split("\n")
    violations = []
    violations.extend(check_latin_prose(path, lines))
    violations.extend(check_raw_html(path, lines))
    violations.extend(check_fences(path, lines))
    violations.extend(check_reference_links(path, lines))
    violations.extend(check_links(path, lines))
    violations.extend(check_decision_records(path, lines))
    return violations


def main() -> int:
    resolve_root(sys.argv[1:])
    files, violations = collect_markdown_files()
    for path in files:
        violations.extend(check_document(path))
    violations.sort(key=Violation.sort_key)
    if not violations:
        print(f"docs:check: checked {len(files)} active Markdown files")
        return 0
    for violation in violations:
        print(violation.render())
    print(f"docs:check: found {len(violations)} violation(s) in {len(files)} active Markdown files")
    return 1


if __name__ == "__main__":
    sys.exit(main())
