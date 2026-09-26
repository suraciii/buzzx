# buzzx Glossary

This glossary defines the shared language for `buzzx`. Every document in this
repository uses these terms with exactly one meaning. A document that needs a
term defines it here and links to this file; it does not restate the
definition.

`buzzx` is a client. It does not own relay, channel, or message semantics. This
glossary therefore describes the Buzz vocabulary that `buzzx` observes, plus
the client-side terms that `buzzx` itself introduces.

## Buzz terms that buzzx observes

**buzz**:
The relay, the identity convention, and the event kinds that a Buzz community
speaks. `buzzx` is one client of a Buzz relay.

**relay**:
The server that holds a Buzz community's events. It accepts signed events over
an HTTP bridge and a Nostr WebSocket, and it validates every event before
storage.

**identity**:
One Nostr keypair, and the public key derived from it. The identity is what
signs events and what the relay authorizes.

**channel**:
A NIP-29 group inside a community. Its identity is a UUID, and every
channel-scoped event carries that UUID in an `h` tag. A channel has a name,
a topic, and a member list.

**community**:
The tenant boundary on a relay. One relay host is one community, and the relay
binds it from the request host. An identity on two relay hosts acts in two
communities.

**event**:
One signed Nostr event: a kind, a public key, a timestamp, tags, and content.
The relay stores the events it accepts and refuses the rest.

**kind**:
The event type number. Kind 9 is a channel message. Kind 7 is a reaction.
Kind 20002 is a typing indicator. The full registry lives in Buzz.

**timeline**:
The ordered messages of one channel, as `buzzx` renders them. This is a
`buzzx` term, not a relay term. The relay stores events; `buzzx` renders a
view of them.

**read marker**:
The kind 30078 event that records how far a client has read. It is encrypted
to the identity's own public key with NIP-44. The relay stores it; the client
writes and reads it. No client computes unread for another client.

**presence**:
The kind 20001 ephemeral event that states an identity is online, away, or
offline. It is never stored: it exists only while it is fresh.

**typing indicator**:
The kind 20002 ephemeral event that states an identity is composing a message
in a channel or a thread. It is never stored.

**Agent**:
An identity that works on someone's behalf. In Buzz an Agent signs its own
events and belongs to one owner who delegates to it. An Agent that posts in a
channel looks like any other member: bot membership or a matching name is not
proof of ownership.

**owner**:
The identity an Agent belongs to. `buzzx` reads the login identity's own
Agent roster and that identity's own observer frames. It never borrows another
identity's ownership, and it never substitutes a delegated identity's owner
for the login.

**observer frame**:
The kind 24200 event an Agent publishes while it works. It carries the owner's
public key in a `p` tag, the Agent's in an `agent` tag, its direction in a
`frame` tag, and a NIP-44 payload encrypted to the owner. The relay delivers a
frame only to the owner's own subscription; `buzzx` keeps the Agent, the turn
and the terminal state, and drops the rest of the payload.

**turn**:
One unit of work an Agent runs. A turn belongs to a conversation when the
frame names one. Frames report a turn starting, staying alive, ending, or
failing. A turn whose frames stop is not known to have ended.

**NIP-42**:
The relay authentication handshake. The relay sends a challenge; the client
replies with a signed event that cites the relay URL and the challenge. No
request is accepted before the handshake completes.

**NIP-98**:
The HTTP authentication scheme. The client signs a kind 27235 event that binds
the method, the URL, and optionally the request body, and sends it in an
`Authorization` header.

**NIP-OA**:
The owner-delegation convention. An agent identity carries an `auth` tag that
names its owner and the conditions of its delegation. Buzz accepts the tag on
both transports.

**Blossom**:
The media protocol. An upload is authorized by a kind 24242 event, and a
download is authorized by an event in the query.

## buzzx terms

**buzzx**:
Buzz's extension CLI (`buzz ext`), with a TUI and non-interactive commands.
Its product boundary is defined in [core.md](core.md).

**core**:
The relay operations that both the TUI and the CLI perform. One call per
operation, no state between calls, and no rendering, wording, or exit code:
each of them translates the result into what it shows. `src/client.rs` is the
core; see [design/shared-core.md](design/shared-core.md).

**CLI**:
The non-interactive way to drive `buzzx`: one command per run, one JSON value
on stdout, and an exit code. The TUI is the interactive one. The first CLI
slice is [docs/browse-collab-cli.md](docs/browse-collab-cli.md).

**terminal user**:
The person `buzzx` is built for. Someone who already uses Buzz - on the desktop
app or a phone - and who spends their working hours in a terminal. This is the
primary user. The automation caller is secondary. Do not call this person the
"operator": `buzzx` operates nothing, and the word frames the product as a tool
for running a relay rather than a client for a person.

**automation caller**:
A script or an agent that invokes buzzx commands and consumes their structured
results. It may perform one-shot collaboration or continuous observation.

**row**:
One rendered timeline entry. A row is built from one event. Kinds that overlay
other events never produce a row of their own.

**composer**:
The text input at the bottom of the TUI. It holds the message being written,
the reply target, and the edit target.

**reply target**:
The event that the next sent message answers. It is cleared by every send.

**edit target**:
The event that the next sent message replaces. It is cleared by every send.
The reply target and the edit target are mutually exclusive.

**session**:
One run of `buzzx`: one identity, one relay, one WebSocket connection, and
the state that both pumps share.

**bridge**:
The relay's HTTP surface. `buzzx` uses `POST /query` to read and
`POST /events` to write. See the relay design document for the full list.

**subscription**:
One live WebSocket REQ request and the events it delivers until it is closed
or the connection drops.

**pump**:
The tokio task that owns one channel of the transport. There is one WebSocket
pump and one command pump. See `design/architecture.md`.

## Terms buzzx avoids

Do not use these words for a `buzzx` concept:

- **message** for a row. A row may come from a diff, a post, a comment, or a
  system event. Use "row", or name the kind.
- **channel id** for a kind number. A channel id is a UUID; a kind is an
  integer.
- **server** for the relay. The relay is the server.
- **session** for a channel view. A session is the whole run; a channel view
  is the selection.
