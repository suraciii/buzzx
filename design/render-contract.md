# Render Contract

This document defines how `buzzx` turns relay events into timeline rows, how
overlays apply to those rows, and how unread state is decided. It is the
contract that `content.rs` and the row-manipulation parts of `app.rs`
implement.

## Event kinds

Kinds come from `buzz_core::kind`. The values below are the ones buzzx acts
on; the table states what buzzx does with each, not what the kind means to the
relay.

### Timeline kinds

These become rows. They are requested in the channel history REQ and in the
live subscription.

| Kind | Constant | Row content |
|---|---|---|
| 9 | `KIND_STREAM_MESSAGE` | The message body |
| 40002 | `KIND_STREAM_MESSAGE_V2` | The message body |
| 40008 | `KIND_STREAM_MESSAGE_DIFF` | The diff text, rendered as a code block |
| 45001 | `KIND_FORUM_POST` | The post body |
| 45003 | `KIND_FORUM_COMMENT` | The comment body |

Requesting only these kinds means the `limit` on a history REQ buys visible
rows instead of being spent on reactions and edits.

### Auxiliary kinds

These never render their own row. They are fetched by `#e` reference against
loaded message ids and applied as overlays.

| Kind | Overlay |
|---|---|
| 7 | Reaction: add or remove one emoji counter on the target row |
| 40003 | Edit: replace the target row's body and media |
| 5 | Deletion: remove the target row |
| 9005 | Deletion: remove the target row |

Kind 5 has a second use: removing the identity's own reaction. When the target
of a kind 5 is a reaction event id rather than a message id, the counter is
decremented instead of removing a row. The client keeps a small map of reaction
event ids it has sent, so it can tell the two apart.

### Kinds buzzx shows but does not request

| Kind | Behavior |
|---|---|
| 40099 | System rows (member joined, channel created). Shown if they arrive. |
| 39005 | Thread summary. Used for a reply count badge. Never a row. |

## Row shape

```text literal
Row {
    event_id:   String        // hex
    pubkey:     String        // hex, author
    author:     String        // display name, resolved from profiles
    created_at: u64           // unix seconds
    body:       String
    kind:       u32
    root_id:    Option<String>
    broadcast:  bool
    reactions:  Vec<(String, u32)>
    pending:    bool          // optimistic local send, replaced on relay OK
}
```

`author` is resolved from kind 0 profiles, falling back to a shortened pubkey.
The TUI requests profiles for the authors it has seen, in chunks, and keeps
them in a map. A missing profile is not an error.

## Threading

A reply is identified by NIP-10 marker tags, in this order:

```text literal
["e", "<parent>", "", "reply"]                      // direct reply to the root
["e", "<root>", "", "root"]                         // then, when nested
["e", "<parent>", "", "reply"]
```

`root_of(event)` reads the `root` marker and falls back to the `reply` marker,
then to the parent id. `parent_of(event)` reads the `reply` marker only.

Sending applies the same shape. Replying to a thread root produces one tag. A
nested reply produces both, in that order. Two replies to two different
messages in the same thread both carry the same root id, which is what groups
them.

A `["broadcast", "1"]` tag on a reply makes it notify the whole channel
instead of the thread. `buzzx` shows a marker on such a row. It can send one
with an explicit key; it does not send broadcast by default.

## Media and attachments

An attachment is an `imeta` tag on the message event:

```text literal
["imeta", "url <https-url>", "m image/png", "x <sha256>", "size 182344",
 "dim 1920x1080", "blurhash <...>", "thumb <url>", "filename <name>"]
```

The body carries a matching markdown line with the same URL. The TUI renders
the row from the tag and keeps the body line visible, because the relay
validates the hash of the URL against the `x` value, not the body text.

`imeta` parsing is client-side. Buzz has no client-side `imeta` decoder, so
`content.rs` reads the `key value` pairs directly.

## Reactions

A reaction (kind 7) has an empty or emoji body and one `e` tag naming the
target. The emoji is at most 64 characters. `buzzx` counts reactions per emoji
per row.

`buzzx` sends a reaction with `+` replaced by `👍` only when the body is empty;
otherwise it sends the body as-is. Removing a reaction sends a kind 5 event
against the reaction's own event id, not the message id.

Reaction counts are per-session and derived from observed events. `buzzx` does
not try to produce a globally correct count: it starts each session at zero
and applies what it sees. This is a stated limitation, not a bug. A
backfilled full count would require a second query per visible row, and the
desktop app's own reaction history is not a count — it is a list.

## Edits

An edit (kind 40003) carries `["h", channel]` and `["e", target]`, and its
content is the complete new body. Applying an edit replaces the body and the
media set. It never merges.

An edit can add mentions. The relay treats mentions as additive, so an edit
that adds a mention does notify the added person. `buzzx` does not need to
calculate this; it only needs to render the result.

## Deletions

A deletion removes the row with the target id. Deletions are not reversible
in the client. `buzzx` can delete only its own messages, and it shows a
distinct key for that.

## Unread state

The relay does not compute unread. Every client derives it. `buzzx` uses the
desktop app's model, because it is the one that works against this relay.

Two inputs:

1. **Read marker.** Kind 30078, a parameterized replaceable event, encrypted to
   the identity's own pubkey with NIP-44. Its `d` tag is `read-state:<32 hex>`
   and it carries exactly one `["t", "read-state"]` tag. The plaintext is a
   map from context key to unix-seconds read frontier:

```text literal
{"v": 1, "client_id": "<hex16>", "contexts": {"<channel-id>": 1758600000}}
```
   A newer event on the same `(pubkey, d)` replaces the older one server-side.
   So the client sends one marker per channel per read advance, and the blob
   grows by association rather than by history.
2. **Observed events.** Events that arrive while a channel is not selected.

A channel is unread when the newest observed event is newer than its marker.
On channel open, `buzzx` snapshots the frontier, marks the channel read, and
publishes a new marker.

`buzzx` reads the marker set at startup with one query by `(pubkey, kind,
d-tag)` and writes it back on read. It does not keep a local unread database.
The marker is the state, and it is on the relay, so a second terminal shows
the same unread.

This phase is after the first. The first phase shows unread from observed
events only and never claims it is cross-device.

The first phase's observed-only unread is therefore near-zero on a cold start:
with no marker read and no events yet observed, the client has nothing to
count. That is the honest reading, and it is why the first-phase channel list
carries no unread column at all — a number that is only right for channels
you happened to watch is worse than no number. When read markers land, the
column appears.

## Mentions

A mention is a `p` tag whose value is the identity's own pubkey. Stream
messages notify only explicit mentions; a DM p-tags every participant.

`buzzx` highlights a row that carries the identity's own `p` tag. It does not
implement the desktop app's notification rules, because it has no notification
surface. Highlighting is the whole behavior.

## Optimistic sends

When the user sends a message, `buzzx` appends a `pending` row before the
network call. The row carries a local id. On the relay's OK the row is
replaced with the real event. On failure the row is removed and the composer
content is restored.

The optimistic row mirrors the final tag shape, so it reacts to overlays the
same way. It never has a thread reference the real event will not have.

## Rendering order

Rows render in relay order: the order events arrived, with history first and
live appended. `buzzx` does not sort. Sorting a timeline by timestamp across
two channels' worth of live traffic reorders messages that were already
correct, and it hides the case where the relay returns them differently than
the user expects.
