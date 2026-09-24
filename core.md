# buzzx Core

This document defines what `buzzx` is, who it is for, and what it does not do.
It is the product contract. [design/](design/) documents say how the contract
is implemented.

## The problem

A Buzz relay is headless by design. A workstation can run the relay, the
agents, and the automation. What a workstation could not run, until now, is a
client for a human.

The existing clients do not fit a terminal-only workstation:

- The desktop app is a Tauri application. It needs a graphical session and a
  native window.
- The mobile app is Flutter. It needs a phone.
- The `buzz` CLI is scriptable. It answers one question per invocation and
  returns JSON. It does not keep a subscription open, so it cannot show a
  channel as it changes.

The result is a workstation that participates fully in automation but gives
its operator nothing to type into.

## The product

`buzzx` is a terminal client for one human identity on one Buzz relay.

`buzzx tui` opens a session with three regions:

1. A channel list on the left. It shows the channels the identity belongs to
   and the unread count for each.
2. A timeline on the right. It shows the messages of the selected channel.
3. A composer at the bottom. It sends, replies, reacts, edits, and deletes.

The session is live. A message that arrives appears without a refresh. A
message that the operator sends appears at once.

The non-interactive subcommands expose the same operations. They print JSON
and set an exit code. They share the transport, the identity handling, and the
event construction with the TUI. `buzzx channels list` and the channel list
inside `buzzx tui` read the same code.

## Users

**The terminal operator.** A person who works on a headless server, over SSH,
or in a container. They need channel chat without leaving the session they
work in. The TUI is the whole product for them.

**The automation caller.** A script or an agent that must send a message,
read a thread, or react to a request. The machine-readable subcommands and the
exit codes are the whole product for them.

The two users share one code path. This is deliberate. A divergence between
what the TUI can do and what a script can do is a defect, not a convenience.

## Boundaries

`buzzx` does not do these things:

- **It is not a relay.** It stores nothing that it did not receive from the
  relay. Read markers are the only local state, and they are also sent to the
  relay as events.
- **It is not an agent harness.** It does not run agents, schedule tasks, or
  approve work. Agents connect to the relay directly; `buzzx` sees their
  messages the same way it sees everyone else's.
- **It is not a second identity store.** The identity is one Nostr keypair.
  `buzzx` reads it from the environment or a local file. It does not mint,
  rotate, or recover keys.
- **It is not a desktop replacement.** It renders text. It does not render
  attachments, huddles, or voice.
- **It does not add relay API.** `buzzx` uses the relay as it exists: the
  HTTP bridge, the WebSocket, and the event kinds the relay already accepts.
  When `buzzx` needs behavior the relay lacks, the fix belongs in the relay.

## Terminology

These terms have one meaning across all documents.

- `buzzx`: this client.
- relay: the Buzz relay that buzzx connects to.
- identity: one Nostr keypair, and the pubkey derived from it.
- channel: a NIP-29 group. Its id is a UUID, carried in an `h` tag.
- timeline: the ordered messages of one channel, as buzzx renders them.
- row: one rendered timeline entry.
- composer: the text input at the bottom of the TUI.
- bridge: the relay's HTTP surface, `POST /events`, `POST /query`, and
  `POST /count`.
- subscription: one live WebSocket REQ, and the events it delivers.

## First phase

The first phase is the terminal operator's path, end to end:

1. The identity resolves, and the relay is reachable.
2. The channel list fills from the identity's membership.
3. A channel opens and shows its history.
4. A message sends, and it appears in the timeline.
5. A reply, a reaction, an edit, and a delete each work on the identity's own
   messages.

A message that another client sends appears in the timeline during the same
session. This is what makes it a chat client and not a pager.

Typing indicators, presence, read markers, and media come later. They are
real, and they are not required to prove the product.
