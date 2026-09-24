# buzzx

`buzzx` is a terminal client for Buzz users. It gives one human identity full
channel chat - read, send, reply, react, edit, delete - from a terminal, over
the relay they already belong to.

- [`buzzx tui`](docs/tui-use.md) starts the terminal chat session.
- `buzzx watch` streams live channel events as JSON.

Buzz ships a Tauri desktop app, a Flutter mobile app, and the `buzz` CLI. The
desktop and mobile apps need a graphical session. The `buzz` CLI is a
scriptable interface: it returns JSON, and it does not hold a live
subscription. Buzz users who live in a terminal - over SSH, in a container, on
a headless box - have no client of their own.

`buzzx` is that client: Buzz's fourth surface, for people. It does not replace
the desktop app, the mobile app, or the CLI.

## What buzzx uses from Buzz

`buzzx` depends on three Buzz crates through a pinned git revision:

- `buzz-core` provides the kind constants, the NIP-44 encryption helpers, and
  event verification.
- `buzz-sdk` provides the event builders for messages, edits, deletions,
  reactions, and presence.
- `buzz-ws-client` provides one NIP-42 WebSocket connection: `send_raw` for
  REQ and `next_event` for incoming frames.

The pinned revision and what it provides are recorded in
[docs/buzz-revisions.md](docs/buzz-revisions.md).

`buzzx` does not depend on the relay, the database, or the desktop crate. It
talks to a Buzz relay over the relay's own public surface.

## Two transports, and why

A Buzz relay exposes an HTTP bridge and a Nostr WebSocket. `buzzx` uses both,
because the relay itself splits work between them:

- **HTTP bridge** carries persistent events - channel messages, edits,
  deletions, reactions. The client signs a NIP-98 event and posts it to
  `POST /events`. Reads use `POST /query`. The relay applies full tag
  validation on this path only, so any event with attachments, emoji, or
  mention overlays must use it.
- **WebSocket** carries ephemeral events - typing indicators (kind 20002) and
  presence (kind 20001). The relay rejects these over HTTP. The same
  connection carries the live subscription that feeds the timeline.

The desktop app makes the same split. `buzzx` mirrors it rather than
inventing a new contract.

## Documents

- [Core](core.md) - what buzzx is, its users, and its boundaries.
- [design/architecture.md](design/architecture.md) - layers, ownership, and the module split.
- [design/relay-transport.md](design/relay-transport.md) - auth, subscriptions, limits, and the self-owned NIP-98 layer.
- [design/render-contract.md](design/render-contract.md) - the event-to-row mapping and the unread model.
- [docs/tui-use.md](docs/tui-use.md) - the terminal interface: keys, layout, and what it does not do.
- [docs/configuration.md](docs/configuration.md) - identity, relay selection, and credentials.
- [docs/buzz-revisions.md](docs/buzz-revisions.md) - the pinned Buzz revisions and the upstream coupling they carry.

## Status

`buzzx` is new. The documents describe the intended contract, and no code
exists yet. The first phase is a working Rust workspace plus the gates in this
repository. See [core.md](core.md) for the boundaries and
[eng/context-management.md](eng/context-management.md) for the document rules.
