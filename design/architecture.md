# Architecture

This document describes the layers of `buzzx`, what each layer owns, and which
way dependencies point. [relay-transport.md](relay-transport.md) and
[render-contract.md](render-contract.md) detail the two hard parts.

The CLI slice added one-shot commands that perform operations the TUI already
performed. [shared-core.md](shared-core.md) fixes the shape of that work: a
core module both front ends drive, with the bridge moved out of `session.rs`.
That core is `client.rs`, and this map records where it sits.

## Layers

```text diagram
 main.rs            terminal setup, event loop, teardown, the clap tree
   |
   +-----> login.rs      key input, wizard, relay verification, config write
   |
   +-----> cli.rs        one-shot commands: JSON on stdout, exit codes
   |         |
   |         +-----> client.rs  the relay operations both front ends drive
   |                   |
   |                   +-----> http.rs     NIP-98 bridge: /query, /events
   |                   +-----> content.rs  event accessors (pure)
   |                   +-----> failure.rs  the failure categories
   |
   +-----> app.rs        state machine: channels, rows, composer, key handling
   |         |
   |         +-----> ui.rs      ratatui widgets (render only)
   |         +-----> layout.rs  terminal size -> layout mode (pure)
   |         +-----> keys.rs    KeyEvent -> Action (pure)
   |         +-----> content.rs Event -> Row (pure)
   |
   +-----> session.rs    the live session: subscriptions and ChatEvents
             |
             +-----> client.rs   the same core calls, one ChatEvent per result
             +-----> sub.rs      live subscription pump
             |
 buzz-sdk            event builders (external)
 buzz-core            kinds, NIP-44 (external)
 buzz-ws-client      WebSocket connection (external)
```

The dependency rule: `cli.rs` and `session.rs` sit above `client.rs`, and
nothing above `client.rs` names `http.rs`. The core has no rendering, no
wording, and no exit code; it does not know either front end exists. `app.rs`
and `ui.rs` never call the core directly; they ask the session, which is what
keeps the TUI's half of the rule (a slow relay must not freeze the terminal).

## Why these layers

The dependency rule is one-way. `app.rs` knows about keys, rows, and the
session. It does not know about WebSocket frames, NIP-98 headers, or terminal
escape codes. `ui.rs` knows about `app.rs` state and nothing else.

This keeps the testable part large. `keys.rs`, `content.rs`, the state
transitions in `app.rs`, and the core's own rules have no I/O. A keypress
becomes an `Action`; an event becomes a `Row`; a filter becomes a query; each
is a pure function over values, or a call whose parse is. The terminal is the
only part that cannot be unit tested, and it is thin. The network is exercised
against a fake relay in `tests/cli.rs` and against the live relay by hand.

## Module ownership

| Module | Owns | Must not do |
|---|---|---|
| `main.rs` | Raw mode, alternate screen, the loop that polls input and drains events | Hold relay state or decide behavior |
| `config.rs` | Identity, relay URL, and auth-tag resolution; the config file | Open a connection or sign |
| `app.rs` | Conversations, rows, composer, filters, unread tracking, read frontiers, selection, and quit | Sign, send, parse frames, or draw |
| `ui.rs` | Layout and rendering of the Inbox, timeline, picker, and help | Mutate `app.rs` state or call the network |
| `keys.rs` | `KeyEvent` to `Action` | Read terminal state |
| `layout.rs` | The layout mode a terminal size selects | Hold state, draw, or read `App` |
| `content.rs` | `Event` to `Row`, thread refs, reaction merging, and event kind lists | Fetch or send |
| `login.rs` | The login flow: key input from flag, file, stdin, environment, or wizard; one-shot relay verification; the atomic config write | Hold session state, render the TUI, or stay in the process after the config is written |
| `cli.rs` | The one-shot commands: argument validation, the JSON projection, stdin content, exit codes | Hold state between calls, or decide the relay protocol |
| `client.rs` | Relay queries, read-state slot encryption and merge, channel list, timeline, thread, profiles, and writes | Render, word a message, or choose an exit code |
| `read_state.rs` | Read-state slot id, encrypted payload construction, and merge of owned slots | Query or publish events |
| `failure.rs` | The failure categories both front ends branch on | Format a user-facing message |
| `session.rs` | The live session: subscriptions, reconnect, catch-up, delayed read publication, and translation into `ChatEvent`s | Render, decide UI state, or sign |
| `http.rs` | NIP-98 signing, request building, response parsing, write classification | Hold UI state |
| `sub.rs` | The WebSocket connection, REQ lifecycle, frame decoding, and live Inbox feeds | Decide what a row means |

`session.rs` owns two channels:

```text diagram
 UI                                 transport
   |                                     |
   |  SessionCommand  ----------------->  |  command pump (tokio task)
   |                                     |     - LoadChannels / OpenChannel
   |                                     |     - LoadProfiles
   |                                     |     - Send / Edit / Delete / React
   |                                     |       -> one client.rs call each
   |                                     |       -> one ChatEvent per write
   |                                     |
   |  ChatEvent  <-------------------     |  WebSocket pump (tokio task)
   |                                     |     - Connected / Disconnected
   |                                     |     - Channels / History / Profiles
   |                                     |     - Timeline / InboxTimeline / Overlay / Typing
   |                                     |     - ReadState / CatchUp / Seed / ReadPublished
   |                                     |     - ChannelGone / InboxClosed / TypingClosed
   |                                     |     - WriteOk / WriteFailed / WriteUncertain / Status
```

The UI never awaits a network call. It sends a command and keeps rendering.
The command pump awaits, and sends back a `ChatEvent` when something changes.
What the pump does not do is build or sign an event: that is `client.rs`.

This matters for the TUI. A slow relay must not freeze the terminal. The
render loop runs on a fixed tick, drains every queued `ChatEvent`, and draws
once. If the relay is slow, the interface still responds to keys.

## State
`app.rs` holds one `ChannelEntry` per listed conversation, with its timeline
rows and a `ReadTrack` containing its frontier, unread message candidates, and
coverage state. `Filter` derives the `All`, `Unread`, and `For you` views; the
picker retains its cursor conversation if a filter change would hide it. The
wide UI groups rows into `Channels` and `DMs`. `●` and `@` rows carry unread
candidate counts in the reserved signal cell; `?` means unknown coverage. The
`Read` marker applies to a picker row retained after it stops matching.

The read-state event and its limits are described in
[shared-core.md#inbox-and-read-state](shared-core.md#inbox-and-read-state).
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
