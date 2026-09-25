# buzzx design

Design specification. It states why the boundaries exist and which contracts
an implementation must preserve.

- [architecture.md](architecture.md) is the module map: what each layer owns
  and the direction the dependencies point.
- [shared-core.md](shared-core.md) is the core contract: the relay operations
  the TUI and the CLI both perform, what each of them owns on top, and the
  failure categories and exit codes.
- [relay-transport.md](relay-transport.md) is the relay contract: the HTTP
  bridge, the WebSocket, NIP-98, the ephemeral kinds, and failure handling.
- [render-contract.md](render-contract.md) is the row contract: which kinds
  become rows, which are overlays, and how a row is shaped.
- [decisions/0003-owner-plane-out-of-scope.md](decisions/0003-owner-plane-out-of-scope.md)
  is why the owner's agent observation plane is excluded, with the facts that
  settled it.
- [decisions/0005-shared-core.md](decisions/0005-shared-core.md) is why the
  core is a module of the one binary, and not a crate or a trait.

The product contract, which states why `buzzx` exists, lives in
[../core.md](../core.md). The term definitions live in
[../CONTEXT.md](../CONTEXT.md).

`design/decisions/` holds durable decision records. Each record carries a
Status line and an Alternatives considered section.
