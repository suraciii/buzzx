# Browse and collaborate from the buzzx CLI

## Problem

`buzz` is useful for one-off relay operations, but its command surface is not
owned by this project and does not give every terminal user or agent a simple,
discoverable collaboration path. `buzzx` is the Buzz extension CLI (`buzz
ext`), so it can fill that product gap while preserving Buzz's relay,
identity, authorization, and event semantics.

The first useful path is deliberately small: discover a channel, read its
recent messages, send a message or reply, and read the resulting event back.
Every step must return enough context for an agent or shell script to pass the
result to the next step without scraping human-oriented text.

## Goal

Enable this complete flow from a non-interactive terminal:

1. Discover channels for the current identity.
2. Read recent messages in one channel, or read a thread.
3. Send a new message or reply to an existing event.
4. Confirm the resulting event id and its channel/thread context.

The commands are one-shot JSON operations. They complement the held TUI
session and do not change the TUI's interaction model.

## Scope

### In scope

- `channels list`
- `messages get`
- `messages thread`
- `messages send`
- `messages reply`
- JSON output with stable identifiers and write status
- Message content from an argument or stdin (`--content -`)
- Explicit error categories and unknown write-result handling

### Out of scope

Agent creation or lifecycle management, channel administration, full-text
search, `watch`, a new protocol (MCP/A2A), persistence of runs, and a TUI
redesign. Those capabilities may use this contract later, but they are not
requirements for this slice.

## User flow

The following is the canonical agent flow. UUIDs and event ids are examples.

```text diagram
buzzx channels list
  -> channel_id=7e5f... , name="buzzx-cli"

buzzx messages get --channel 7e5f... --limit 20
  -> event_id=f8e3... , thread_root=null

buzzx messages reply --event f8e3... --content -
  -> status=sent_confirmed, event_id=91ab... , reply_to=f8e3...

buzzx messages get --event 91ab...
  -> the accepted event, with its canonical channel and thread fields
```

`messages reply` accepts an event id as its only routing input. It derives the
channel and thread root from the referenced event or returns a categorized
error when that event cannot be resolved. `messages send` requires an explicit
channel and creates a top-level message.

## Command behavior

| Command | Required input | Result |
| --- | --- | --- |
| `channels list` | identity | Channels the identity can access |
| `messages get` | `--channel`, or `--event` for one event | Recent channel messages, or one canonical event |
| `messages thread` | `--event` | Root event and replies in stable order |
| `messages send` | `--channel`, content | A top-level message write result |
| `messages reply` | `--event`, content | A reply write result with derived routing |

All read commands are safe to repeat. They return an empty collection when a
valid channel has no matching messages. They never turn a permission failure
into an empty result.

Content may be supplied as a normal argument or with `--content -`, which
reads bytes from stdin and preserves newlines. The CLI rejects missing content
before signing an event. It must not silently truncate content.

## JSON contract

The default output is JSON. Collection commands return an array; a single
event returns an object. Each event object includes these fields when they are
known:

| Field | Meaning |
| --- | --- |
| `channel_id` | Buzz channel UUID |
| `event_id` | Canonical signed event id |
| `author` | Pubkey of the event author |
| `created_at` | Relay event timestamp |
| `thread_root` | Root event id, or `null` for a top-level event |
| `reply_to` | Direct parent event id, or `null` |
| `content` | Message content for read results |

Write commands additionally return `status` and, when available, `event_id`.
The field names and enum values are stable so callers can branch without
parsing prose. Human-readable diagnostics may be printed to stderr, but stdout
must remain machine-readable JSON.

## Write-result semantics

Every write has exactly one of these outcomes:

- `sent_confirmed`: the relay accepted the signed event and returned its
  canonical event id.
- `sent_unconfirmed`: the client submitted the write, but the response was
  lost or timed out before acceptance could be confirmed. The event may exist.
- `not_sent`: validation, authorization, or a transport failure happened before
  submission was known to occur.

The CLI never retries `sent_unconfirmed` automatically. A caller may inspect
the channel or query the returned event id when one is available, then decide
whether to retry. This prevents duplicate messages when the network result is
unknown.

## Error contract

Failures use a stable `error` category and a non-zero exit code:

- `invalid_input`: missing, malformed, or contradictory arguments
- `not_found`: channel or referenced event does not exist
- `forbidden`: identity is not authorized for the requested operation
- `network`: connection failed before a write was submitted
- `timeout_unknown`: write submission may have happened, but confirmation timed
  out; the result must be represented as `sent_unconfirmed`
- `relay_rejected`: the relay refused a submitted event; represent the write as
  `not_sent` only when the relay confirms it did not store the event

Errors identify the relevant `channel_id` or `event_id` when one is known.
They do not include private keys or raw authorization headers.

## Acceptance criteria

1. A new user can run `buzzx help` and discover the five commands and their
   required inputs without reading source code.
2. The canonical flow completes using only JSON output and the ids returned by
   prior commands.
3. `messages reply --event <id>` derives the channel and thread context and
   returns `reply_to` equal to the supplied event id.
4. A successful send returns `sent_confirmed` and a canonical event id that can
   be read back.
5. A permission failure returns `forbidden`, a non-zero exit code, and no
   apparent successful write.
6. A lost confirmation returns `sent_unconfirmed`; the CLI does not submit a
   second event automatically.
7. `--content -` preserves multi-line input, and invalid empty input is
   rejected before submission.
8. Read commands preserve the event's channel, author, timestamp, thread root,
   and direct-parent identifiers.

## Product boundary after this slice

This contract establishes the reusable browse-and-collaborate path for
terminal users and agents. Agent drafts, request-status, channel management,
search, and live `watch` remain separate product slices with their own
contracts. Implementations should extend these fields and statuses only when a
later slice requires them.
