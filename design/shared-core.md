# Shared core

The TUI and the CLI perform the same relay operations. This document fixes
where that shared implementation lives, what each side owns, and the
contracts a caller inherits from it. The CLI's product contract is
[../docs/browse-collab-cli.md](../docs/browse-collab-cli.md); this document
does not restate it. The terms are defined in [../CONTEXT.md](../CONTEXT.md).

[../core.md](../core.md) already requires the non-interactive subcommands to
share the transport, the identity handling, and the event construction with
the TUI. This is how that requirement is met.

The five commands of the slice exist, and the core they share exists with
them. [architecture.md](architecture.md) records the module map;
[../docs/browse-collab-cli.md](../docs/browse-collab-cli.md) is what the CLI
promises a caller, and [../docs/configuration.md](../docs/configuration.md)
owns the exit codes. This document states the shape the code must keep.

## Why one implementation

Four of the five commands repeat an operation the TUI already performs in
`session.rs`, where each one is entangled with the live session:
`load_channels` also sets the typing feed, `open_channel` also opens a
timeline subscription and backfills overlays, and the write path is a match
arm that ends in a `ChatEvent`.

A CLI that copied those operations would copy four decisions that must stay
identical to the TUI's:

- the channel-list query and its name fallback,
- the history filter and its order,
- the signing path, NIP-OA tag included,
- the write-result classification.

Drift in any of them is invisible. A different filter still returns rows, a
differently signed event still publishes, and a second classification still
prints a status. The two front ends would disagree, and nothing would fail.

## The core

`src/client.rs` is the core: the relay operations both front ends perform.
One async call per operation, no state between calls, and no rendering,
wording, or exit code. It holds the identity and the parsed NIP-OA tag, and
it knows the bridge (`http.rs`) and the pure accessors (`content.rs`). An
operation may take more than one query: a channel list is a roster query plus
a metadata query, and a thread is the root plus its replies. Every id is
typed on entry (`Uuid`, `EventId`, `ThreadRef`), so a malformed id cannot
reach a builder.

Reads, each returning a typed value:

- `channels()` returns the membership roster, its metadata, and hidden-DM
  visibility. Classification-query failures produce an incomplete roster;
  membership-query failure fails the call. Rows with no metadata are marked
  unknown, not silently assigned a kind. (Sources: `Client::channels`,
  `channels_from`, `Roster::complete`.)
- `history(channel, limit)` - the timeline kinds, `#h` the channel, oldest
  first. An unknown channel UUID is an empty array; the bridge has no
  channel-existence query, so "no such channel" and "no messages" cannot be
  told apart. A refusal is an error, never an empty array.
- `thread(root)` - the root event first, then every event that references it,
  oldest first. Two queries, because `/query` takes one filter, merged and
  deduplicated by event id. A root that does not resolve is `not_found`. 500
  replies is the bound; a longer thread is a later slice.
- `event(id)` - one event, `not_found` when the relay returns none.
- `profiles(pubkeys)` - display names: deduplicated, capped at 200, queried
  in chunks of 50. A failed chunk fails the call. The TUI is the only caller
  today and ignores that failure, because a name it cannot resolve stays a
  short key; the CLI slice has no profile command.

Writes, each returning a `WriteOutcome`:

- `submit(builder)` - attach the NIP-OA tag, sign, submit, classify.
- `send_message(channel, content, thread)` - a kind 9 message through
  `submit`. The composer and `messages send` produce the same event.
- `reply(target, content)` - derive the routing from `target`, then
  `send_message`.
- `edit`, `delete`, `react`, `remove_reaction` - the TUI's other writes. The
  CLI slice does not expose them; they move with `submit` so that every
  signed event has one path.

The shared types:

```text literal
// failure.rs
pub enum Category { InvalidInput, NotFound, Forbidden, Network, TimeoutUnknown, RelayRejected }
pub struct Failure { pub category: Category, pub detail: String }

// http.rs
pub enum WriteOutcome {
    Stored { event_id: String },
    Refused { category: Category, reason: String },  // the relay did not store it
    Unknown { category: Category, reason: String },  // storage is not established
}
```

`query` returns `Result<Vec<Event>, Failure>`. The bridge turns a status and
a transport error into a category; the core adds the categories a result
decides.

The routing rule `messages reply` needs:

```text literal
channel = extract_channel_id(target)            // the h tag, buzz-sdk
root    = root_of(target).unwrap_or(target.id)  // content.rs
parent  = target.id
```

`ThreadRef` collapses `root == parent` into one `e` tag, so a reply to a
top-level message and a reply inside a thread come out of the same rule. A
target without a channel is `invalid_input`: the reference resolves, but it
cannot receive a channel reply. Today the send path converts a bad id to
`None` and would publish an unthreaded message; the typed entry removes that
path.

Invariants:

- One validation point, before signing. Empty content and a malformed id are
  `invalid_input`, and no event is signed.
- One retry rule, owned by [relay-transport.md](relay-transport.md). A read
  may be retried once after a connect failure or a lost answer, because a
  read changes nothing. A write is not automatically retried by the client;
  an uncertain publish is not resent because it may have been admitted. A later
  read advance may issue a new publish; that is a new read update, not a retry.
- No rendering, no wording, no exit code.

## Inbox and read state

`app.rs` owns the conversation sections, filters, unread candidates, and the
read frontier. `client.rs` reads and publishes markers; `session.rs` schedules
the read-back and turns failures into events; `sub.rs` watches listed
conversations under `Channels` and `DMs`. Hidden DMs and archived channels are
excluded by `ChannelInfo::listed`; if the visibility or metadata query fails,
the roster is incomplete and the list is not authoritative. Missing metadata
also sets `Roster::complete` false. Generic DM labels use other participants'
profile names or shortened public keys, name at most three, and then add
`+N more`. (Sources: `src/client.rs::ChannelInfo::listed`, `Client::channels`,
`channels_from`; `src/app.rs::label`, `author_name`, `merge_channels`.)

A row's signal is `●` for unread, `@` for an unread direct mention, `?` for
unknown state, `Read` only for a picker row retained after it stops matching,
and two spaces for no signal. `●` and `@` include the count of unread
message candidates, such as `● 3` or `@ 2`. The five-column minimum signal
cell is reserved before the label is clipped. Typing adds `…` after the label
and can coexist with a signal. `All`, `Unread`, and `For you` are filters on
the same list; `For you` includes every DM and channels with unread direct
mentions. New messages do not reorder rows. (Sources: `src/app.rs::Filter`,
`refresh_views`, `marker`, `unread_count`, `picker_view`;
`src/ui.rs::conversation_item`, `list_rows`; `src/sub.rs::inbox_filter`.)

Read progress advances only after history has loaded, the conversation is
selected and visible, focus is at its newest loaded message, and neither the
picker nor help covers the timeline. A conversation without a stored context
starts from a local baseline at its newest message. That seed is not proof of
reading and is never uploaded. A message timestamp equal to the frontier
remains unread until that event is shown, because frontiers have second
resolution. (Sources: `src/app.rs::note_presented`, `apply_seed`,
`ReadTrack::unread_at`; `src/client.rs::CatchUp::Newest`.)

The marker is a NIP-44 encrypted kind 30078 event. Its `d` tag is
`read-state:` plus the hex encoding of the first 16 bytes of SHA-256 over the
public-key hex text. Its `t` tag is `read-state`. The payload written by this
client has `v: 1`, `client_id: "buzzx"`, and `contexts`, whose keys are channel
UUID strings and whose values are Unix seconds. The parser also accepts a
missing `v` as version 1 and requires a `client_id` string of at most 64
bytes. Read-back merges every matching slot authored by this identity using
the per-context maximum before writing this client's slot. The lookup covers
the last seven days and up to 500 events. (Sources: `src/read_state.rs::slot`,
`builder`, `parse`; `src/client.rs::read_state`, `publish_read_state`.)


A failed marker lookup or an unreadable owned slot is unknown, not read. Failed
or truncated conversation catch-up is also unknown. A failed publish leaves
the local frontier advanced and shows `Read here; not synced (<reason>)`; the
current publish is not retried automatically. Later read updates can issue new
publishes. The session continues. (Sources: `src/app.rs::apply_read_state`,
`apply_catch_up`, `inbox_footer`; `src/session.rs::publish_read`,
`run_command_pump`.)


## What each front end keeps

The TUI (`app.rs`, `session.rs`, `sub.rs`, and `ui.rs`) keeps the held-session
state: subscriptions, reconnect, conversations, unread tracking, typing,
overlays, key mapping, and rendering. `client.rs` owns the relay queries,
read-state encryption and merge, and writes. The command pump calls the core
and translates results into `ChatEvent`s.


The CLI (`src/cli.rs` and the clap tree in `main.rs`) keeps the one-shot
concerns: argument parsing, the JSON projection, stdin content, stderr
diagnostics, and exit codes. One command, one call, one result, and no state
that survives the call.

Nothing above the core names the bridge any more:

```text diagram
  main.rs : argument parsing, dispatch, the TUI event loop
    |
    +-- cli.rs      one-shot commands: JSON, stdin, exit codes
    +-- session.rs  the live session: subscriptions, ChatEvents
    |     |
    |     +-- sub.rs   the WebSocket pump
    |
    +-- client.rs   the core: the relay operations both front ends drive
          |
          +-- http.rs     the NIP-98 bridge: /query, /events
          +-- content.rs  root_of, parent_of, profile_names
          +-- config.rs   identity, relay, auth tag
          +-- failure.rs  the categories

  client.rs and http.rs import failure.rs. Nothing imports cli.rs,
  session.rs, or sub.rs.
```

## Failures

`src/failure.rs` owns the category type both front ends branch on. The
meanings are the spec's; this section fixes which layer raises each one and
which code the CLI exits with.

The bridge raises what the relay's answer decides:

- A connect failure, before the request left the client, is `network`.
- 401 and 403 are `forbidden`.
- 404 is `not_found`.
- A read that loses its answer - a timeout, or a body that dies mid-send -
  is `network`, and the read may be repeated.
- A write that loses its answer - a timeout, a body that dies mid-send, or a
  502 through a 504 from a proxy - is `timeout_unknown`, and storage is not
  established.
- A 4xx answer to a write is `relay_rejected` (401 and 403 excepted): the
  relay refused, and it did not store the event. A write has no `not_found`:
  a 404 on a write is the relay's own refusal, not a reference that resolved
  to nothing.
- Any other refusal is `relay_rejected`, carrying the relay's own reason. A
  5xx answer to a write leaves storage unestablished.
- An answer whose rows none parse as events is `relay_rejected`, never an
  empty collection: a read the client could not read is not a read that
  found nothing. A row that fails to parse among readable ones is dropped,
  so one bad row cannot discard a channel's history.

The core raises what a result decides. A single-event read that returns no
event, and a thread root that does not resolve, are `not_found`. A reply
target without a channel is `invalid_input`, like empty content and a
malformed id.

A startup failure - an identity or a relay that does not resolve - is raised
before any relay call and keeps its configuration code. The CLI prints the
category that carries that code, so a one-shot command answers with one JSON
object on every path: 1 is `invalid_input`, 3 is `forbidden`, and 4 is
`relay_rejected`.

| Outcome | Meaning | CLI status |
|---|---|---|
| `Stored` | the relay accepted the event and returned its canonical id | `sent_confirmed` |
| `Refused` | never submitted, or the relay refused it | `not_sent` |
| `Unknown` | submitted, and storage is not established | `sent_unconfirmed` |

`not_found` exits 1 to match `buzz`'s own contract, which groups input and
not-found. It is not a pre-network verdict: a referenced event that resolves
to nothing is learned from the relay's answer, which is why
[../docs/configuration.md](../docs/configuration.md) widened code 1 from
"bad input before any network call" to include it.

The mapping covers the five commands of the CLI slice. The subcommands that
already exist keep the codes that document records.

[../docs/configuration.md](../docs/configuration.md) owns the table that turns
a category into an exit code. A failure exits by its category, so the code
and the `error` the object prints cannot disagree: a write whose answer was
lost is `timeout_unknown` and exits 2, and a relay that answered and failed
leaves the write `sent_unconfirmed` with `relay_rejected` and exit 4. Either
way a script cannot read an unknown write as a success. The CLI never retries
it; the caller decides.

`Stored` requires the relay's canonical id, because a reply, an edit, or a
delete must address the event the relay stored, and the relay may store a
different id than the one that was signed. A 2xx answer without an id is
therefore `Unknown`, and the fallback to the locally signed id goes away.

Two TUI behaviors change with these rules. A write whose connect failed is
reported as uncertain today, and its optimistic row stays behind; a request
that never left the client cannot have been stored, so it becomes a failure
with the category `network`: the row is removed, the draft, the reply target,
and the composer mode come back, and the status line says the write failed.
An unconfirmed write keeps the row marked uncertain, as it does today, and is
never retried.

## The CLI contract the spec leaves open

- stdout carries one JSON value: an array for a channel read, an object for
  one event, one object for a write. Diagnostics go to stderr.
- `channels list` returns an array of `{channel_id, name}` objects on stdout.
  It also writes an incomplete-roster warning to stderr when a classification
  query fails or a roster row has no metadata. It projects every roster item;
  the TUI separately excludes hidden DMs and archived channels from its list
  when their classification queries succeed. (Sources:
  `src/cli.rs::run_channels`, `src/client.rs::Client::channels`,
  `ChannelInfo::listed`.)
- `messages get --channel` returns the newest `--limit` messages (default 20,
  minimum 1) in timeline order, oldest first. An empty channel is an empty
  array. `--limit` belongs to a channel read: with `--event` it is
  `invalid_input`, like any other contradictory argument.
- `messages get --event` returns one object, and `messages thread` returns an
  array with the root first. Each is `not_found` when the id resolves to no
  event.
- A write returns one object: `status` always; `event_id` when the write was
  confirmed and `null` otherwise; `channel_id` when the command has a channel;
  and `reply_to` for `messages reply`, stored or not. When the status is not
  `sent_confirmed`, the object also carries `error` and `message`, and the
  reason goes to stderr too. A write the command refused before submission
  keeps this shape with the status `not_sent`, and the ids it was given:
  `--channel` for a send, the target for a reply.
- A read failure returns one object: `error`, `message`, and the id the
  command was given or derived from (`channel_id`, or `event_id`);
  `channels list` carries neither.
- A failure raised before the command ran uses the recognized command's
  result shape: a read error object or a write with `status: not_sent`.
  The product specification owns this requirement and its implementation status;
  the argument-parser exception is defined in configuration documentation.
- `--content -` reads stdin as bytes and keeps every byte, including a
  trailing newline. An empty stream or invalid UTF-8 is `invalid_input`
  before anything is signed.
- `messages reply --event <id>` is one read and one write: resolve the
  target, derive the routing, sign once. A target that does not resolve is
  `not_found` with the write `not_sent` and the target in `reply_to`, never in
  `event_id`, which names the reply that was not stored.

## Out of scope

- No second crate. The core is a module of the one `buzzx` binary; see
  [decision 0005](decisions/0005-shared-core.md).
- No transport trait and no mock layer. The parsing stays pure and unit
  tested, and the round trips are proven against the fake relay and one live
  run, as [../eng/testing.md](../eng/testing.md) and
  [../eng/ci.md](../eng/ci.md) require.
- No capability the CLI slice does not name: search, `watch`, agent drafts,
  channel administration, pagination, or MCP. `watch` keeps its own
  WebSocket path; it streams a subscription, which is not an operation the
  two front ends share.
- No TUI redesign.

## Status

Landed. `client.rs` owns the operations, `session.rs` translates, `cli.rs`
projects, and `tests/cli.rs` drives the five commands against a fake relay.

The product boundary is settled in [../core.md](../core.md). Remaining
implementation gaps are tracked in the
[product specification](../docs/browse-collab-cli.md#implementation-status).

## Verification

- Unit tests, no socket: the routing rule against a top-level message, a
  direct reply, and a nested reply; the channel-list parse with complete
  metadata, with a missing entry, and with a failed query; the order of a
  thread read; the validation of content and ids; the category of each
  answer; the JSON projection of the three event shapes and of a write
  result.
- A local fake relay drives the five commands and asserts the JSON and the
  exit codes, including a refused write and a lost answer.
- One live run of the canonical flow, and one live TUI session for the
  writes that moved.
