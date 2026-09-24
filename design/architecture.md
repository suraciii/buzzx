# Architecture

This document describes the layers of `buzzx`, what each layer owns, and which
way dependencies point. [relay-transport.md](relay-transport.md) and
[render-contract.md](render-contract.md) detail the two hard parts.

## Layers

```text diagram
 main.rs            terminal setup, event loop, teardown
   |
 app.rs             state machine: channels, rows, composer, key handling
   |
   +-----> ui.rs          ratatui widgets (render only)
   |
   +-----> keys.rs        KeyEvent -> Action (pure)
   |
   +-----> content.rs     Event -> Row (pure)
   |
   +-----> session.rs     transport: HTTP bridge + WebSocket
              |
              +-----> http.rs      NIP-98 signing, /query, /events (self-owned)
              +-----> sub.rs       live subscription pump
              |
 buzz-sdk            event builders (external)
 buzz-core            kinds, NIP-44 (external)
 buzz-ws-client      WebSocket connection (external)
```

## Why these layers

The dependency rule is one-way. `app.rs` knows about keys, rows, and the
session. It does not know about WebSocket frames, NIP-98 headers, or terminal
escape codes. `ui.rs` knows about `app.rs` state and nothing else.

This keeps the testable part large. `keys.rs`, `content.rs`, and the state
transitions in `app.rs` have no I/O. A keypress becomes an `Action`; an event
becomes a `Row`; both are pure functions over values. The tests exercise those
and the state machine directly. The terminal and the network are the only
parts that cannot be unit tested, and both are thin.

## Module ownership

| Module | Owns | Must not do |
|---|---|---|
| `main.rs` | Raw mode, alternate screen, the loop that polls input and drains events | Hold relay state or decide behavior |
| `config.rs` | Identity, relay URL, and auth-tag resolution; the config file | Open a connection or sign |
| `app.rs` | Channel list, per-channel rows, composer, selection, scroll offset, quit | Sign, send, parse frames, or draw |
| `ui.rs` | Layout and drawing | Mutate `app.rs` state or call the network |
| `keys.rs` | `KeyEvent` to `Action` | Read terminal state |
| `content.rs` | `Event` to `Row`, thread refs, reaction merging | Fetch or send |
| `session.rs` | The two channels between UI and transport, and command dispatch | Render or decide UI state |
| `http.rs` | NIP-98 signing, request building, response parsing | Hold UI state |
| `sub.rs` | The WebSocket connection, REQ lifecycle, frame decoding | Decide what a row means |

## Session: two one-way channels

`session.rs` is the only module that touches both sides. It owns two channels:

```text diagram
 UI                                 transport
   |                                     |
   |  SessionCommand  ----------------->  |  command pump (tokio task)
   |                                     |     - Send{channel, content, reply_to}
   |                                     |     - Edit{target, content, channel}
   |                                     |     - Delete{target}
   |                                     |     - React{target, emoji}
   |                                     |     - SwitchChannel{id}
   |                                     |
   |  ChatEvent  <-------------------     |  WebSocket pump (tokio task)
   |                                     |     - Connected / Disconnected
   |                                     |     - Channels / Members / History
   |                                     |     - Message / Reaction
   |                                     |     - Status / Error
```

The UI never awaits a network call. It sends a command and keeps rendering.
The command pump awaits, and sends back a `ChatEvent` when something changes.

This matters for the TUI. A slow relay must not freeze the terminal. The
render loop runs on a fixed tick, drains every queued `ChatEvent`, and draws
once. If the relay is slow, the interface still responds to keys.

## State

`app.rs` holds one `ChannelEntry` per channel:

```text literal
ChannelEntry {
    id:       channel UUID
    name:     display name
    rows:     Vec<Row>        // append only; overlays mutate rows in place
    unread:   usize           // present, not yet populated; read markers are a later phase
}
```

`rows` is append-only in the common case. An edit replaces the content of the
row with the target id. A deletion removes the row. A reaction adds or
decrements a counter on the row with the target id. There is no reorder: the
relay returns events in a stable order, and buzzx keeps it.

The composer holds a text buffer, a byte cursor, the reply target, and the
edit target. Reply target and edit target are mutually exclusive. Sending
clears both.

## The event loop

`main.rs` runs one loop:

1. Drain every queued `ChatEvent` into `app.rs`. Do this before drawing, so a
   burst of events renders once instead of once per event.
2. Draw.
3. Poll for a key with a fixed timeout. The timeout is what makes typing
   expiry and reconnects tick without a second timer.
4. Apply the action.
5. Repeat.

The poll timeout is 100 ms. Anything faster wastes frames; anything slower
makes typing indicators feel dead.

## External dependencies

`buzzx` pins Buzz by git revision:

```text literal
buzz-core     = { git = "https://github.com/block/buzz", rev = "..." }
buzz-sdk      = { git = "https://github.com/block/buzz", rev = "..." }
buzz-ws-client = { git = "https://github.com/block/buzz", rev = "..." }
```

All three are closed dependency chains: `buzz-core` and `buzz-sdk` carry no
tokio, sqlx, redis, or axum, and `buzz-ws-client` carries no internal
dependencies. A revision bump is a deliberate, reviewed change, not an
automatic update.

`nostr` stays on `0.44.x`. The Buzz crates are built against `0.44`, and
feature unification makes mixed versions fragile.

## What buzzx does not share

`BuzzClient` in `buzz-cli` is the reference implementation of the NIP-98 HTTP
client. It is private to that crate, so `buzzx` writes its own (see
[relay-transport.md](relay-transport.md)). This is a known duplication.

The duplication is deliberate and reversible. `http.rs` is one module with one
job. If Buzz later exposes a public client, `http.rs` is replaced and nothing
else changes. The alternative — patching upstream before the contract settles
— couples a young client's schedule to a large repo's review cycle.
