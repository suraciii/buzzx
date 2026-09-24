# buzzx design

Design specification. It states why the boundaries exist and which contracts
an implementation must preserve.

- [architecture.md](architecture.md) is the module map: what each layer owns
  and the direction the dependencies point.
- [relay-transport.md](relay-transport.md) is the relay contract: the HTTP
  bridge, the WebSocket, NIP-98, the ephemeral kinds, and failure handling.
- [render-contract.md](render-contract.md) is the row contract: which kinds
  become rows, which are overlays, and how a row is shaped.

The product contract, which states why `buzzx` exists, lives in
[../core.md](../core.md). The term definitions live in
[../CONTEXT.md](../CONTEXT.md).

`design/decisions/` holds durable decision records. Each record carries a
Status line and an Alternatives considered section.
