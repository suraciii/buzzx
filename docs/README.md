# buzzx documentation

Product documentation. It states what the product must satisfy and how a user
works with it.

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

The planned [Agents overview](tui-use.md#agents-overview) is the next
read-only Agent capability; it is not implemented yet.

The planned [send-time mention resolution](tui-use.md#planned-resolve-mentions-when-sending)
is the minimal recipient-correctness slice for Build.
