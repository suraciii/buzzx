# Contributing to buzzx

## Setup

`buzzx` has no Rust code yet. The repository currently holds the product
contract, the design documents, and the documentation gates. Python 3 runs the
gates, and nothing else is required.

```bash
python3 scripts/check-docs.py
python3 scripts/check-file-sizes.py
python3 -m unittest discover -s tests -p '*_test.py'
```

When the first Rust workspace lands, this section gains the toolchain line,
`Cargo.toml` at the root, and the cargo commands. Until then, a cargo command
in a contributing guide would be a documented failure.

## Run

Nothing runs yet. The gates run, and that is the whole executable surface
today. `docs/tui-use.md` describes the intended CLI and TUI behaviour.

## Local verification

```bash
python3 scripts/check-docs.py
python3 scripts/check-file-sizes.py
python3 -m unittest discover -s tests -p '*_test.py'
```

`just check` runs all three. The documentation gate runs in CI, so a commit
that fails it does not land.

## Documentation changes

Read [eng/context-management.md](eng/context-management.md) before you write
or edit a document. It holds the layer rules, the diagram rules, the prose
rules, and the shape of the documentation tree.

The short version:

- One fact, one home. Link to the home; never restate it.
- Latin script only, in prose and in every diagram.
- Use a relative Markdown link, not a backticked path.
- Never introduce mermaid or another diagram DSL.
- Split a file at 1000 lines; never raise the ceiling.

## Review contract

A change lands when:

- Both gate scripts pass and the gate tests pass.
- The contracts in `design/` still describe the code, when there is code.
- A new term goes into [CONTEXT.md](CONTEXT.md).
- A durable decision goes into `design/decisions/` as its own document.
- No test asserts wiring, copies, source text, or a mock echo.

## Reporting a bug

Open an issue with:

- The relay host class and the identity kind, for example "human, local
  relay".
- The exact command, including the flags.
- What you expected and what happened instead.
- The terminal and the version, for a TUI report.
- The output of the gateway failure, for a relay error.
