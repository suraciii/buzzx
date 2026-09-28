# buzzx documentation

Product documentation. It states what the product must satisfy and how a user
works with it.

- [installation.md](installation.md) is the install guide: the artifact for
  each platform, the checksums, the warnings an unsigned binary raises, and
  the first run.
- [tui.md](tui.md) is the TUI capability: its layout modes, its unified keys,
  and the criteria it must satisfy.
- [tui-use.md](tui-use.md) is the TUI manual: the keys, the modes, and the
  states a user sees.
- [configuration.md](configuration.md) is the configuration reference:
  identity, relay, the auth tag, the config file, login, environment
  variables, and exit codes.
- [buzz-revisions.md](buzz-revisions.md) records the pinned Buzz crate
  revision and what it provides.
- [browse-collab-cli.md](browse-collab-cli.md) specifies the first `buzz ext`
  browse-and-collaborate flow for terminal users and agents.

The design documents, which state the boundaries and the contracts to
preserve, live in [../design/](../design/). The product contract, which states
why `buzzx` exists, lives in [../core.md](../core.md).

The [Agents overview](tui-use.md#agents-overview) lists the Agents the signed-in
identity owns and what the owner-private observer feed says about their work.

[Resolving mentions when sending](tui-use.md#mentions-when-sending) is the
recipient-correctness rule for the composer: a complete `@member name` in a new
message or reply becomes a signed recipient, and a draft that cannot be resolved
is not published.

The [Focused thread reading](tui-use.md#focused-thread-reading) section documents the implemented thread navigation: open one discussion, reply, and return to the channel.
It is a product capability, not a planned slice.

[tui-visual.md](tui-visual.md) is the approved implementation spec for the shared TUI
visual language, with message separators, region budgets and acceptance checks.
Its atlas shows intended appearance; the change is not yet implemented.

[Complete-message reading](tui-reading.md) documents the implemented full-screen reader for a
loaded row whose content exceeds the timeline, with line scrolling and return
to its original reply target. It is part of the M1 foundation and the find-and-resume release.

[Find and resume a conversation](tui-context.md) is the release-level specification for conversation lookup, message search, context, history, complete reading and safe return. The implementation status and acceptance evidence are tracked in the linked workspace log.
