# Contributing to buzzx

## Setup

`buzzx` is Rust, and the gates are Python. Install the Rust toolchain that CI
pins ([eng/ci.md](eng/ci.md)) with the `clippy` and `rustfmt` components, plus
[just](https://github.com/casey/just) for the verification recipes.

## Run

`buzzx tui` opens the client. `buzzx login`, `buzzx whoami`, and `buzzx
logout` manage the identity. [docs/configuration.md](docs/configuration.md)
holds the commands and the config file. [docs/tui-use.md](docs/tui-use.md) is
the terminal interface.

## Local verification

```bash
just check
```

The recipes are the contract a commit must satisfy. CI runs the same recipes
on every pull request and on every push to `main`, so a commit that fails one
does not land.

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

- Every recipe in `just check` passes.
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
