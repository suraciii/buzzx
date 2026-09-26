# Using `buzzx tui`

`buzzx tui` opens a live terminal chat session against one Buzz relay.

Start it:

```text literal
export BUZZ_PRIVATE_KEY=<hex-or-nsec>
buzzx tui
```

The relay defaults to `http://localhost:3000`. See
[configuration.md](configuration.md) for other ways to set identity and relay.

## Layout

The client picks its layout from the terminal's size and never infers a device
type. The selected conversation, focused row, draft, and reply target survive
a resize. Read progress is tracked per conversation and shared through the
relay; the limits are described below.

| Terminal | Layout | What it shows |
| --- | --- | --- |
| At least 80 columns and 12 rows | Wide | Inbox sidebar, timeline, and composer. |
| At least 40 columns and 10 rows, but not wide | Narrow | One-column timeline; `c` opens the full-screen conversation picker. |
| At least 24 columns and 6 rows, but neither above | Minimal | One-column timeline with compact rows and a one-line composer while composing. |
| Below 24 columns or 6 rows | Too small | A size message. Resize the terminal; the session continues. |

### Wide

```text diagram
| Inbox: All | #general                                     |
| Channels   |                                             |
| 1 @ 2 eng  | alice         2m                            |
| 2 * 3 rnd  | the migration is on main                    |
| DMs        |                                             |
| 3 * 1 Sam  | bob           1m                            |
|            | @tyler can you look                         |
|            | +2 reactions                                |
|            | Agent A typing...                            |
|            +---------------------------------------------+
|            | reply to bob - enter to send, esc to clear  |
+-----------+---------------------------------------------+
 status: connected https://relay.example | sent | mode: nav | ?=help
```

The wide sidebar lists conversations under `Channels` and `DMs`. The selected
conversation's timeline is on the right. The typing line appears above the
composer while another identity is composing, and takes no row when nobody is.
The row signals and filtering rules are below.

In narrow and minimal modes the list is not shown beside the timeline. Press
`c` to open the full-screen conversation picker. It uses the same Inbox
sections and filters as the wide sidebar; `j` and `k` move its cursor, `Enter`
opens the conversation, and `Esc` closes it. Filtering is available at every
layout size. Message bodies wrap to the available width. Long words and URLs
break at character boundaries; the message itself is unchanged.

### Narrow and minimal

One column: the timeline, with the conversation and connection state on the top
line and the composer and a status hint at the bottom. The full-screen
conversation picker is opened with `c`. In minimal mode the timeline uses
compact rows and the composer takes one line while it is being used.

A message body wraps to the column's width. Long words and URLs break at
character boundaries; the message itself is unchanged.

### Too small

Below 24 columns or 6 rows the client shows the size it needs and the size it
has, and keeps running. Resizing the terminal brings the layout back with the
selection, the draft, and the reply target intact.

The session always exposes one focused row in the timeline. Scrolling moves the
focus with the viewport and keeps the focused row visible. Actions that target a
row use this focus, so a reply, reaction, edit, or delete never acts on an
unrelated message at the bottom of the timeline. When a channel opens, focus is
the newest loaded row. If there are no rows, `Enter` opens a new-message
composer without a reply target.

## Inbox and read state

The conversation list has `Channels` and `DMs` sections. Hidden DMs and
archived channels are excluded when the relay's visibility and metadata queries
are available. DM labels use their own non-generic name; generic DM names use
the other participants' display names, falling back to shortened public keys.
At most three names are printed, followed by `+N more`. New messages do not
reorder existing rows. (Sources: `src/client.rs::ChannelInfo::listed`,
`channels_from`; `src/app.rs::merge_channels`, `label`, `refresh_views`.)

Each conversation reserves five columns for its signal before the label is
clipped. The unread count is the number of unread message candidates, not a
total-history count. The `…` typing marker follows the label and can appear
with any signal. (Sources: `src/ui.rs::conversation_item`;
`src/app.rs::marker`, `unread_count`, `label`.)

| Signal | Meaning |
| --- | --- |
| `● 3` | Three unread messages, with no unread direct mention |
| `@ 2` | Two unread messages, at least one of which mentions this identity |
| `?` | Read state or catch-up is unknown |
| `Read` | A picker row retained after it stops matching the active filter |
| `…` | Someone is typing; this is independent of the unread signal |
| two spaces | No signal |


Unread messages clear only after the newest loaded message has actually been
presented: its conversation is selected, history is loaded, focus is at the
bottom, and neither the picker nor help covers the timeline. Opening a
conversation or reading older history does not clear unread messages. When a
saved marker is available, the next session reads it from the relay and
continues from that frontier. A conversation with no marker starts at the
newest message as a local baseline; the seed is never uploaded. A message in
the frontier's own second stays unread until shown. The claim is re-made when
the answer that makes it possible lands — the marker, the catch-up, or the
seed — so a conversation that was already on screen before the relay answered
is not left unclaimed. (Sources:
`src/app.rs::note_presented`, `apply_read_state`, `apply_catch_up`,
`apply_seed`, `ReadTrack::unread_at`;
`src/client.rs::CatchUp::Newest`, `read_state`; `src/session.rs::load_read_state`.)

Read state is a NIP-44 encrypted kind 30078 event with one replaceable `buzzx`
slot. Its `d` value is `read-state:` plus the hex encoding of the first 16
bytes of SHA-256 over the public-key hex string; the `t` tag is `read-state`.
The payload written by this client has `v: 1`, `client_id: "buzzx"`, and
`contexts` keyed by channel UUID, each value a Unix-second frontier. The parser
also accepts a missing `v` as version 1 and requires a `client_id` string of
at most 64 bytes. Before writing, the client reads the identity's slots and
merges each context by its maximum. (Sources: `src/read_state.rs::slot`,
`builder`, `parse`; `src/client.rs::read_state`, `publish_read_state`.)

A failed marker lookup or unreadable slot is unknown, not read. Failed or
truncated catch-up is also unknown; the list can show `?` or `Checking...`
frontier stays advanced and the list shows `Read here; not synced (<reason>)`.
This note remains until a later publish succeeds. The session keeps working
and does not retry the failed publish automatically; a later read advance can
issue a new publish. If the failed read was not stored, a later session can
show those messages as unread again. (Sources: `src/app.rs::apply_read_state`,
`apply_catch_up`, `inbox_footer`; `src/session.rs::load_read_state`,
`publish_read`, `run_command_pump`.)


## Keys

Keys have two modes. The timeline starts in navigation mode.

In navigation mode:
- `j` or the down arrow moves to the next conversation in the wide layout. In
  one-column layouts, it moves through timeline rows; use `c` to open the
  conversation picker there.
- `k` or the up arrow moves to the previous conversation or timeline row.
- `1` through `9` jump to the conversation with that session shortcut.
- `f` cycles `All`, `Unread`, and `For you`. In the picker, `f` or Tab cycles
  the same filters.
- `c` opens the conversation picker. `j` and `k` move within it, `Enter` opens
  the highlighted conversation, and `Esc` closes it.
- `g` or `Home` focuses the oldest loaded message.
- `G` or `End` focuses the newest message.
- `PgUp` and `PgDn` move ten rows.
- `i` or `Tab` opens the composer for a new message.
- `Enter` opens the composer with the focused row as the reply target. The
  reply target is shown in the composer and can be cleared with `Esc` before
  sending.
- `r` reacts with the default thumbs-up emoji to the focused row. Pressing it
  again on your own reaction removes it.
- `e` edits the identity's own focused row.
- `d` deletes the identity's own focused row.
- `?` toggles the key help, and `Esc` closes it.
- `q` quits.

In composer mode every printable character, `j`, `k`, `c`, `i`, `r`, and `?`
included, is typed into the draft. The composer reserves `Enter` (send),
`Alt+Enter` (newline), the cursor keys, `Home`, `End`, `Backspace`, and `Esc`
(clear the reply or edit target, then leave the composer with the draft kept).

The composer has no external editor integration in the first phase. A long
message is typed in an editor and sent with
`buzz messages send --channel <uuid> --file <path>`, which is the same relay
write the composer performs.

## What the timeline shows

Each message is two or more lines: the author and the age, then the body, then
reactions. Messages are grouped by the presence or absence of a thread
reference. A reply carries a marker naming what it answers.

A row that carries the identity's own `p` tag is highlighted. A broadcast
reply carries an `@channel` marker. An attachment shows the `imeta` tag's
filename and a marker; the body keeps its markdown line.

A pending send renders dimmed with a `...` marker until the relay answers. If
the relay refuses the send, the row disappears, the composer text returns, and
the status line shows the relay's reason.

## Typing indicators

When another identity is composing in the selected channel, one dim line
appears between the timeline and the composer. It shows a display name per
identity, at most two, then `+N` for the rest:

```text literal
Agent A typing…
Agent A, Agent B are typing…
Agent A, Agent B +2 are typing…
```

The wide layout writes the names as they are. The one-column layouts use the
compact `@` form, `@Agent A typing…`. When nobody is composing, the line is
gone and the timeline gets the row back.

Channels where someone is composing carry a `…` after the name in the channel
list, and the picker shows the same marker, so activity in a channel you are
not reading is still visible. The marker is not an unread count and it does not
reorder the list.

Indicators are ephemeral: they are never part of the history, they are not
counted as unread, and they do not survive a restart. One expires 8 seconds
after the last repeat from its author, and it ends immediately when that
author's message arrives, when the connection drops (the status line shows
`reconnecting`), or when the channel closes or leaves your channel list. If the
relay refuses a channel's typing feed, the status line says so and the rest of
the client keeps working. Another client signed in with the same identity does
not show up as a typist in your session.

Two limits are worth knowing. Typing is shown per channel, not per thread, so a
reply in progress looks the same as a new message in progress. And the line
reports typing only: whether an agent has accepted a request, is running a
tool, or has failed needs its own status event from the agent's runtime, which
does not exist yet.

## Status line

The bottom line shows three things: the connection state, the relay's answer to
the last write, and the mode. The connection state is one of `connecting`,
`connected`, `reconnecting`, or `failed`.

## What `buzzx tui` does not do
This TUI does not render images, video, or audio. Attachments show as filename
lines. It does not join huddles or search; voice needs the relay's separate
audio WebSocket, and search is available through the `buzz` CLI. It does not
configure the relay, manage members, or moderate. DMs appear as first-class
rows in the Inbox. Read progress is shared through encrypted relay markers,
but a marker lookup or publish can fail; see
[Inbox and read state](#inbox-and-read-state) for the limits.

The TUI does not publish a typing indicator of its own. It shows other
identities composing; publishing your own is a later phase, and the event
contract on the relay already accepts it.

## Terminal requirements

The TUI needs a terminal that reports a size and supports the alternate
screen. It runs at 24 columns by 6 rows and larger, in the layout the size
allows. Below that it shows the required size instead of a broken layout, and
it switches to a layout as soon as the terminal grows.

Raw mode is entered at start and restored at exit, including on error. A
terminal that is left in raw mode after a crash is a bug.


## Agents overview

`a` opens this view. It answers which Agents the signed-in user owns, which
have observed active work, and which accessible conversation to open. It does
not add an Agent management console. The boundary is in
[decision 0006](../design/decisions/0006-agent-summary.md).

### Entry and navigation

In navigation mode, `a` opens an Agents overlay; Esc returns to the prior
conversation with its focus, draft and reply target intact. In composer mode
`a` remains text. Opening the overlay does not mark messages read.

Use one stable list, with name and status per Agent. No Working filter,
search, sorting settings, purpose editor or new top-level navigation system.
Order by display name and public key on load; live updates never move focus.
Keep names stable in position until the overlay is reopened. The selected
item can reveal its full name and short public key to distinguish duplicates.

`j/k` select an Agent; Enter opens its detail. Detail contains the status,
last signal age and a list of observed working contexts. `j/k` select a
context and Enter opens its channel. Esc goes back one level. A context with
no accessible channel is not actionable. Opening a channel does not send,
create a DM, add members, or change ownership. With no known work, detail
says so and offers Back, not an invented destination.

```text diagram
Agents                         Agent A
> Agent A       Working        Last signal: 3s ago
  Agent B       Typing         Contexts:
  Agent C       Unknown        > #engineering
                                 #support
Enter details   Esc back       Enter channel   Esc back
```

At 80 by 12 or larger, list and selected detail may share the overlay. At
40 by 10, 79 by 12 and 24 by 6, use full-screen list then detail, not squeezed
columns. Reserve space for status and a key hint before clipping names;
detail can scroll long labels. Resize and returning from a channel preserve
the selected Agent identity. If it is removed, choose the next surviving
item or the previous item at the end. An empty list retains the exit key.

### Whose Agents

My Agents is the current login identity's verified owned roster, excluding
archived entries. Channel bot membership or a matching name is not proof of
ownership. Do not substitute a delegated identity's owner for the login or
borrow another client's private key. Unsupported identity access says
`Agent overview unavailable for this identity`; chat remains usable.

A complete empty query says `No agents`. Failed, partial and unverified
results remain distinguishable: `Cannot load agents`, `List incomplete`,
or `Ownership unverified`. Unverified records cannot be actionable owned
Agents. Identity or relay changes clear the roster and private summary state.

### Status meaning

- `Working`: a fresh, authenticated active-turn signal exists. Aggregate
  independent turns per Agent; ending one does not end its other turns.
- `Typing`: only channel typing is observed. This is not evidence of tools
  running or a request being accepted. It does not override known Working.
- `No active turn observed`: the current observation has no active turn.
  This does not promise that the Agent is idle or has no background work.
- `Unknown`: observation is not established, disconnected, stale or unreadable.
  Previously observed work can be described as last seen, never current.

Online presence is not a work signal. Do not show a separate presence column,
running duration, global running total, percentage, history or notification.
Detail shows last received signal age so the evidence is inspectable.
An explicit failed terminal signal may show `Last observed turn failed`
for the current session, alongside any remaining work; no automatic retry.
Ordinary messages do not finish a turn. Late stale liveness cannot revive a
completed turn. Typing keeps its existing expiry independently of work.

During a missing stream, do not infer success or failure. Reconnection starts
with unknown state until fresh evidence arrives. Late join must accept a
valid active-turn liveness signal without requiring a previously seen start;
when that source cannot recover activity, remain Unknown. Implementation must
use a bounded freshness rule grounded in the deployed producer's heartbeat
cadence, document it, and test the exact expiry boundary. No new setting or
protocol is added to tune that rule.

Only accessible channel names and destinations appear. For a private context
outside the user's membership, show `Unavailable context` without its name,
id or content. If only a channel is known, label the action `Open channel`;
only a verified message reference permits exact message navigation.

### Acceptance

A roster-only or typing-only delivery does not fulfill the working-state goal.

1. Two owners, same-name Agents and an archived Agent produce the correct
   owned list without cross-owner disclosure. Empty, partial, failed and
   unverified roster states remain distinct.
2. A turn running a long tool without typing remains Working with fresh
   liveness; typing alone remains Typing. Mere online presence proves neither.
3. Two simultaneous turns survive one completion or failure independently.
   A progress message does not end work; terminal events do.
4. Late join, duplicate and out-of-order events, producer restart, missing
   terminal, stale heartbeat and reconnect never leave a permanent false busy
   badge or invent success. Test the documented freshness threshold.
5. All four terminal sizes support Agent selection, detail, channel entry and
   return without changing drafts, reply targets or read state merely by
   opening the overlay. Revoked access removes inaccessible destinations.
6. No private observer payload, tool argument, secret or transcript is rendered,
   logged, persisted or published as an ordinary channel message.

Creation, configuration, stop/restart, tool transcripts, Waiting for you,
recent results, task completion, cost and model switching are excluded. Add
none of them as placeholders or extension points in this slice.

### Verification

The relay rules this view depends on were checked against the deployed relay
`https://buzz.surac.cloud` with a delegated Agent identity: the identity that
would receive the frames is the identity that made the request. Both the
roster read and the observer feed answered, and both stayed inside the owner
boundary:

- `POST /query` for kind 30177 authored by the identity answered HTTP 200 with
  an empty page. An empty roster is an answer, not a refusal.
- `REQ` for kind 24200 with `#p` set to the identity's own pubkey was answered
  with `EOSE` and never with `CLOSED`, and delivered no frame: the
  subscription is legal, live-only, and quiet.
- The same `REQ` naming another identity was closed with `restricted: p-gated
  events require #p matching your pubkey`. A client can only subscribe to its
  own observer feed, which is what lets a frame be read as evidence about the
  Agents this identity owns and no others.
- A kind-24200 telemetry frame signed by a key this identity does not own was
  refused at the WebSocket: `invalid: event pubkey does not match
  authenticated identity`. A frame cannot be planted by a client that is not
  the Agent it claims to be, and the owner/agent check sits behind that.

Not verified: no identity here owns a running Agent, so a live `Working`
signal was not observed on the deployed relay. The states driven by frames are
covered by the module and render tests, which use the deployed producer's
field names (`kind`, `turnId`, `channelId`, `seq`) and its `batch` envelope.
Confirm them once against a deployment where the signed-in identity owns a
running Agent. `Unknown` is the answer for a feed that is refused, closed, or
not yet established; `No active turn observed` follows only a feed the relay
has answered with `EOSE`.
