# Mention suggestions and channel creation

Status: implementation-ready product specification. The current `buzzx` baseline
(`dc6c468`) has send-time mention resolution in the shared session, but the CLI
send/reply paths still pass an empty recipient set. Channel creation is not yet
available. This document defines the next synchronized TUI, WebUI and CLI
slice; it does not claim that slice is implemented.

This specification extends the shared behavior in
[`interactive.md`](interactive.md), keeps the community boundary in
[`configuration.md`](configuration.md#community-profiles), and preserves the
architecture in [`../design/architecture.md`](../design/architecture.md).

## Product outcome

The terminal user can:

1. Type `@` in a message or reply and select the intended current-channel
   member from an identity-aware suggestion list.
2. Send the message with a signed `p` tag for the selected public key, or get a
   precise correction while the draft remains intact.
3. Create a channel in the active community from TUI, WebUI or CLI, then open
   it after relay confirmation.

The same identity, relay, membership, reply target, write outcome and
community isolation rules apply in every surface. A visible `@Name` is not
evidence of a notification; the signed recipient tag is.

## Baseline findings

- [`src/session.rs`](../src/session.rs) already performs a member-directory
  lookup and blocks unresolved mentions for shared interactive sending.
- [`src/cli.rs`](../src/cli.rs) currently passes `&[]` mentions to both CLI
  message write paths, so visible names do not become signed recipients.
- [`src/web.rs`](../src/web.rs) exposes pending write state but not the
  structured mention error needed by a browser composer.
- [`src/cli.rs`](../src/cli.rs) only declares `channels list`; the client
  has no channel creation operation in [`src/client.rs`](../src/client.rs).
- [`docs/browse-collab-cli.md`](browse-collab-cli.md) currently excludes
  channel administration. This slice adds channel creation while leaving
  membership administration, invites and archival separate.

## Desktop and Mobile reference

Buzz Desktop separates candidate construction, ranking and editor insertion in
`REPOS/buzz-reference/desktop/src/features/messages/lib/useMentions.ts` and
`REPOS/buzz-reference/desktop/src/features/messages/ui/MentionAutocomplete.tsx`.
Buzz Mobile uses a debounced query provider and an identity-rich suggestion
row in `REPOS/buzz-reference/mobile/lib/features/channels/mentions/mention_candidates_provider.dart`
and `REPOS/buzz-reference/mobile/lib/features/channels/compose_bar/suggestions.dart`.

We adopt their interaction shape, not their broader membership policy:

- `@` at a word boundary opens suggestions; typing filters them.
- Exact name, prefix and word-prefix matches rank ahead of weaker matches.
- Current-channel members rank first. A row can show avatar, display name,
  Agent/admin marker and a short public-key disambiguator.
- Click/tap or arrow keys move selection. Enter selects; it does not submit.
- Selection replaces only the active `@query` and retains a readable display
  name. The selected public key is remembered only for the current draft.
- The final send revalidates the current community, channel membership and
  identity. A stale selection never becomes a hidden recipient.

P0 limits suggestions to members of the current channel, plus identities that
the current relay already exposes as eligible channel members. Global people
search, non-member invitation, teams/personas and cross-community directories
are P1 because they require a separate permission and membership contract.

## Shared mention contract

### Domain values

| Value | Meaning |
| --- | --- |
| `MentionQuery` | The text after the active `@` and its byte/cursor range. |
| `MentionSuggestion` | Display label, public key, avatar, role/Agent marker and stable ranking fields. |
| `MentionBinding` | A draft-local mapping from the inserted label occurrence to a public key. |
| `MentionPreflight` | The final resolver run against the current community and channel immediately before signing. |
| `MentionBlock` | A structured refusal: `unknown`, `ambiguous`, `not_member`, `directory_failed` or `over_cap`, with correction details. |

The message body remains exactly as entered. Bindings and `p` tags are separate
from presentation text. Duplicate public keys collapse. An exact
`nostr:npub...` reference remains supported for CLI and for correcting an
ambiguous name.

### Candidate scope and ranking

Candidate construction uses the current channel member snapshot. The ranking
order is:

1. current members before any future non-member source;
2. exact label match;
3. label prefix match;
4. complete word match;
5. word-prefix match;
6. public-key prefix/substring match;
7. stable source order, then normalized public key.

The list is capped at 50 visible candidates. The cap is a rendering limit;
the final recipient cap remains the SDK limit and is never truncated.

No query, candidate or binding may cross `community_id`, `channel_id` or the
active session `generation`. A relay read failure is different from an empty
member list and must remain visible to the user.

### Send-time truth

Immediately before signing, the shared preflight:

1. checks the draft's active `community_id`, channel and generation;
2. resolves every mention fragment, including fragments not selected from the
   picker;
3. confirms each recipient is still a member of that channel;
4. deduplicates public keys and enforces the recipient cap; and
5. passes the resulting keys to the existing message builder.

Unknown, ambiguous, non-member, failed-directory and over-cap results do not
publish. The draft and reply target stay in place. A confirmed write means the
relay stored the event; it does not mean a recipient read it or an Agent acted.
An uncertain write is never retried automatically.

## TUI behavior

### Trigger and selection

- In composer mode, typing `@` after the start of a line, whitespace or an
  editor-approved boundary opens the current-channel picker. Typing continues
  to update the query; `@` inside an email or code span stays literal.
- The picker is a compact overlay in wide mode and a scrollable temporary
  full-screen view in narrow/minimal modes. It never introduces a second
  timeline column.
- `j/k` or Up/Down moves the cursor. Enter inserts `@Display Name ` and closes
  the picker. Esc closes it without changing the draft. The Enter used to
  select a person never sends the message.
- A selected occurrence remembers its public key until the text occurrence is
  changed, the channel/community changes, or the final revalidation fails.
- `?` explains an unresolved or ambiguous fragment and shows complete exact
  `nostr:npub...` references where useful. It never relies on color alone.

### TUI states

| State | Visible behavior | Allowed actions |
| --- | --- | --- |
| `closed` | Ordinary composer | Type, open picker, submit if no picker is active |
| `loading` | `Loading members…` | Continue typing, Esc, return to draft |
| `open` | Ranked suggestions | Move, select, dismiss |
| `empty` | `No matching members` | Edit query, dismiss |
| `directory_failed` | `Could not load members; retry` | Retry lookup, keep draft |
| `blocked` | Short send refusal in status line | `?` details, edit and retry |

### TUI creation entry

`Ctrl+P` opens the existing command palette. **Create channel** opens a form
with Name, Type, Visibility and Description. The form uses the current
community shown in the header and does not add a new global key.

`Creating…` disables duplicate submit. `Created` refreshes the roster and
opens the new channel. `Failed` keeps editable fields. `Unknown` says that the
channel may exist and requires an explicit refresh; it never retries creation.
Pending or uncertain creation blocks community switching until the result is
resolved.

## WebUI behavior

The browser uses the same session action and generation checks. It never signs,
accepts a relay URL or receives auth material.

### Composer

- Typing `@` at a valid boundary opens a popover below or above the composer.
- The popover supports pointer selection, Up/Down, Enter and Esc. Enter
  inserts a display name and leaves sending to the next submit action.
- Rows show avatar, name, Agent/admin marker and short-key disambiguation for
  collisions. An older generation's suggestions or error cannot replace the
  active community's state.
- Browser state exposes `mention_suggestions` with `query`, `items`,
  `selected_index` and `loading`, and `mention_block` with `kind`, `summary`,
  `details`, `community_id`, `channel_id` and `generation`.
- A blocked send keeps text and reply target beside the composer and offers
  edit/retry. A pending submit disables a second submit for that operation.

### Channel creation

Wide WebUI shows `+ Create channel` beside the Inbox/sidebar list. Narrow
WebUI shows the same action in the Inbox menu while the app bar keeps the
current community selector visible. The form fields and states match TUI.

## CLI contract

CLI is a non-interactive surface, so it has no suggestion popover. It accepts
complete unique names and exact `nostr:npub...` references, then runs the same
final preflight.

### Message writes

```text literal
buzzx messages send --channel <channel-id> --content "@Buzzx Build 请看这个" [--community <profile-id>]
buzzx messages reply --event <event-id> --content "@Buzzx Product 已完成" [--community <profile-id>]
```

Success JSON includes `community`, `status`, `event_id` and
`mention_pubkeys`. A blocked write returns `status: "not_sent"`,
`mention_block` and a non-zero exit code. It must never report success for a
message whose signed event omitted the resolved recipients.

### Channel creation

```text literal
buzzx channels create \
  --name "项目讨论" \
  --type stream \
  --visibility open \
  [--description "可选说明"] \
  [--community <profile-id>]
```

`--community` selects a saved profile; without it, the active profile is used.
The existing `--relay`/environment precedence and conflict errors remain in
force. Name is required and trimmed. Type is `stream|forum`, defaulting to
`stream`; visibility is `open|private`, defaulting to `open`; description is
optional.

The JSON result includes `community`, `status`, `channel_id` when known and
the submitted fields:

- `created_confirmed`: relay confirmed the canonical channel event.
- `created_unconfirmed`: the write may have happened; refresh the current
  community and do not submit an automatic duplicate.
- `not_created`: local validation or relay refusal established no channel.

The client uses the existing relay/channel creation contract. The relay, not a
local optimistic cache, establishes channel id, creator ownership and initial
membership. Member invites, join/leave, TTL and archive are separate P1 work.

## Shared implementation boundary

The implementation must keep the architecture's one-way ownership:

| Area | Responsibility |
| --- | --- |
| `client.rs` | Member/profile query, channel-create event builder call, final mention preflight inputs and write classification. |
| `session.rs` | Async lookup, generation/community guards, candidate/block events, send and create commands, pending/uncertain protection. |
| `app.rs` | Draft-local query, picker cursor, binding invalidation, creation form and state transitions. |
| `keys.rs` | TUI picker/form actions; composer text remains text in composer mode. |
| `ui.rs` | TUI candidate list, status/detail wording and channel form rendering. |
| `web.rs` | Browser action/update schema, structured suggestions/block state and create-channel form actions. |
| `cli.rs` | `channels create`, shared message preflight invocation, stable JSON and exit codes. |

`app.rs` and `ui.rs` never await the relay. The browser never implements a
second resolver or event builder. Candidate reads are cancellable or
generation-tagged, and stale results are discarded rather than merged.

## State and failure contract

Mention picker: `Closed → Loading → Open|Empty|DirectoryFailed`; selecting a
row returns to `Closed`; submit enters the existing write states. A blocked
preflight returns to the draft with `MentionBlock`.

Channel creation: `Form → Validating → Creating → Created|Failed|Unknown`.
`Unknown` is terminal for that operation until an explicit refresh or lookup
establishes the result. It is not permission to retry.

Community switching is blocked while any tab/session view has a pending or
uncertain mention or channel write. A switch never carries a draft binding or
late relay result into the target community.

## Delivery slices and dependencies

| Issue | Depends on | Completion condition |
| --- | --- | --- |
| M1. Shared mention candidate model and ranking | current member/profile query | Pure fixtures cover trigger boundaries, ranking, collisions, cap and stale generation. |
| M2. TUI smart mention picker | M1 | Four required terminal sizes support query, selection, dismissal, binding invalidation and readable failure states. |
| M3. Web smart mention picker/state | M1, existing Web session | Browser popover and structured state match TUI semantics across community switch and tab generations. |
| M4. CLI mention preflight parity | shared preflight | `send` and `reply` produce signed `p` tags or `not_sent` JSON with no silent drop. |
| C1. Channel-create client and CLI | relay contract, existing write outcomes | `channels create` validates fields and returns confirmed/uncertain/not-created JSON. |
| C2. TUI channel-create form | C1 | Command palette form refreshes and opens only confirmed channels; unknown never auto-retries. |
| C3. Web channel-create form | C1, existing Web session | Wide/narrow entry points share fields, states and generation guards. |
| A1. Cross-surface relay acceptance | M2, M3, M4, C2, C3 | Two communities and two identities verify recipients, creator membership, isolation and unknown-write behavior. |

The dependency order is M1 → M2/M3/M4 and C1 → C2/C3, then A1. M and C
can proceed in parallel after their shared session/write contracts are agreed.

## Acceptance matrix

1. At every required TUI size and in wide/narrow WebUI, `@` opens suggestions;
   exact matches rank first, same-name rows are distinguishable, Enter selects
   without sending, and Esc preserves the draft.
2. A selected or manually typed member name produces exactly that public key in
   the signed event's `p` tags; body, root and parent stay unchanged.
3. Unknown, ambiguous, non-member, failed-directory and over-cap drafts are
   not published; details identify the correction and no stale binding leaks
   across channel/community changes.
4. CLI send/reply has the same recipient semantics and JSON write outcomes as
   TUI/WebUI. A relay refusal and an uncertain response never trigger a retry.
5. Create a channel from CLI, TUI and WebUI in each of two communities. The
   creator sees it after confirmation and roster refresh; same names remain
   separate channel ids and caches.
6. Empty/invalid fields, unavailable identity, permission refusal and lost
   creation responses produce the specified failure state; unknown creation is
   refreshed explicitly and never auto-submitted again.
7. A community switch is blocked during pending/uncertain writes. Old
   generation events, suggestions, errors, drafts and channel rows never
   appear as data for the new community.
8. Live relay evidence reads the stored event tags, recipient mention feed,
   canonical channel id, creator membership and private/open visibility. Unit
   and fake-relay tests supplement but do not replace this evidence.

## Success signals and non-goals

The first release is successful when users stop seeing a message that looks
like a mention but has no recipient tag, and can create a channel without
leaving the active surface. Track blocked mention corrections, confirmed
mention readbacks, channel creation confirmation rate and unknown-write
frequency by surface; do not infer notification delivery or Agent execution
from these numbers.

This slice does not add cross-community inboxes, global people search,
automatic invitation, member administration, forum-specific authoring, DM
creation, persisted drafts, offline writes or a new relay protocol.
