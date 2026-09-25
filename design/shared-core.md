# Shared core

The TUI and the CLI perform the same relay operations. This document fixes
where that shared implementation lives, what each side owns, and the
contracts a caller inherits from it. The CLI's product contract is
[../docs/browse-collab-cli.md](../docs/browse-collab-cli.md); this document
does not restate it. The terms are defined in [../CONTEXT.md](../CONTEXT.md).

[../core.md](../core.md) already requires the non-interactive subcommands to
share the transport, the identity handling, and the event construction with
the TUI. This is how that requirement is met.

The five commands of the slice do not exist yet. This is the design they
start from. [architecture.md](architecture.md) records the module map of the
code that exists, and points here for the target.

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

- `channels()` - the membership roster (kind 39002, `#p` the identity), then
  metadata (kind 39000, `#d` the ids). One `ChannelInfo { id, name }` per
  roster id, the name from the metadata event when there is one and the id
  when there is not. A metadata failure fails the call instead of shortening
  the list: today a failed metadata query empties the TUI's channel list, and
  that is a bug this fixes.
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

- One validation point, before signing. Empty content, content over 64 KiB,
  and a malformed id are `invalid_input`, and no event is signed.
- One retry rule, owned by [relay-transport.md](relay-transport.md). A read
  may be retried once after a connect failure or a lost answer, because a
  read changes nothing. A write is retried only while it is certain that the
  request never left the client; after that a retry can duplicate an event.
- No rendering, no wording, no exit code.

## What each front end keeps

The TUI (`session.rs` and the modules under it) keeps what a held session
needs: subscriptions, reconnect, optimistic rows, typing, presence, read
markers, overlay backfill, key mapping, and the choice of when to ask. The
command pump becomes a translation: call the core, turn the result into a
`ChatEvent`.

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
  relay refused, and it did not store the event.
- Any other refusal is `relay_rejected`, carrying the relay's own reason. A
  5xx answer to a write leaves storage unestablished.

The core raises what a result decides. A single-event read that returns no
event, and a thread root that does not resolve, are `not_found`. A reply
target without a channel is `invalid_input`, like empty or oversized content
and a malformed id.

| Outcome | Meaning | CLI status | Exit |
|---|---|---|---|
| `Stored` | the relay accepted the event and returned its canonical id | `sent_confirmed` | 0 |
| `Refused` | never submitted, or the relay refused it | `not_sent` | by category |
| `Unknown` | submitted, and storage is not established | `sent_unconfirmed` | 2 |

| Category | Exit |
|---|---|
| `invalid_input`, `not_found` | 1 |
| `network`, `timeout_unknown` | 2 |
| `forbidden` | 3 |
| `relay_rejected` | 4 |

`not_found` exits 1 to match `buzz`'s own contract, which groups input and
not-found. It is not a pre-network verdict: a referenced event that resolves
to nothing is learned from the relay's answer.
[../docs/configuration.md](../docs/configuration.md) now defines code 1 as
bad input before any network call, and gains this case when the code lands.

The mapping covers the five commands of the CLI slice. The subcommands that
already exist keep the codes that document records.

`sent_unconfirmed` exits 2, so a script cannot read an unknown write as a
success. The CLI never retries it; the caller decides.

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
- Event objects carry all seven fields of the spec, `null` when a value is
  unknown, so a caller never branches on presence.
- `channels list` returns one object per channel: `channel_id` and `name`.
- `messages get --channel` returns the newest `--limit` messages (default 20,
  minimum 1) in timeline order, oldest first. An empty channel is an empty
  array.
- `messages get --event` returns one object, and `messages thread` returns an
  array with the root first. Each is `not_found` when the id resolves to no
  event.
- A write returns one object: `status` always; `event_id` when the write was
  confirmed and `null` otherwise; `channel_id`; and `reply_to` for
  `messages reply`. When the status is not `sent_confirmed`, the object also
  carries `error` and `message`.
- A read failure returns one object: `error`, `message`, and the id the
  command was given or derived from (`channel_id`, or `event_id`);
  `channels list` carries neither.
- `--content -` reads stdin as bytes and keeps every byte, including a
  trailing newline. An empty stream or invalid UTF-8 is `invalid_input`
  before anything is signed.
- `messages reply --event <id>` is one read and one write: resolve the
  target, derive the routing, sign once. A target that does not resolve is
  `not_found` with the target in `event_id`.

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

## How this lands

1. `failure.rs` and the core's reads; `session.rs` drives them and loses the
   moved code, including the silent metadata fallback. A live run opens the
   channel list, one channel, and its history.
2. The writes move; the pump keeps only the translation. A live run sends,
   replies, reacts, edits, and deletes.
3. `cli.rs`, the clap tree, the JSON projection, and the exit codes. The
   canonical flow of the spec runs end to end against the live relay.
4. The documents that own the facts this design borrowed:
   [architecture.md](architecture.md) gains the core's rows and the
   dependency rule, [../docs/configuration.md](../docs/configuration.md)
   gains the exit codes and the widened code 1, and this document loses what
   they now own. One product question stays open: [../core.md](../core.md)
   still sends the automation caller's one-shot reads to `buzz`, while
   [../docs/browse-collab-cli.md](../docs/browse-collab-cli.md) supersedes
   that paragraph for this slice. The two contracts should agree before the
   code lands.

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
