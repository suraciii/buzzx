# Context Management

The documentation rules for this repository, and how an agent keeps its own
context in the same shape.

## The gate

`python3 scripts/check-docs.py` checks every `.md` file under `docs/`,
`design/`, and `eng/`, plus the root documentation files. Exit code 1 means a
violation exists.

| Rule | Meaning |
|---|---|
| `latin-script-prose-only` | Prose carries no non-Latin letters |
| `raw-html-not-allowed` | No raw HTML |
| `ascii-diagram-only` | No mermaid fence; a text diagram is ASCII only |
| `text-fence-kind-required` | A bare text fence declares diagram or literal |
| `markdown-link-reference-exists` | Every reference link resolves |
| `valid-relative-link` | Links are well formed |
| `relative-link-target-exists` | Link targets exist inside the repository |
| `markdown-heading-fragment-exists` | `#fragments` name real headings |
| `required-document-exists` | README.md, CONTEXT.md, CONTRIBUTING.md |
| `documentation-root-exists` | docs/, design/, eng/ |
| `decision-record-status` | design/decisions/*.md carries a Status line |
| `decision-record-alternatives` | Decision records carry Alternatives considered |

The gate checks mechanics. It cannot judge whether content is at the right
layer, whether coverage is complete, or whether a sentence is still true. A
reviewer does that.

## Prose for a global audience

Write Latin script only. Full-width punctuation, CJK characters, curly quotes,
and box drawing characters are not allowed in prose. The gate scans every
line outside a fenced block and outside an inline code span.

This is `mohist`'s rule for documents that an international audience reads.
The same rationale applies here: `buzzx` and Buzz are open source, so the
prose must be translatable without editing artifacts out of it.

Notes:

- Inline code, fenced blocks, and URLs are exempt.
- An em dash is not a non-Latin glyph, but it is a typographic ambiguity. Use
  " - " like this document does.
- Write plainly. Forget creative labels like "Determinism cuts both ways".
  State a plain section title instead.
- Keep each sentence ASCII. Where a distinctive character is genuinely needed
  inside prose, it belongs in inline code.

## Diagram rules

Never introduce mermaid or any diagram DSL. The tools will not render it, and a
"diagram" version becomes a second source of truth, silently stale.

- A `text diagram` fence holds only ASCII art. The gate enforces this.
- A `text literal` fence holds a code fragment, a Rust type, or a JSON blob.
- A box drawing character, a full-width bar, and a Unicode arrow are not ASCII
  art. Use `+`, `-`, `|`, `>`, and `<` instead.
- Prefer a bullet list over a diagram whenever the structure fits the format.
  A diagram earns its keep when it shows shape, which is a lattice, a cycle, a
  boundary, or a correspondence between two columns. It does not earn it when
  it restates a list.

## Tables

Prefer bullets or prose. A table is allowed when the comparison is genuinely
row-shaped, such as the gate rule list above. Prefer prose when a table's cell
values are sentences, which is the case for most design content.

## Link rules

- Always write a plain Markdown relative link. This document links to
  [testing](testing.md) and to [AGENTS.md](../AGENTS.md) the same way. Never
  write a backticked path alone, which tools cannot hyperlink.
- `python3 scripts/check-docs.py` verifies that every target exists.
- A URL may be absolute, for example
  `[Buzz](https://github.com/block/buzz)`. A link must point at a document
  that the repository owns or that exhausts its subject.

## Layer rules

One fact has one home. Other documents link to the home; they never restate
the fact. When you add a fact, put it in the owning layer:

| Task | Destination |
|---|---|
| Define a term | [CONTEXT.md](../CONTEXT.md) |
| Record a boundary | [architecture.md](../design/architecture.md) |
| State user-visible behavior | [docs/tui-use.md](../docs/tui-use.md) |
| State an API or protocol contract | [relay-transport.md](../design/relay-transport.md) |
| Record a durable decision | `design/decisions/<slug>.md` |

A document that restates another document is not allowed to keep its own copy
in sync. Prefer prescribing semantics over prescribing files. Code comments
explain why, never what, and they never cite a document or an issue.

## Context layout

| Document | Role |
|---|---|
| `core.md` | Why `buzzx` exists and what it must never become |
| `AGENTS.md` | Every cross-repo rule, the document layers, the verification list |
| `CONTEXT.md` | Every term, one definition each. The single entry point |
| `CONTRIBUTING.md` | Setup, commands, local verification |
| `docs/` | Product specification: what the product must satisfy and how it is used |
| `design/` | Design specification: boundaries and the contracts to preserve |
| `design/decisions/` | Durable decision records, one per document |
| `eng/` | Repository engineering practice. Never product facts |

## Coverage and staleness

A reviewer checks three things that the gate cannot:

1. Every fact exists in at least one active document.
2. Every fact has exactly one home. A fact in two places is a bug.
3. Every document whose subject changed is still consistent.

When a fact moves, the old location either links to the new home or deletes
itself. A stale claim is worse than a missing one: it teaches the reader the
wrong shape of the system.

## File size

`python3 scripts/check-file-sizes.py` enforces a 1000-line ceiling on every
source and document file. Split a file when it trips the gate; never raise the
limit. See [testing.md](testing.md) for the companion testing rules.

## Pronunciation

- "`buzzx`", lowercase, one word, no space. It is pronounced like "buzz".
- "Buzz", capitalized, is the system `buzzx` is a client of.
- "relay" is lowercase.
