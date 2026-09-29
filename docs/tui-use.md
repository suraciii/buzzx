# Using `buzzx tui`

`buzzx tui` opens a live terminal chat session against one active Buzz
community. With more than one saved profile the current community is shown
beside the channel; `C` opens the saved-profile picker and switches the single
active relay connection in place.

The [shared interactive contract](interactive.md) maps these capabilities to
both TUI and Web. This manual owns terminal controls; its linked functional
rules remain shared where the catalog names them.

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
| At least 80 columns and 12 rows | Wide | Full-width timeline, composer, and status line. |
| At least 40 columns and 10 rows, but not wide | Narrow | The same timeline; `c` opens the full-screen conversation switcher. |
| At least 24 columns and 6 rows, but neither above | Minimal | The same timeline with compact typing text and a one-line composer while composing. |
| Below 24 columns or 6 rows | Too small | A size message. Resize the terminal; the session continues. |

### Timeline first

One column is the base surface at every size: the header names the open
conversation and the Inbox answer, the timeline takes the rest of the width,
and the composer, hint and status line sit at the bottom. Nothing is drawn
beside the timeline, so a 80-column terminal reads a message across the whole
80 columns instead of a 58-column remainder.

```text diagram
#general                                                            Inbox read
> alice       2m
  the migration is on main

  bob         1m
  @tyler can you look
  +2 reactions

j/k move  c switch  Enter reply  ? help
status: connected https://relay.example | sent | mode: nav | ?=help
```

The header's Inbox segment is `Inbox read` when nothing is unread anywhere,
`Inbox ● N` with the unread messages outside the open conversation,
`Inbox @ N` when any of them mentions this identity, and `Inbox ?` when the
answer is not known yet — a missing roster, an unread marker lookup that has
not answered, or a catch-up that failed. Narrow and minimal add the connection
state to the header; the wide header leaves it to the status line. Below 36
columns the segment is dropped rather than clipping the conversation name.

### Wide

The wide layout keeps the timeline's structure and spends its extra width on
density: messages are separated by a rule, and the header's connection state
moves to the status line. Its keys are the same ones every other layout uses.

### Narrow and minimal

One column: the timeline, with the conversation, connection state and Inbox
answer on the top line, and the composer and a status hint at the bottom. The
full-screen conversation switcher is opened with `c`. In minimal mode the
typing text is compact and the composer takes one line while it is being used.

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
| `Read` | A switcher row retained after it stops matching the active filter |
| `…` | Someone is typing; this is independent of the unread signal |
| two spaces | No signal |


Unread messages clear only after the newest loaded message has actually been
presented: its conversation is selected, history is loaded, focus is at the
bottom, and neither the switcher nor help covers the timeline. Opening a
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
truncated catch-up is also unknown, and the list says `?` or `Checking...`
rather than an empty answer. A roster the relay never reported complete is
unknown the same way: an empty list it did not vouch for is not
`No conversations`, and the footer says the listing may be missing rows. The
header counts what waits outside the conversation that is open; it answers
`Inbox ?` while any of those conversations is unknown, and an empty roster the
relay called complete reads as `Inbox read`.
A failed publish leaves the frontier advanced and the list shows `Read here;
not synced (<reason>)`. This note remains until a later publish succeeds. The
session keeps working and does not retry the failed publish automatically; a
later read advance can issue a new publish. If the failed read was not stored,
a later session can show those messages as unread again. (Sources:
`src/app.rs::apply_read_state`, `apply_catch_up`, `empty_view`, `inbox_summary`,
`inbox_footer`; `src/session.rs::load_read_state`, `publish_read`,
`run_command_pump`.)


## Keys

Keys have two modes. The timeline starts in navigation mode.

In navigation mode:
- `j` or the down arrow moves to the next timeline row.
- `k` or the up arrow moves to the previous timeline row.
- `1` through `9` jump to the conversation with that session shortcut. Inside
  the switcher the digits are filter text instead.
- `f` cycles `All`, `Unread`, and `For you`. In the switcher, `f` or Tab cycles
  the same filters.
- `c` opens the conversation switcher at every layout size. The section
  below owns its keys.
- `C` opens the saved-community selector. `j` and `k` move between saved
  profiles, `Enter` switches the active relay, and `Esc` closes it. Add,
  rename and remove profiles with `buzzx community`.
- `Ctrl+P` opens the command palette.
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

### Conversation switcher

`c` opens the same full-screen switcher at every size; `c` or `Esc` closes it
without opening anything. It is the only conversation list the client draws,
and it never advances a read marker: previewing it and opening a conversation
from it are different acts, and only the conversation that ends up on screen
with its newest loaded message focused claims its read state.

```text diagram
Switch conversation  name: rel
All  Unread  For you
  Channels
> 3      release
  2      general
  DMs
  4      Direct Person
j/k move  type to filter  enter open  esc back  ? help
```

- `j`/`k` or the arrows move the cursor; `Enter` opens the conversation the
  cursor is on and closes the list.
- Typing filters by name against the labels the list shows. Matches rank
  exact, then prefix, then substring; equal ranks keep section order (Channels
  before DMs) and then roster order. The list keeps its sections, while every
  keystroke puts the cursor on the best match in either of them: an exact name
  wins over a longer name that merely contains the query, so typing a name in
  full and pressing `Enter` opens it. A cleared query leaves the cursor where
  it is, and a query that matches nothing says `No match`, opens nothing, and
  keeps the cursor too.
- `Backspace` edits the query; `/` resumes it without adding a slash; `Esc`
  cancels the query and returns the list to the applied one, and `Esc` again
  closes the list.
- `f` or `Tab` cycles the filter; the cursor moves to the first conversation
  the new filter shows when the row it was on is hidden by it.
- The digits are filter text here, not session shortcuts: every printable key
  belongs to the query, so a name that starts with a digit (a hex id) is
  reachable like any other. The `1`–`9` shortcuts apply on the timeline.
- A conversation whose state is unknown shows `?`, and one retained after it
  stops matching the active filter shows `Read`; the header summary says
  `Inbox ?` while any answer is still missing. The list is not a placement
  surface: it never joins, leaves, or creates a conversation.

### Commands

`Ctrl+P` opens a command palette over the timeline: one fixed list of the
actions the keys already reach, each showing the key that reaches it. It owns
no state of its own.

| Command | Key | What it does |
| --- | --- | --- |
| Switch conversation | `c` | Opens the conversation switcher. |
| Search messages | `/` | Opens message search. |
| My agents | `a` | Opens the Agents overlay. |
| Open thread | `t` | Opens the focused event's thread. |
| Help | `?` | Opens help. |
| Status | none | Displays the current relay, connection, membership and draft target. |
| Quit | `q` | Ends the session. |

`j`/`k` move the cursor, `Enter` runs the highlighted command through the same
handler its key uses, and `Esc` closes the palette without running anything. A
second `Ctrl+P` closes it too, and `Ctrl+F` opens search from it, the command
the list offers.
The palette, the switcher, the Agents overlay and help never share the screen:
opening one closes the others. It opens only over the channel timeline itself:
with the switcher, help or the Agents list open, `Ctrl+F` and `Ctrl+P` do
nothing and the overlay keeps its keys; the under-minimum size message answers
`q` alone; and inside a thread, search, context, or the reader the destination
is protected until the user returns.

### Slash commands in the composer

Only a draft whose first character is `/` can run a command. Navigation `/` still opens message search, and `/` inside ordinary text stays text. Type `/` for suggestions, use the arrow keys to select one, Tab to complete its name, and Enter to execute a complete name. Esc closes the suggestion list without deleting the draft. A prefix such as `/sea` or an unknown name such as `/foo` still sends as ordinary text; a known command with invalid arguments keeps the draft and shows usage. `//search old decision` sends `/search old decision` literally. Multi-line drafts remain messages.

| Command | Effect |
| --- | --- |
| `/help [command]` | Open help, optionally focusing a command's usage and surfaces. |
| `/search [query]` | Open message search with an optional prefilled query, or return to the search this surface came from. |
| `/switch [name]` | Open the local conversation switcher, optionally filtering by name. |
| `/thread` | Open the confirmed focused event's thread; the composer target survives the visit. |
| `/agents` | Open the Agents overlay. |
| `/status` | Show relay, community, connection, membership, and composer target. |

Search runs from channel, context, and thread composers, thread from channel and context, and switch and agents only from a channel composer; a surface that cannot run a command keeps the draft and reports it. The suggestion list offers only the commands the current surface can run, and `/help <command>` names the surfaces a command has and whether this one is among them.
Success consumes the command line and keeps the composer's target: `/thread` puts that target back when the thread is left, and `/search` on a surface that was opened from a search returns to that search with its results, filters, and drafts while the layers above it keep their own. These are navigation and read-only commands: none sends a message or changes a read marker merely by opening a list. The Web send control continues to publish its input literally, including leading `/`; slash parsing belongs only to the TUI composer Enter path, and the one-shot CLI remains explicit subcommands.
### Message search and context (M2)

`/` in channel or thread navigation opens full-screen message search, scoped
to the current conversation first. `Ctrl+f` opens it from the composer with
the draft, target, and mode kept for return; a pending write blocks that entry.

Search has three editing surfaces. While the query is being edited, every
printable character is query text: `Enter` submits, `Esc` restores the applied
query. In result navigation:

- `j`/`k` or arrows move the selection; the selection is an event id, so a
  replaced result list keeps it when the same hit is still there.
- `Enter` opens the exact hit in a fullscreen context view, or retries the
  kept query and filters after a failure.
- `/` edits the query again and `f` opens the filter form.
- Filters apply only on explicit submission. Results keep the relay's
  relevance order and stay stable until then; up to 50 results are requested.
- A failed read preserves the previous results, labeled `previous query`;
  a stale response never replaces a newer list. Unlisted or deleted hits are
  hidden and the remaining list is labeled bounded.
- `Esc` returns to the origin; `?` opens help and `q` quits.

The filter form (`f`, replacing the results while open) has one control per
row for scope, author and time:

- `j`/`k` move between the controls; `h`/`l` adjust the focused one; `s`
  cycles the scope, `t` the time range (all time, 7 days, 30 days, relative
  to the submission), and `Enter` applies the form and submits.
- Scope is the current conversation, all accessible listed conversations, or
  one chosen conversation (`o` opens the conversation selector).
- `a` opens the author picker over known, accessible participant profiles.
  The list may be incomplete and says so; duplicate names carry short public
  keys. `p` types an exact 64-character public key instead. Identity is the
  full public key, never a display name.
- An author alone can be searched without a keyword; an empty submit asks
  for one instead of loading the entire archive. `Esc` cancels the form and
  restores the applied filters.

The context view reads the hit plus up to 20 rows on each side and focuses the
hit itself. `j`/`k` move, `g`/`G` and `PgUp`/`PgDn` jump and page, `v` opens
the full-message reader, `t` opens the focused row's thread (Esc returns to
this context), `Enter` composes a reply under the normal draft guards, `[` and
`]` load one older or newer page, and `Esc` returns to the same search result.
Neither view advances channel read progress.

### Full-message reader (M1)

In navigation mode, `v` opens the focused confirmed message in a full-screen
reader. The reader binds to the event id, not its timeline index:

- `j`/`k` or the arrow keys scroll one displayed line.
- `PgUp`/`PgDn` scroll a page; `g`/Home and `G`/End go to the start and end.
- `Enter` starts a reply to the displayed message. If a nonempty draft already
  exists, the reader stays open and says `Draft kept; Esc back`.
- `Esc` or `v` returns to the channel or thread with its prior focus. `?` opens
  help and `q` quits.

The reader preserves blank lines, indentation, tabs, URLs, Unicode text,
attachments, reactions, and the source-line anchor across terminal resizes. It
does not advance channel read progress. Pending and uncertain local rows are
not readable; edits reset to the start with `Updated; at start`, and deletion
shows `Message deleted` and disables reply. `v` remains literal text in composer
mode.

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

Channels where someone is composing carry a `…` after the name in the
switcher's row, so activity in a channel you are not reading is still
visible. The marker is not an unread count and it does not
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
lines. It does not join huddles; voice needs the relay's separate audio
WebSocket. It does not
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
boundary. The roster read was made with the identity that signed the
deployment's three agent records too, read-only: one `POST /query`, one
observer `REQ` answered with `EOSE`, and nothing published. The records that
came back were then fed through this module and the overlay renderer.

- `POST /query` for kind 30177 authored by the identity answered HTTP 200 and
  delivered only records that identity had signed. On a delegated Agent
  identity with no Agent the page was empty, and an empty roster is an answer,
  not a refusal. On the deployment's owner identity the same read answered the
  three records that identity owns: each one verified, each carried its Agent
  key in the `d` tag and its name in the record content, and the classifier
  read them as `Bumble`, `Fizz` and `Honey` sorted by name, with nothing
  counted as foreign, unverified or unreadable.
- The overlay rendered those three records as `Agents (3)`, one row per Agent,
  each with `No active turn observed`, at the full width and at 40x10.
- `REQ` for kind 24200 with `#p` set to the identity's own pubkey was answered
  with `EOSE` and never with `CLOSED`, and delivered no frame: the
  subscription is legal, live-only, and quiet. The owner identity's feed
  behaved the same way in a twenty-second window, which is what makes `No
  active turn observed` the tested state rather than an assumed one.
- The same `REQ` naming another identity was closed with `restricted: p-gated
  events require #p matching your pubkey`. A client can only subscribe to its
  own observer feed, which is what lets a frame be read as evidence about the
  Agents this identity owns and no others.
- A kind-24200 telemetry frame signed by a key this identity does not own was
  refused at the WebSocket: `invalid: event pubkey does not match
  authenticated identity`. A frame cannot be planted by a client that is not
  the Agent it claims to be, and the owner/agent check sits behind that.

Not verified: no Agent owned by an identity reachable from this workspace had
a turn running while the feed was open, so a live `Working` signal was not
observed on the deployed relay. The states driven by frames are covered by the
module and render tests, which use the deployed producer's field names
(`kind`, `turnId`, `channelId`, `seq`) and its `batch` envelope. Confirm them
once against a deployment where an owned Agent has a turn running. `Unknown`
is the answer for a feed that is refused, closed, or not yet established; `No
active turn observed` follows only a feed the relay has answered with `EOSE`.


## Mentions when sending

[Mention suggestions and channel creation](mentions-and-channel-creation.md)
is the product contract this follows.

`@` at the start of a word opens a draft-local picker over the open
conversation's members: exact names first, then prefix matches, with the Agent or
admin role and a short key only on rows that would otherwise read the same.
Typing filters it, Up/Down and Tab/BackTab move, Enter inserts the name and
closes it, and Esc closes it and keeps the draft. Rows move by name, not by key:
a name may contain `j` or `k`, so those letters stay text. Selecting a row binds
that member's key to that occurrence for this draft; it never sends and never
inserts an identity into the text.

Every submit re-reads the roster, so the binding is a convenience, not an
authority: a member who left is resolved as a non-member and the draft is
blocked. Bindings belong to the draft's conversation and community, and a name
typed again is resolved again. If the roster read fails the picker says so and
`Ctrl+R` retries it; the send stays blocked until it answers rather than being
published against a guess.

At 24 by 6 the picker still shows its rows and its keys stay readable. The
status line drops the connection word, which the header already carries, so a
blocked draft keeps as much of its reason as the row holds - `?` keeps the whole
correction.

On send, the text is resolved against the current conversation's membership and
member profiles through the SDK's own helpers: code regions are stripped,
complete `@member names` are matched against display names, and non-code
`nostr:npub…` references are read as exact identities. A unique complete match
adds that public key to the message's signed `p` tags. The text itself is
unchanged, and duplicate recipients collapse.

A name resolves only when it is complete: `@Buzz` never expands to
`@Buzzx Build`. Case does not matter, spaces and Unicode names do, and an `@`
inside a word - an email address - is not a mention. To refer to a person
without notifying them, omit the `@` or put the text in a code span.

An unknown name, a name that matches more than one member, a reference to a
non-member, or a failed membership read stops the send before publication. The
draft and its reply target come back to the composer, the status line names the
fragment and the next correction, and the help surface (`?`) carries the exact
`nostr:npub…` reference for each ambiguous candidate, wrapped so the whole of it
stays readable at 24 columns. The correction is text: replace the ambiguous name
with one of those references. A short public key is display-only and is never
accepted as an input identity.

A fragment the SDK extractor cannot read at all counts as an unknown name, not
as text: a name that begins with a non-ASCII character has no fallback token, so
without this check the fragment would vanish from the recipient list while the
message published as written. The refusal names the fragment as the reader sees
it, up to the next space.

An exact reference picks one candidate of an otherwise ambiguous name, and it
does not excuse another unresolved name in the same draft. Every reference must
name a current member. The recipient list is capped at the SDK's
`MENTION_CAP` (50): a draft over the cap is refused, never truncated.

A draft with no mention input is sent without reading the roster or the
profiles. A read that fails is an error, never an empty member list, and the
relay stays authoritative if membership changes after validation.

New messages and replies use the same rules. A reply keeps its existing root and
parent tags and does not notify the whole thread. Explicit mentions in an
existing DM follow the same rule; adding every DM participant automatically is
not part of this slice. Editing an old message is unchanged: no resolution runs,
and no new notification is promised through an edit.

A blocked or refused send keeps the existing send-result semantics. Nothing is
retried automatically, and a confirmed send means the relay stored the event -
not that a recipient read it or an Agent started.

### Scope

No new CLI flags, relay API, stored draft identity model, global directory,
automatic invitation, completion UI, DM creation, or editor rewrite. The
tradeoff is deliberate: a duplicate name is corrected in plain text rather than
through a richer composer token.

### Acceptance

1. Complete unique member names - including spaces and Unicode - and exact
   references produce the intended signed `p` tags. Duplicates collapse, and a
   reply's root and parent tags stay correct.
2. Unknown and ambiguous names, non-members, a failed membership read, and an
   over-cap recipient list submit nothing and keep the draft. An exact reference
   resolves ambiguity without suppressing another error in the draft.
3. Code spans, fenced code, and email text create no recipients, and an ordinary
   message sends without the membership read.
4. At 24 by 6, 40 by 10, 79 by 12 and 80 by 12 the composer sends, shows an
   actionable error, exposes a full identity reference, and returns to the draft
   without losing it. No refused or uncertain write is sent twice.

### Verification

`RESEARCH/harness/verify_mentions.py` drives the real TUI over a PTY against the
harness fake relay and reads the relay log, so the evidence is the signed event
rather than the text on screen. Eight scenarios pass, at 80 by 12, 40 by 10, 79
by 12 and 24 by 6 (`WORK_LOGS/BUZZX_MENTIONS_ACCEPTANCE_LOG.md`, raw screens and
relay logs in `RESEARCH/harness/captures-mentions/`).

- `hello @Direct Person` in a channel where that name belongs to one member: the
  stored kind 9 event carries exactly that member's public key in its `p` tag,
  the content is stored unchanged, the relay log shows the roster read
  (`kinds: [39002]`, `#d: <channel>`, `limit: 1`) and the member-profile read
  before the write, and the mentioned identity's own `#p`-filtered query returns
  the message.
- A draft with no mention input (`hello there`) stores no `p` tag and issues no
  roster or profile read at all.
- `hello @Nobody` stores nothing, returns the draft to the composer with
  `mention "@nobody" is not a current member here` on the status line, and names
  the fragment on the help surface.
- Two members sharing the name `Twin One`: the draft is refused, the help
  surface exposes both candidate `nostr:npub1…` references at 24 columns, and
  `@Twin One nostr:npub1…` with one of them sends with exactly that public key
  as its recipient and nothing else.
- A `nostr:npub1…` reference to an identity that is not a member stores nothing
  and keeps the draft.
- `` `@Direct Person` `` inside a code span sends as plain text: no recipient,
  no membership read.
- A reply to a message written by someone else, naming one member, stores the
  named member as its only `p` tag and the answered message as its `e` tag with
  the `reply` marker: the parent's author is not turned into a recipient.
- A member whose display name is Chinese is named correctly, while an unknown
  name in Chinese, a half-typed one, and an unknown Chinese fragment next to a
  readable name all publish nothing and keep the draft; the same text in a code
  span sends as plain text without a membership read.

`RESEARCH/harness/verify_mentions_live.py` then drives the real TUI against the
deployment itself (`wss://buzz.surac.cloud`) and reads the event back with the
mentioned identity's own credential. Four sends pass - a channel and a
three-person DM conversation, at 80 by 12 and 24 by 6
(`RESEARCH/harness/captures-mentions-live/`) - and one refusal on the same
deployment. Each send's signed kind 9 event carries the conversation's `h` tag
and exactly the named member's `p` tag; the reply inside the DM also carries the
answered message as its `e` tag with the `reply` marker, and neither the parent's
author nor the third participant becomes a recipient. The mentioned identity's
own read returns the event (`buzz messages get`, `buzz social event`) and its
mention feed lists it (`buzz feed get --types mentions`). The refusal publishes
nothing at all: the conversation's event set is identical before and after.

Not verified: no Agent execution was observed from a mention - the evidence is
storage and the recipient's own read, not an Agent's turn. The live runs cover
one deployment and the acceptance identities in this workspace, not every relay
or membership edge, and the fake relay still does not enforce authorization, so
the refusal-by-policy case (a valid mention the reader is not entitled to read)
is out of scope here.

Source basis: Buzz SDK builders and mentions, the CLI message preflight, Desktop
mention candidates and Mobile message recipients at upstream revision
`93114c9c65138397de39729fde0a816eb9f314ab`. Both graphical clients separate
names from identities; this slice reuses that semantic requirement without
copying their richer editor and non-member invitation flows.

## Focused thread reading

Status: implemented. This slice lets a person follow one conversation inside a
busy channel, reply, and return to the same channel row. It applies to stream
channels and existing DMs. Forum navigation remains deferred.

### Enter, read, return

In timeline navigation, `t` opens the focused message's containing thread.
A top-level message is its own root; a reply uses its existing thread root.
An empty timeline or an unconfirmed local send cannot open a thread. Keep the
current view and explain why. Do not change the existing `Enter` reply shortcut
in the channel timeline.

The thread uses one full-screen timeline at every supported size. Show the
channel label, `Thread`, and `Esc: back`; do not add a split pane or nested
navigation stack. The root is the first row, followed by loaded replies in
chronological order. Focus the message used to enter when it is available;
otherwise focus the root and report that the selected reply was not loaded.
The root scrolls normally; it is not pinned above every reply.

```text diagram
#general / Thread
  Alice: Can we ship this?
> Build: Checks passed.
  Product: One acceptance case remains.
Enter: reply   i: reply to root   Esc: back
```

Within this view, `j`/`k` and arrows move message focus at all widths.
`g`/`G`, Home/End, PgUp/PgDn, help, and quit retain their timeline meaning.
`t` does not open another level. Conversation shortcuts, switcher, Inbox filters,
and Agents navigation are unavailable until returning; they must not silently
switch the destination. `Esc` in navigation returns to the saved channel,
filter, focus, and viewport. If the saved row was deleted, select its nearest
surviving neighbor. Incoming messages must not pull focus away from older rows.
Resize, including below 24x6 and back, preserves this context.

### Tail follow and detached reading

The thread follows its tail exactly while its focused row is the newest one:
entering at the newest reply, `G`/`End`, stepping onto the last row, and
sending your own reply all follow. A matching live reply that arrives while
following keeps the newest row in view and does not count.

Every other position is detached. A matching live reply is cached without
moving the focused row, the viewport, or a line of the message being read. The
status line then counts what this client observed since it left the tail:
`3 new replies · G latest`, `1 new reply · G latest`, and the compact
`3 new · G latest`. The count is this client's observation, never the thread's
total reply count. `G`/`End` reads the newest window, focuses the newest valid
event, clears the count and follows the tail again. A reply that is not the
newest one - a late backfill above the reader - neither moves the view nor
counts.

An older-page request on any of the three surfaces - channel, context, or
thread - that would answer a full page inside the boundary second grows the
request (4x per retry, up to the relay's own bound of 1000 rows) before that
second is reported as unpassable, so `History limit reached` means a single
second holds more events than one query may carry.

### Reply without changing destination

`Enter` replies to the focused confirmed row; `i` or Tab replies to the root.
The composer visibly identifies the reply target. Sending uses the existing
mention resolution and write-result rules. A reply to a reply keeps the thread
root and uses the focused message as parent. No implicit recipients or channel
broadcast are added. Reaction, edit, and delete keep their existing ownership
and focused-row rules.

Share the selected conversation's existing composer buffer; preserve existing
per-conversation draft behavior. If it contains unsent text, entering or leaving the
thread is refused with `Send or clear the draft first`; the text and destination
stay intact. This avoids adding a per-thread draft store. Empty composers may
change view. In a thread composer, Esc leaves composing without clearing its
reply or edit destination; returning to channel navigation still requires an empty
buffer. Never turn a cancelled thread reply into a top-level channel send.
A refused or uncertain write follows the existing send contract, never an
automatic resend. Pending writes must settle before leaving the thread.
An uncertain result ends that pending wait: keep the uncertain row and allow
return, without restoring that already-attempted send as a ready-to-send draft.
An unconfirmed local row cannot be a reply/edit/reaction target.

When a nonempty draft exists, i, Tab, or Enter resumes that draft with its
original reply/edit target; none retargets it to the root or current focus.
Starting a different edit is refused until the draft is sent or cleared.
The root/focused-row shortcuts above apply only to an empty buffer. Clearing
means deleting the buffer's text using the existing editor; no discard dialog
or draft manager is added. On leaving with an empty buffer, clear thread-only
reply/edit targets so the next channel composition cannot inherit them.
If a target is deleted while composing, keep the text but refuse its write;
the user can clear the draft and return. Empty edited content must not be
implicitly published when leaving.

These are thread-view rules. In particular, thread Esc leaves composing in one
press and preserves its target, unlike the existing channel composer's two-step
Esc. Help must state this difference. Quit retains the existing application
behavior; it does not promise persisted drafts.

### Honest loading and read state

Fetch the root and replies even when the root is outside loaded channel
history. A local filter of the channel's loaded rows is insufficient. Show
`Loading thread...`, not an empty-thread claim, until the query finishes.
On missing or inaccessible root, or query failure, show an explicit error and
keep Esc available. `t` retries a failed read only; it never retries a write.
Late results from a closed thread must not replace the active timeline.

The initial thread read is bounded: root plus at most 500 replies. If the
reply query reaches its limit, show `Partial thread: reply limit reached`.
Determine saturation from the raw reply query before deduplication or filtering.
The initial cap is not a total reply count or an archive boundary. Continue
through [incremental history](tui-context.md#c4-read-earlier-and-later-history)
and use that contract's saturation and error behavior. Thread-only search,
follow/unfollow, thread unread badges, and reply-count queries remain deferred.
If the deployment cannot provide the initial bounded read, report that
as an implementation blocker rather than silently using channel cache only.

Display only messages from the selected conversation belonging to this root.
Validate both conversation and thread membership before displaying a returned
row; do not trust an arbitrary e-reference to prove thread membership. Use the
existing root/parent semantics. Resolve an entry reply's root before querying;
reject a root in another conversation rather than switching conversations.
Merge matching live replies once by canonical event id; unrelated traffic
continues to update the channel in the background. Apply edit, delete, and
reaction overlays to loaded rows as in the channel view. A deleted root becomes
a `Root deleted` placeholder while loaded replies remain readable; disable
root-targeted actions. Disconnect keeps loaded content with a stale/reconnecting
status. Reconnect re-reads the bounded thread before removing that status.
If a reconnect read fails, keep loaded rows with an explicit stale/error state
and allow t to retry. A late history response must not undo a newer observed
edit or deletion, or drop a matching live reply received during the read.
Before initial load succeeds, disable every content-targeted action. While
disconnected, composing and reading remain possible but publication is disabled
with a connection reason; do not queue writes for reconnect. Lost membership
blocks writes and exposes the access error; draft text remains available.
Typing remains channel-scoped and must not be labeled as thread activity.

Thread reading does not advance the channel read frontier: other discussions
may be unseen. On return, the ordinary channel presentation rules decide when
read progress advances. No new persistent read state is introduced.

### Thread view layout

These are proposed frames with sample messages, not implementation screenshots.
Blank lines count toward the height; trailing padding is omitted. Each line
fits its stated ASCII column budget.

Use the existing palette: bold for focus/context, dim for metadata, normal for
bodies, and the existing error accent for failures. The `>` focus marker and
state text work without color. Authors are labels, not notification syntax.

| Region | Reading | Composing |
| --- | --- | --- |
| Header: one row | Thread, conversation, back hint | Thread and conversation |
| Timeline: remaining rows | Keep focus visible | Keep at least one context row |
| Target: one row when composing | Absent | Reply to author, or Edit own message |
| Input: one row when composing | Absent | Buffer and cursor; horizontal scroll in minimal mode |
| Keys: one row | Navigation hints | Send and leave-composer hints |
| Status: one row | Connection, coverage, or error | Connection or write outcome |

Reading has no empty composer border. At 24x6 it leaves three message rows;
composing leaves one context row. At larger sizes use the existing multi-line
composer, provided all other regions still fit. At 79x12 use the same single
column as 80x12 with width-dependent wrapping, never a sidebar. The target is
an explicit label, never inferred from the currently visible timeline row.

#### 80x12: reading

```text diagram
Thread / #buzzx-tui                                             Esc: back
  Alice  2m  [root]
  Can we ship the mention fix?

> Buzzx Build  1m
  Checks passed. The installed version is ready.

  Product  30s
  I will verify the recipient readback.

j/k: move  Enter: reply  i: root  ?: help
Connected
```

#### 40x10: replying

```text diagram
Thread / #buzzx-tui
  Alice [root]
  Can we ship the mention fix?
> Buzzx Build
  Checks passed.

Reply to Buzzx Build
> @Buzzx Build please share evidence
Enter: send  Esc: cancel compose
Connected
```


#### Clipping and priority

Reserve the navigation back hint before shortening the conversation name.
Use `...` for clipped labels; wrap message bodies rather than truncating
content. Measure terminal cells, not bytes: wide characters and emoji must not
overlap markers or status. The root label belongs only to the root row; it is
not a second pinned header. No reply counts or progress percentages are added.

The status row prioritizes write error/uncertainty, then read error or stale
connection, then partial coverage, then connection success. Lower-priority
states and full clipped labels/errors remain available in help. Leaving help
returns to the thread. In composer mode `?` remains literal input; Esc returns
to navigation before opening help. When a higher-priority transient message
clears, reveal the still-active lower-priority state.

#### State frames

| State | Content | Status and actions |
| --- | --- | --- |
| Loading | Loading thread, no false empty result | Esc back; sending disabled |
| Root without replies | Root stays visible | No replies yet; i replies to root |
| Read failure | Error replaces unloaded content | Thread load failed; t retry, Esc back; reason in help |
| Partial | Loaded rows remain usable | Partial: limit reached; limit explanation in help |
| Disconnected | Keep loaded rows | Reconnecting; stale |
| Sending | Dim pending reply | Sending...; leaving reports Wait for send result |
| Refused write | Restore draft and target | Error reason; correction and manual send |
| Uncertain write | Existing uncertain-write presentation | Send unconfirmed; no automatic retry |
| Draft blocks return | Keep text and destination | Send or clear draft; i resumes existing draft |
| Deleted root | Root deleted placeholder | Replies remain; disable root-targeted actions |

Short status labels fit 24 columns; detailed reasons go to help rather than
taking the input row. Suppress channel-scoped typing inside the thread view:
it cannot identify who is replying to this thread. The channel view keeps its
existing typing behavior.

### Acceptance required before delivery

| Case | Required evidence |
| --- | --- |
| Root and nested reply entry | Both open the same root; root outside channel cache is fetched; unrelated discussion excluded |
| 80x12, 79x12, 40x10, 24x6 | Open, move focus, reply, return; resize preserves target and saved channel focus |
| Reply to root and to reply | Stored event has correct h/root/parent and only explicit mention recipients; recipient-authenticated readback |
| Draft and failed send | Resume via i/Tab/Enter preserves reply/edit target; refusal keeps draft; uncertainty permits return without a resend draft; empty exit clears thread targets |
| Live and overlays | Matching reply appears once; unrelated reply stays out; edit/delete/reaction targets remain correct |
| Read frontier | Reading this thread leaves other channel unread candidates intact |
| Loading/error/late result | No false empty success; Esc works; closed-thread results cannot steal the view |
| Full bounded page | Partial label appears, no invented count or claim of complete history |
| Reconnect and deleted root | Stale state clears only after successful read; live changes survive read races; deleted targets and lost access cannot be written to |

Implementation delivery requires the repository checks and a real-relay TUI
run, including connect/send/reply/react/edit/delete/quit, at the final source
state. Record signed events, authenticated readback, dimensions, and source
revision separately from CI. Merge and verify remote main, then identify the
actual usable binary. A spec commit or passing unit tests alone is not delivery.
