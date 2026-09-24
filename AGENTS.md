# Agents

This file holds the rules that apply across the whole `buzzx` repository. Each
rule links to the document that owns the detail.

## Product contract

Before you plan or review a change to `buzzx`:

1. Read [core.md](core.md).
2. Read the design document for the surface you touch, under
   [design/](design/).
3. Confirm that the change advances the product, or at least does not
   contradict it. State any intentional tension openly.

[core.md](core.md) is the product contract. The documents under `design/`
state why the boundaries exist and which contracts an implementation must
preserve. The documents under `docs/` state what the product must satisfy and
what a user does with it.

## Document layers

One fact has one home. Other documents link to the home; they never restate
the fact.

- `docs/` is the product specification. It is written for users.
- `design/` is the design specification. It is written for implementers.
- `eng/` is the repository engineering practice. It is written for
  contributors and agents, and it governs the repository, not the product.
- [CONTEXT.md](CONTEXT.md) is the single entry point for term definitions.
- `README.md` files index their directory.
- `design/decisions/` holds durable decision records.
- Code comments, when there is code, explain why, never what. They never cite a
  document or an issue.

The full writing rules, the diagram rules, and the context layout rules live
in [eng/context-management.md](eng/context-management.md).

## Engineering principles

- Follow KISS and YAGNI. Choose the simplest implementation that fully meets
  the current requirement. Do not add speculative configuration, abstraction,
  or indirection.
- Do not preserve backward compatibility. Remove the obsolete path instead of
  adding a compatibility layer, a shim, or a fallback.
- Grow the system in working layers. Start with the smallest version that
  works end to end, then add each capability on top of a product that already
  works. Never trade a working product for unfinished complexity.
- Reuse the Buzz crates before you write your own code. `buzz-sdk` builds the
  events; `buzz-ws-client` owns the connection; `buzz-core` owns the kinds and
  the crypto. Check their API before you assume they lack a capability.
- Keep the models small. Add only the fields that the current contract needs.

## Architecture constraints

These apply when code exists:

- Keep the layers in `design/architecture.md`. Dependencies point one way.
  `app.rs` never awaits a network call, `ui.rs` never mutates state, and
  `session.rs` is the only module that touches both transports.
- Use the two transports as the relay defines them. Persistent events go over
  the HTTP bridge. Ephemeral events and the live feed go over the WebSocket.
- Time must be injectable. No test reads the wall clock.
- A write whose result is unknown is reported as uncertain. Never retry it.
  A retry duplicates a message.

## Verification

Before you hand work to a reviewer:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
python3 scripts/check-docs.py
python3 scripts/check-file-sizes.py
python3 -m unittest discover -s tests -p '*_test.py'
```

`just check` runs all six. The documents state the intended contract. The
commands prove the repository still matches it. A plan, a draft, or a summary
in a message is not evidence.

A change that touches the session, the transport, or the render contract also
needs one live-relay run of the terminal loop - connect, send, reply, react,
edit, delete, quit - because the unit tests cannot see the relay
canonicalize an event id or re-challenge a connection.
