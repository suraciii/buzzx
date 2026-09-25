# Decision: 0005 - The CLI and the TUI share one core

## Status

proposed

## Context

[docs/browse-collab-cli.md](../../docs/browse-collab-cli.md) adds five one-shot
commands to `buzzx`: `channels list`, `messages get`, `messages thread`,
`messages send`, and `messages reply`. Four of them perform an operation the
TUI already performs inside `session.rs`: the channel list, the channel
history, the signing and submit path, and the write-result classification.
That code sits inside the session pump, next to the subscriptions and the
`ChatEvent` plumbing, so the CLI could not use it without carrying a live
session with it.

The question was where the shared part lives: a second crate, a transport
trait with a fake, or a module of the one binary.

[core.md](../../core.md) already requires the non-interactive subcommands to
share the transport, the identity handling, and the event construction with
the TUI, and it permits `buzzx` to provide the one-shot operations `buzz`
serves badly. One of its paragraphs still sends the automation caller's
one-shot reads to `buzz` instead, which
[docs/browse-collab-cli.md](../../docs/browse-collab-cli.md) supersedes for
this slice; the product contract needs that paragraph updated, and this
decision does not depend on how it is worded.

## Decision

The core is `src/client.rs`, a module of the one `buzzx` binary. Both the TUI
session and the CLI drive it. It owns the relay operations, the signing path,
and the write outcome, and nothing else: no rendering, no wording, no exit
code. The failure categories live in `src/failure.rs`, a leaf module the
bridge and the core both import.

The CLI is one-shot: parse the arguments, call the core once, print JSON,
exit. The TUI session keeps what a held session needs: subscriptions,
reconnect, optimistic rows, ephemeral state, and the translation of results
into `ChatEvent`s.

The contracts are in [shared-core.md](../shared-core.md): the core's
operations, the reply routing rule, the failure categories and their exit
codes, and the CLI output the product spec leaves open.

## Consequences

- A second front end adds a driver, not an implementation. It cannot invent
  a second channel query, a second signature, or a second write status.
- `session.rs` stops naming the bridge and the builders, so every relay call
  and every signed event has one path through the code.
- One write classification serves the CLI's exit code, the CLI's JSON, and
  the TUI's status line, which is why they cannot disagree.
- A write whose connect failed before the request left the client becomes a
  failure, not an uncertain result: the optimistic row goes away and the
  draft, the reply target, and the composer mode come back. An unconfirmed
  write keeps its row marked uncertain, and nothing is ever retried
  automatically.
- The core is not a published API and carries no version. If a second binary
  ever needs it, this decision is revisited rather than worked around.

## Alternatives considered

**A `buzzx-core` crate in a workspace.** Gives the boundary its own manifest
and lets a second binary link it. Rejected: there is one binary, module
privacy already enforces the boundary, and the split adds a crate, a version,
and a published surface for a consumer that does not exist. YAGNI applies.

**A transport trait, so the core can be tested against a fake.** Rejected:
every operation is a query plus a pure parse, and the parse is what carries
the decisions, so the tests target the parse directly. A trait would add
indirection for tests that the extracted functions already allow.

**Call the session's operations from the CLI.** Rejected: a one-shot command
would then stand up a WebSocket pump, a reconnect loop, and subscriptions it
never reads, to print twenty messages. The CLI would inherit the session's
lifetime and its failure modes.

**Duplicate the four operations in the CLI.** Rejected: two channel queries,
two history orders, two signing paths, and two write classifications drift
silently. Nothing fails when they diverge, and the two front ends start
reporting different answers for the same relay.

**Keep the CLI out of the TUI's transport and use `buzz` for reads.**
Rejected by the slice's own problem statement: `buzz`'s surface is not owned
by this project and does not return the stable identifiers and write statuses
the flow needs. Two clients also mean two identities, two auth paths, and two
JSON shapes for one answer.
