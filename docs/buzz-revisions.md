# Buzz Revisions

Pinned Buzz crate revisions used by `buzzx`.

## buzz-core, buzz-sdk, buzz-ws-client: 93114c9c65138397de39729fde0a816eb9f314ab

`buzzx` depends on three Buzz crates through a pinned git revision. This
document records what each crate provides and why `buzzx` needs it. A bump is
a deliberate change: check the upstream diff, then update this document and
`Cargo.toml` in the same commit.

The pinned revision is `93114c9c65138397de39729fde0a816eb9f314ab`.

### What each crate provides

- `buzz-core` provides kind constants, for example
  `kind::KIND_STREAM_MESSAGE`; the `observer::{encrypt, decrypt}` pair for the
  NIP-44 read marker; and `verify_event`.
- `buzz-sdk` provides `builders::{build_message, build_edit,
  build_delete_message, build_reaction, build_presence_update}`,
  `extract_channel_id`, and the `mentions` pipeline.
- `buzz-ws-client` provides `NostrWsConnection::{connect_authenticated,
  send_raw, send_event, next_event, disconnect}` and `RelayMessage` decoding.

### Facts confirmed in the Buzz source

These were read out of the Buzz repository and hold until Buzz changes:

- `buzz-core` and `buzz-sdk` carry no tokio, sqlx, redis, or axum. The chain
  is closed on registry crates.
- `buzz-ws-client` has no internal Buzz dependencies.
- `buzz-sdk` has no builder for kind 20002 (typing) or kind 30078 (read
  marker). `buzzx` builds both by hand.
- `build_message(channel_id, content, thread_ref, mentions, broadcast, media_tags)`
  emits `h`, then thread `e` tags, then `p` tags, then `broadcast`, then
  `imeta`. A direct reply emits one `e` tag; a nested reply emits `root` then
  `reply`, in that order.
- The relay accepts kind 9 and kind 40002 identically. `buzzx` sends kind 9,
  because the relay's `link-preview` tag validation applies only to kind 9 and
  `build_message` produces kind 9.
- `buzz-cli`'s HTTP client (`BuzzClient`, `sign_nip98`) is private. `buzzx`
  implements NIP-98 itself. If upstream makes it public, delete `http.rs` and
  depend on it.

### Known upstream coupling

`buzzx` pins `nostr = "0.44.x"` because the Buzz crates do. A buzz version
that moves to a different nostr major bumps this pin.

The relay canonicalizes a submitted event and may store it under a different
id than the client signed. `POST /events` answers with
`{"accepted":true,"event_id":"<stored id>","message":""}`, and that stored id
is the address a later edit, deletion, or reply must target. `buzzx` takes
the id from the response, never from the locally signed event.
