# Decision: 0006 - Read state uses per-client encrypted slots

## Status

accepted

## Context

`buzzx` needs read progress that survives a terminal session and is visible to another terminal using the same identity. A failed lookup must not turn missing data into a claim that a conversation is read. The marker must not disclose per-conversation read times in plaintext.

Use one deterministic, encrypted slot per client. It is a NIP-44-encrypted
kind 30078 event. The `d` tag is `read-state:` plus the hex encoding of the
first 16 bytes of SHA-256 over the identity's public-key hex. The `t` tag is
`read-state`. Its version 1 payload carries `contexts`, keyed by channel UUID
and valued by Unix-second frontier. Before each write, read all owned slots
and merge each context by its maximum. (Sources: `src/read_state.rs::slot`,
`builder`, `parse`; `src/client.rs::read_state`, `publish_read_state`.)

## Consequences

- Terminals using this protocol see one another's progress. Read-back plus
  per-context maximum merging prevents a write from replacing a later
  frontier with an older one.
- A failed read-back, an unreadable slot, or a failed write remains unknown or
  unsynced. An uncertain write is not retried automatically; the read pump
  makes no retry. A later, separate read update may cause another publish.
- Unknown coverage remains distinct from read; a failed lookup is not treated
  as proof that no messages are unread.
- A conversation with no marker starts from a local seed at its newest
  message. The seed is not uploaded as proof of reading.
- Marker timestamps have one-second resolution. A message delivered in the
  frontier's own second remains unread until shown.

## Alternatives considered

**Keep read time local only.** Rejected: another terminal would not share progress, and a new session would have no durable frontier.

**Use one encrypted slot per client.** Chosen: each terminal can own a deterministic replaceable slot, while read-back and per-context maximum merging preserve progress across clients without publishing channel frontiers in plaintext.

**Publish one shared plaintext marker.** Rejected: channel identifiers and read times would be visible to relay readers, and simultaneous clients could overwrite one another's contexts without read-back merging.

## Does not cover

This decision covers read progress only. It does not represent task completion, deferred work, approvals, or agent status. It does not make a failed lookup complete, guarantee that every relay retains the marker, or retry an uncertain write. The lookup is bounded to seven days and 500 events; that window is a query bound, not marker expiry.
