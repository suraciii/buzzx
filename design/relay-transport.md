# Relay Transport

This document defines how `buzzx` talks to a Buzz relay: authentication, the
two transports, the subscription lifecycle, and the limits that constrain the
client. It is the contract that `http.rs`, `sub.rs`, and `session.rs`
implement.

## Surface

A Buzz relay exposes these paths. The WebSocket root accepts a NIP-01
connection, and NIP-42 authentication is required before any REQ.

- `POST /query` takes one Nostr filter and returns the matching events. It
  needs a NIP-98 header.
- `POST /count` takes one Nostr COUNT filter. It needs a NIP-98 header.
- `POST /events` takes one signed event. It needs a NIP-98 header.
- `GET /media/{sha256}` downloads Blossom media. It needs an event in the
  query, in the style of NIP-98.
- `PUT /upload` uploads Blossom media. It needs a Blossom 24242 auth event.

`buzzx` uses the WebSocket, `/query`, and `/events` in the first phase. Media
comes later.

## Identity

The identity is one Nostr keypair. It resolves in this order:

1. `BUZZ_PRIVATE_KEY`, then `--private-key`.
2. `~/.config/buzzx/config.toml`.

The key is hex or `nsec`. `buzzx` fails at startup when neither source has a
key. It never prompts, because a prompt blocks the TUI's raw-mode loop and has
no way to mask input correctly in all terminals.

The relay URL resolves the same way: `BUZZ_RELAY_URL`, then `--relay`, then
the config file, then `http://localhost:3000`. A relay URL may be `ws://`,
`wss://`, `http://`, or `https://`; `buzzx` normalizes it to both forms.

An optional NIP-OA `auth` tag comes from `BUZZ_AUTH_TAG`. When present it is
attached to every signed event and sent as an `x-auth-tag` header on HTTP
requests. This is the same contract the `buzz` CLI uses, so an agent identity
that works with `buzz` works with `buzzx`.

The event tag is the channel that cannot fail. The relay resolves the
attestation as `event_tag.or(header)`, so the tag on the signed event wins
wherever both are present, and it is the only form the git smart-HTTP path can
carry — a credential helper cannot pass a standalone header. The header is a
convenience for HTTP clients, never a second source of truth.

`buzzx` installs the `ring` rustls crypto provider at startup, before any TLS
use. Without it a multi-crate build that unifies `ring` and `aws-lc-rs`
panics inside rustls. This is a required call, not a guard.

## NIP-98: the HTTP layer buzzx owns

`BuzzClient` in `buzz-cli` is the working reference for this. It is private to
that crate, so `buzzx` implements the same protocol in `http.rs`.

For each request:

1. Build a kind 27235 event with empty content and these tags:
   - `u` — the full request URL, including query string.
   - `method` — the HTTP method, uppercase.
   - `nonce` — a random UUID v4. The relay keeps a short replay guard, and the
     nonce is what makes two identical bodies two different events.
   - `payload` — the SHA-256 of the request body, hex. Present only when the
     request has a body.
2. Sign the event with the identity key.
3. Serialize the event to JSON, base64 it, and send
   `Authorization: Nostr <base64>`.

Reads use one filter per call:

```text literal
POST /query
{"kinds": [9, 40002, 40008, 45001, 45003], "#h": ["<channel-uuid>"], "limit": 100}
```

Writes submit one signed event:

```text literal
POST /events
<signed nostr event JSON>
```

The relay runs the same ingest validation on this path as on the WebSocket
path. The client validates what it can before sending, but the relay is the
authority. A client check is convenience, never enforcement.

Retry is narrow. A connection failure or a read timeout may be retried once.
A write is not retried when the result is unknown: a timeout after the request
was sent may mean the event was admitted. `buzzx` reports uncertainty and does
not resend. Resending a message duplicate-posts it.

## WebSocket: the live path

The WebSocket connection carries two things: ephemeral publishes that HTTP
rejects, and the live subscription.

### Connection and NIP-42

1. Connect to `wss://<relay-host>/`.
2. The relay sends `["AUTH", "<challenge>"]`. It allows 5 seconds.
3. `buzzx` replies with `["AUTH", <signed kind 22242 event>]`. The event cites
   the relay URL and the challenge, and carries the NIP-OA `auth` tag when the
   identity has one.
4. On `["OK", ..., true]` the connection is authenticated. On `false` the
   reason is shown and the connection is not used.

`buzz-ws-client` provides this as `connect_authenticated`. Its client-side
timeouts (20 s for the challenge, 20 s for the OK) are longer than the relay's
5 s, so a timeout means the relay is unreachable rather than strict.

No request is accepted before authentication. There is no anonymous subscribe
mode. This is why the TUI shows a connecting state before it shows channels.

### Subscription lifecycle

One subscription per channel, plus one global subscription:

```json
["REQ", "<sub-id>", {"kinds": [9, 40002, 40008, 45001, 45003], "#h": ["<uuid>"], "limit": 1000, "since": <now>}]
```

Lifecycle:

1. Send REQ after authentication.
2. Relay returns matching history, then `["EOSE", "<sub-id>"]`. History before
   EOSE is the catch-up; events after EOSE are live.
3. The TUI sends new events into the channel's rows and stops treating the
   channel as loading.
4. On reconnect, REQ is sent again from scratch. Reconnects are full
   resubscribes, not resumes.

Auxiliary events — reactions (kind 7), edits (40003), deletions (5, 9005) —
are fetched by reference, not by channel:

```json
["REQ", "aux", {"kinds": [7, 40003, 5, 9005], "#e": ["<id1>", "<id2>", "..."]}]
```

Ids are chunked at 100. Auxiliary events are never in the channel REQ, so a
wave of reactions cannot dilute the history window.

Typing indicators are ephemeral, so they are subscribed per channel and never
fetched by reference:

```json
["REQ", "y:<uuid>", {"kinds": [20002], "#h": ["<uuid>"]}]
```

One REQ per member channel, because the relay indexes live fan-out by a single
`#h` value: a filter that names several channels falls back to a global
subscription and receives no channel event at all, ephemeral or stored. The
filter carries no `since` and no `limit` because nothing is stored: both would
only describe a history that does not exist. The channel set comes from the
membership roster, so the feed is replaced whenever the channel list reloads —
including after every reconnect — and an empty roster closes it. One REQ per
channel is affordable because the relay advertises 1024 subscriptions per
connection: a timeline REQ is one more, and the count stays inside a
membership the roster can plausibly hold.

The typing feed is the only subscription whose channel list is the identity's
whole membership; a timeline REQ carries one channel. That is what the channel
list's activity marker reads, and it is why the marker is right for a channel
the user is not looking at.

The global subscription covers what is not channel-scoped and what the
identity must see: read-marker confirmations, member notifications, and the
presence snapshot. It is filtered with `#p` set to the identity's own pubkey,
which is what the relay's p-gated filter requires.

### Pump

`buzz-ws-client` is a pull connection: `next_event(timeout)` returns one
`RelayMessage`. There is no callback and no channel. `buzzx` therefore runs one
tokio task per connection that loops on `next_event`, decodes each message, and
forwards a `ChatEvent` to the UI. The pump owns the socket; nothing else
touches it.

The pump answers these frames:

| Frame | Action |
|---|---|
| `EVENT` | Decode. Timeline kinds become rows; aux kinds become overlays; a typing indicator into the line above the composer. |
| `EOSE` | End the channel's loading state. |
| `NOTICE` | Show as status. |
| `CLOSED` | Report and stop that subscription. A `restricted:` reason means membership was lost. |
| `AUTH` (late) | Stash and re-authenticate, if the relay re-challenges. |
| `OK` | Report publish result. A `false` with a reason is shown to the user. |

## Ephemeral events

Kind 20001 (presence) and 20002 (typing) are in the ephemeral range
20000–29999. The relay rejects them over HTTP. They are WebSocket-only, and
they are never stored: they go to Redis fan-out and are gone.

Both are published on the live connection with `send_event`, not by opening a
new connection per publish. The `buzz` CLI opens a fresh connection for each
one, which is fine for a one-shot command and wrong for an interactive client.

Consequences of ephemerality:

- Presence is rebuilt after every reconnect by querying kind 20001 or the
  relay-synthesized 40902 snapshot.
- Typing state expires on its own. The consumer keeps an entry for 8 seconds
  and drops it. The publisher sends at most one indicator per 3 seconds per
  channel.

`buzzx` consumes typing indicators and does not publish them. One indicator
carries the channel in its `h` tag; any thread tags on it are ignored, so the
state is per channel and not per thread. The consumer holds an entry for 8
seconds, refreshing the deadline on every repeat, and renders the whole set as
one line. An entry also ends before its deadline: the author's own message in
that channel ends it, a disconnect drops every entry, and a closed channel
drops its own. Indicators from the identity itself are ignored, whichever
client published them. Nothing about typing is persisted, so a restart starts
with none.

## Rate limits

The relay's default limits for a human identity:

| Limit | Value |
|---|---|
| Messages per minute | 60 |
| API calls per minute | 300 |
| WebSocket events per second | 10 |

The WebSocket admission check is a fixed window of 5 seconds, so a burst of
about 50 events per window is admitted before throttling.

`buzzx` stays under these by design:

- Typing is consumed, never published: one REQ per channel set, no publish
  budget at all.
- Presence heartbeat is one publish per 60 seconds, only while the TUI is
  focused.
- Auxiliary backfill chunks are 100 ids and run once per channel open.

A relay that returns `rate-limited:` is shown to the user with the
relay's own retry hint. `buzzx` does not back off silently; the user needs
to know the message was not sent.

## Failure handling

| Failure | Behavior |
|---|---|
| Relay unreachable at startup | Exit with a clear reason and exit code 2. |
| NIP-42 rejected | Show the relay's reason. Exit code 3. |
| WebSocket drops mid-session | Reconnect with jitter. Re-REQ all channels. History is refetched. |
| Channel becomes inaccessible | The relay closes the subscription with `restricted:`. The channel is removed from the list and the selection moves. |
| Write result unknown | Report as uncertain. Do not retry. |
| Not a member of the channel | The relay refuses the write. Show the reason; keep the composer content so it can be edited and retried. |

Reconnect is bounded: full jitter, capped backoff, and no more than one
reconnect per 25 seconds. The channel list and loaded rows survive a
reconnect; only the live feed is re-established.

## What is not here

Media upload and download, huddle audio, search, and moderation are out of
scope for the first phase. Each is a relay surface `buzzx` can add without
changing this document's structure.
