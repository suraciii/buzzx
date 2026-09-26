# buzzx Core

This document defines what `buzzx` is, who it is for, and what it does not do.
It is the product contract. [design/](design/) documents say how the contract
is implemented.

## The problem

Buzz users read and write channels in three places: a Tauri desktop app, a
Flutter mobile app, and the `buzz` CLI. Two of them need a graphical session or
a phone. The third is scriptable: it answers one question per invocation and
returns JSON, and it does not hold a subscription open, so it cannot show a
channel as it changes.

Buzz users who live in a terminal therefore have no client of their own. They
can run `buzz` for one-off reads and writes, but they cannot sit in a terminal
and have a conversation. A headless or terminal-only workstation is the sharpest
case of this, and it is not the only one: an SSH session, a container, a plain
console all lack an interactive client.

`buzzx` is that client. It is to Buzz what the mobile app is — a client for
people, not a tool for relays.

The existing clients do not do this job:

- The desktop app is a Tauri application. It needs a graphical session and a
  native window.
- The mobile app is Flutter. It needs a phone.
- The `buzz` CLI is scriptable, and that is its whole shape: one invocation,
  one answer, no session.

## The product

`buzzx` is Buzz's extension CLI (`buzz ext`). It serves terminal users through
an interactive TUI and scripts or agents through non-interactive commands.
Both use the same relay, identity, authorization, and event semantics.

Where the desktop app and the mobile app are Buzz's clients for people with a
display, `buzzx` is Buzz's client for people in a terminal. Same product,
fourth surface. It does not replace any of them, and it is not a tool for
operating a relay.

`buzzx tui` opens a session with three regions:

1. A channel list on the left. It shows the channels the identity belongs to.
2. A timeline on the right. It shows the messages of the selected channel.
3. A composer at the bottom. It sends, replies, reacts, edits, and deletes.

The channel list groups channels and direct conversations and carries a
lightweight Inbox view. Unread is computed from a read marker (kind 30078)
plus events observed while a conversation is not selected. If either lookup
is incomplete, the client shows an unknown/checking state rather than claiming
that everything is read.

The session is live. A message that arrives appears without a refresh. A
message that the user sends appears at once.

The non-interactive commands print JSON and set an exit code. Operations shared
with the TUI use the same internal logic; see
[design/shared-core.md](design/shared-core.md). The first collaboration slice
is specified in [docs/browse-collab-cli.md](docs/browse-collab-cli.md).

The standalone `buzz` CLI has limits and its development is outside this
project's scope. `buzzx` may provide one-shot browsing, collaboration, and
Agent management capabilities even where `buzz` offers a similar command.
It reuses Buzz semantics without requiring callers to switch to `buzz`.
Agent management is a future slice, not part of the current collaboration MVP.
The extension boundary does not make buzzx a relay or an agent harness.

## Users

**The terminal user**, primary. A person who already uses Buzz — on the desktop
app, on a phone — and who spends their working hours in a terminal. Over SSH,
in a container, on a console. They want the same channels they already have,
from where they already are. They can use the TUI for a conversation or the
CLI for a single operation.

**The automation caller**, secondary. A script or an agent that uses the CLI
to discover channels, read messages and threads, send or reply, and inspect
the result. Stable JSON and explicit write outcomes let it compose these
operations. Continuous observation is a separate `watch` capability.

The terminal user is the one whose workflow gets designed first. `buzzx` is a
client for people before it is an interface for programs, and when the two
disagree the person wins.

## Boundaries

`buzzx` does not do these things:

- **It is not a relay.** It stores nothing that it did not receive from the
  relay. Read markers are the only local state, and they are also sent to the
  relay as events.
- **It is not an agent harness.** It does not run agents, schedule tasks, or
  approve work. Agents connect to the relay directly; `buzzx` sees their
  messages the same way it sees everyone else's.
- **It is not an Agent control console.** The planned owner-only Agents
  overview may read a roster and minimal runtime summaries. It does not
  expose transcripts, tool arguments, configuration or control operations.
  See [decision 0006](design/decisions/0006-agent-summary.md) for the narrow
  exception to the former owner-plane exclusion.
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

The first phase is the terminal user's path, end to end:

1. The identity resolves, and the relay is reachable.
2. The channel list fills from the identity's membership.
3. A channel opens and shows its history.
4. A message sends, and it appears in the timeline.
5. A reply, a reaction, an edit, and a delete each work on the identity's own
   messages.

A message that another client sends appears in the timeline during the same
session. This is what makes it a chat client and not a pager.

A typing indicator (kind 20002) from another identity appears as one line
between the timeline and the composer, and expires on its own. buzzx consumes
indicators; it does not publish its own yet.

Presence and media come later. Read markers and the Inbox are part of the
current terminal workflow; they track reading, not task completion.
