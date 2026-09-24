# Decision: 0001 - Two transports, with the HTTP bridge for writes

## Status

accepted

## Context

A Buzz relay exposes an HTTP bridge and a Nostr WebSocket. The relay splits
persistent events from ephemeral traffic itself: a write to `POST /events`
receives full tag validation, while the WebSocket path accepts the same kinds
with less validation and rejects the ephemeral range outright.

`buzzx` must send messages, edits, deletions, reactions, and presence, and
must subscribe to channels live. The question was whether one transport can
serve both directions, or whether the relay's own split must be mirrored in the
client.

## Decision

Mirror the relay's split. The HTTP bridge carries every persistent event, both
reads and writes. The WebSocket carries the live feed and the ephemeral events
only.

Concretely:

- Reads and writes go through `POST /query` and `POST /events`, each signed
  with a NIP-98 event.
- One long-lived NIP-42 WebSocket connection accepts a challenge, subscribes,
  and stays open.
- Ephemeral kinds, 20001 presence and 20002 typing, are published on the live
  connection with `send_event`.

The `buzz` CLI opens a fresh connection per publish. That is correct for a
one-shot command and wrong for an interactive client, so `buzzx` keeps the
single connection open for the life of the session.

## Consequences

- Full tag validation applies to everything `buzzx` sends, including
  attachments, emoji, and mention overlays. This is the stricter path, chosen
  deliberately: the validation is free and it removes a class of silent
  rejection that the desktop app's own history shows.
- A send costs two round trips, one to sign and one to post, before the
  response arrives. The command pump absorbs the latency; the render loop does
  not await it.
- The write path depends on `buzzx` implementing NIP-98 itself, because
  `buzz-cli`'s `BuzzClient` and `sign_nip98` are private. That is a known
  duplication. It is recorded, and it should be deleted the moment upstream
  makes those items public.
- A relay that is behind a proxy which drops WebSocket upgrades still allows
  reads and writes, so `buzzx` degrades to a request-and-response client rather
  than failing.

## Alternatives considered

**One transport over the WebSocket for everything.** This is the shape the
`buzz` CLI almost uses, and it is less code: one connection, one event
pipeline, no NIP-98 implementation. It was rejected because the WebSocket path
does not apply full tag validation, so an event with attachments could be
accepted here and refused on another client's retry. The relay's validation is
part of the contract; bypassing it because a transport is convenient defeats
the purpose.

**Two transport abstractions behind one facade.** This hides the choice from
the callers, which was attractive while the pump was being written. It was
rejected because the two transports have genuinely different failure modes,
different URL forms, and different auth rounds, and a facade that pretends
they are the same has to lie about at least one of those. The callers are a
command pump and a WebSocket pump; they know which transport they are on, and
`session.rs` is the only module that sees both.

**Publishing presence over HTTP.** The ephemeral kinds are WebSocket-only, so
this was never available. It is listed because a naive implementation reading
`buzz-cli` first will look for an HTTP route and waste the time discovering
this. The relay rejects the ephemeral range with a clear reason; see
`design/relay-transport.md`.
