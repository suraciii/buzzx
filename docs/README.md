# buzzx documentation

Product documentation. It states what the product must satisfy and how a user
works with it.

- [interactive.md](interactive.md) owns the shared TUI/Web capability catalog
  and synchronized acceptance, including find-and-resume and complete reading.
- [installation.md](installation.md) is the install guide: the artifact for
  each platform, the checksums, the warnings an unsigned binary raises, and
  the first run.
- [versioning-and-updates.md](versioning-and-updates.md) defines release and
  development version strings, source/prebuilt installation, and update
  commands.
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
- [mentions-and-channel-creation.md](mentions-and-channel-creation.md) specifies
  the implementation-ready mention suggestion, signed-recipient and channel
  creation contract for TUI, WebUI and CLI.
- [channel-lifecycle.md](channel-lifecycle.md) specifies channel metadata,
  archive/restore, member roles, add/remove/leave flows, permissions and
  write-outcome handling for TUI, WebUI and CLI.
- [web.md](web.md) specifies the proposed local browser surface, its TUI
  equivalent controls, interaction flows and acceptance. It is not implemented.

The design documents, which state the boundaries and the contracts to
preserve, live in [../design/](../design/). The product contract, which states
why `buzzx` exists, lives in [../core.md](../core.md).

The [Agents overview](tui-use.md#agents-overview) lists the Agents the signed-in
identity owns and what the owner-private observer feed says about their work.

[Resolving mentions when sending](tui-use.md#mentions-when-sending) is the
recipient-correctness rule for the composer: a complete `@member name` in a new
message or reply becomes a signed recipient, and a draft that cannot be resolved
is not published.

[Focused thread reading](tui-use.md#focused-thread-reading) opens one
discussion, replies, and returns to the channel. See its status in the manual.

[tui-visual.md](tui-visual.md) is the approved implementation spec for the shared TUI
visual language, with message separators, region budgets and acceptance checks.
Its status section separates implementation evidence from the intended atlas.

[Complete-message reading](tui-reading.md) documents the implemented full-screen reader for a
loaded row whose content exceeds the timeline, with line scrolling and return
to its original reply target. It is part of the M1 foundation and the find-and-resume release.

[Find and resume a conversation](tui-context.md) is the release-level specification for conversation lookup, message search, context, history, complete reading and safe return. The implementation status and acceptance evidence are tracked in the linked workspace log.
