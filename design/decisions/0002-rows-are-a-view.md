# Decision: 0002 - Rows are a view, not a stored timeline

## Status

accepted

## Context

A Buzz relay stores events keyed by id. It does not order them into a channel
timeline, does not compute a thread, and does not compute a reply count. Every
client builds its own view, and Buzz ships three clients that already do this.

`buzzx` had to choose between mirroring the desktop app's in-memory model,
building a timeline out of the database a relay owns, or deriving the view
fresh from whatever events arrive in a session.

## Decision

A row is a per-session view of one event. The client keeps a `Vec<Row>` per
channel, appends to it, and mutates it in place when an overlay arrives.

Concretely:

- A timeline kind in `design/render-contract.md` becomes exactly one row.
- Auxiliary kinds never produce a row. A reaction, an edit, and a deletion are
  applied to an existing row by event id.
- Reaction counts start at zero each session and are derived from observed
  events. The client does not backfill a globally correct count.
- Unread state derives from the kind 30078 read marker plus the events that
  arrive while a channel is not selected. No client computes unread for
  another client.

## Consequences

- A reaction count is correct for what the operator watches. It is not correct
  for the channel's full history, and it is never presented as such. This is a
  stated limitation, not a defect.
- The memory held by a session is the events it has seen. A long session with
  many channels holds a lot. The 1000-line file ceiling forces the rows into a
  small focused module, and the length of a channel's history is the only
  thing that grows.
- Overlays are applied by id, so an edit or a deletion for an id the client has
  not seen is dropped rather than queued. This is correct: the row will be
  loaded by the history REQ before it can be replied to, and if it is not
  loaded there is nothing to overlay.
- Threading is NIP-10 marker tags, read in order, and the reply target is
  carried in the composer. The relay does not maintain thread structure.

## Alternatives considered

**Backfilling reaction counts with a query per visible row.** This produces a
globally correct count and costs one request per row, on every render, for a
number that a human glances at. The desktop app's own history is a list of
reaction events, not a count, which is the same tradeoff taken at a larger
scale. Rejected on cost and on the second source of truth it creates.

**A materialized timeline stored by `buzzx` itself.** This would make
reconnects instant and would let a second session resume the first. It was
rejected because the product is a single session at a time, the relay is the
store of record, and a local cache invents a second truth that the relay is not
informed of. The reconnect path re-issues the REQ instead, which is correct
because the relay is authoritative.
