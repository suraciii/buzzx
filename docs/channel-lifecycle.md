# Channel lifecycle and membership management

Status: implementation-ready product specification. This document extends
the channel-creation contract in
[`mentions-and-channel-creation.md`](mentions-and-channel-creation.md). It
defines the smallest useful channel administration slice for TUI, WebUI and
CLI; it does not claim that the slice is implemented.

## Product outcome

People can create a channel, keep its metadata correct, archive it without
losing history, restore it later, and manage its members from the active
community. The same channel, role, permission, write-result and community
rules apply to every surface.

The stable domain reference is `(community_id, channel_id)`. A channel name is
presentation only and may be reused in another community. No member lookup,
permission check, draft, cache, pending write or relay event may cross that
pair or the active session `generation`.

## Baseline and protocol evidence

The current `buzzx` baseline can list channels and read channel membership,
but it has no channel-management client methods or `channels` write commands.
`ChannelInfo::listed()` already excludes archived channels, so archive is a
visibility and write-state gap rather than a new channel identity model. See
[`../src/client.rs`](../src/client.rs), [`../src/content.rs`](../src/content.rs)
and [`../src/cli.rs`](../src/cli.rs).

The local Buzz reference uses the following NIP-29 command events. The relay is
authoritative for authorization and resulting state; the client must not fake a
membership or metadata change in its local cache.

| Operation | Kind | Required tags | Meaning |
| --- | ---: | --- | --- |
| Create | `9007` | `h`, `name`, `visibility`, `channel_type`; optional `about`, `ttl` | Creates a channel and establishes the creator's owner/member state. |
| Update metadata | `9002` | `h`; optional `name`, `about`, `visibility`, `ttl` | Changes editable metadata. Channel type is immutable in P0. |
| Archive / unarchive | `9002` | `h`, `archived=true|false` | Hides or restores the channel without deleting history or membership. |
| Add member / set role | `9000` | `h`, `p`; optional `role` | Adds an identity or changes its role. |
| Remove member | `9001` | `h`, `p` | Removes another identity. |
| Join | `9021` | `h` | Requests membership in an open channel. |
| Leave | `9022` | `h` | Removes the signed-in identity's own membership. |
| Delete | `9008` | `h` | Destructive channel deletion; P1 and separately confirmed. |

The protocol shape is based on the local reference builders and client flows:
`REPOS/buzz-reference/crates/buzz-sdk/src/builders.rs`,
`REPOS/buzz-reference/crates/buzz-cli/src/commands/channels.rs` and
`REPOS/buzz-reference/mobile/lib/features/channels/channel_management_actions.dart`.
These files are evidence for event shape and interaction, not a promise that
the reference clients' wider policy is copied into buzzx.

## Scope and priority

### P0: reversible lifecycle and membership

| Capability | User result | Default authorization |
| --- | --- | --- |
| Create | New channel appears only after relay confirmation and roster refresh. | Authenticated identity; relay may impose community policy. |
| View details | Name, description, type, visibility, archive state and current role are readable. | Any identity that can read the channel. |
| Update name/description/visibility | Metadata changes are visible after readback. | Owner or admin. |
| Archive / unarchive | Active channel becomes read-only and leaves the normal list; unarchive restores it. | Owner or admin. |
| List members | Shows pubkey, display name when available, role and membership state. | Any channel member; relay may redact private data. |
| Add members | Adds one or more selected identities with an explicit role. | Owner or admin. |
| Change member role | Changes one selected member's role. | Owner or admin; owner changes require relay policy. |
| Remove member | Removes another member after explicit confirmation. | Owner or admin; last-owner protection is mandatory. |
| Leave | Signed-in identity leaves the channel. | Any member except a last owner; relay is authoritative. |
| Archived view/filter | Finds an archived channel to inspect or restore. | Existing channel access. |

P0 does not infer permission from a button's presence. The UI may hide or
disable actions using the current role snapshot, but every write still handles
a relay refusal and refreshes authoritative state.

### P1: deliberate expansion

- Discover and join open channels not yet in the member roster. Private channels
  remain invitation-only. This requires a channel discovery/query contract,
  because the current `buzzx` list is membership-based.
- Delete a channel (`9008`). Deletion is not an alias for archive: it may make
  history and membership inaccessible and needs an owner-only, two-step
  confirmation with a name/channel-id check.
- TTL/ephemeral channels, topic and purpose fields. They use the reference
  `ttl`, `topic` and `purpose` tags but need product copy, expiry behavior and
  readback rules before entering a default form.
- Global people search, non-member directory browsing, bulk invitations and
  cross-community member transfer. P0 accepts exact identities and identities
  the active relay can resolve; a mention never silently invites someone.

## Channel and membership model

### Channel states

| State | Listed in normal Inbox | Can read history | Can send | Can restore |
| --- | --- | --- | --- | --- |
| `active` | Yes if accessible and a member | Yes | According to current role and relay policy | N/A |
| `archived` | No | Yes while accessible | No; composer is disabled with an explanation | Owner/admin through Archived view |
| `inaccessible` | No | No | No | No; show no private name or content |
| `unknown` | Do not guess | Do not guess | No | Refresh or inspect the write result |

Archiving does not delete messages, membership or drafts. A draft remains
attached to `(community_id, channel_id)` but cannot be sent while the channel
is archived. Unarchiving re-enables the draft only after metadata and
membership readback confirms access. If the user was removed while archived,
the channel stays inaccessible and its private data is not restored locally.

### Roles

The relay role vocabulary is `owner`, `admin`, `member`, `guest` and `bot`.
Owner is above admin; admin is above member; guest is read-only; bot is a
separate automation designation rather than a level in that hierarchy.

- Owner and admin can update metadata, archive/unarchive, add members, change
  roles and remove members when the relay allows it.
- Member can read and send according to channel policy and can leave.
- Guest can read but cannot send or administer.
- Bot is displayed as an Agent/integration identity and has no implicit admin
  rights.
- The last owner cannot be removed or demoted. Owner transfer is not a silent
  side effect of adding a member; expose it only when the relay offers an
  atomic transfer operation.

The role shown in a row is a snapshot. A stale snapshot can only explain why an
action was attempted; it cannot authorize the event.

## Shared write contract

Every lifecycle or membership mutation follows:

```text diagram
Ready -> Validating -> Writing -> Confirmed
                         |          |
                         +-> Refused +-> Unknown
```

`Confirmed` means the relay accepted the signed event. The session then
refreshes channel metadata or membership before presenting the resulting state.
If the relay accepted the event but readback is delayed, show “Saved; waiting
for channel refresh” rather than an optimistic final role or archive state.

`Refused` means the client knows the event was not stored, usually because of
validation, authorization or a definitive relay error. `Unknown` means the
submission may have been stored but confirmation was lost or malformed. An
unknown operation is never automatically retried. The user can refresh the
target channel or member list, inspect the event when an id is available, and
then decide what to do.

Member batches use one signed event per target identity. They run sequentially
so a community switch or generation change can stop the batch safely. The
result is per target:

```json
{
  "status": "partial",
  "community_id": "…",
  "channel_id": "…",
  "members": [
    {"pubkey": "…", "status": "confirmed"},
    {"pubkey": "…", "status": "refused", "error": "forbidden"},
    {"pubkey": "…", "status": "unknown", "error": "timeout_unknown"}
  ]
}
```

Already confirmed targets are not submitted again by a recovery path. If a
community switch occurs between targets, stop with `cancelled_generation` and
leave the remaining targets untouched. The result identifies the active
community and never reports a cross-community partial success.

All command and interactive responses include `community`, `channel_id`,
`generation` when applicable, `status` and a structured error. They do not
include private keys, auth tags or raw authorization headers.

## CLI contract

All commands accept the existing `--community <profile-id>` selector. Without
it, they use the active saved profile. `--relay` remains a one-shot unsaved
connection and conflicts with `--community` under the existing configuration
rules.

### Commands

```text literal
buzzx channels list [--include-archived] [--community <profile-id>]
buzzx channels get --channel <channel-id> [--community <profile-id>]
buzzx channels create --name <name> [--type stream|forum] \
  [--visibility open|private] [--description <text>] [--community <profile-id>]
buzzx channels update --channel <channel-id> \
  [--name <name>] [--description <text>] [--visibility open|private] \
  [--community <profile-id>]
buzzx channels archive --channel <channel-id> [--community <profile-id>]
buzzx channels unarchive --channel <channel-id> [--community <profile-id>]
buzzx channels members --channel <channel-id> [--community <profile-id>]
buzzx channels add-member --channel <channel-id> --pubkey <hex64> \
  [--role owner|admin|member|guest|bot] [--community <profile-id>]
buzzx channels set-role --channel <channel-id> --pubkey <hex64> \
  --role owner|admin|member|guest|bot [--community <profile-id>]
buzzx channels remove-member --channel <channel-id> --pubkey <hex64> \
  [--community <profile-id>]
buzzx channels leave --channel <channel-id> [--community <profile-id>]
```

Type defaults to `stream`; visibility defaults to `open`. `channels join` and
`channels delete` are P1 commands and must not be hidden
behind `archive` or `remove-member`. The initial CLI accepts canonical 64
character hex pubkeys; an `npub` input form can be added only through the same
normalizer used by other identity commands.

`channels list` excludes archived channels by default and offers an explicit
`--include-archived` view. It must not turn a forbidden or failed query into an
empty list. `members` returns role and profile fields when available, while
preserving the canonical pubkey even when a profile is missing.

Single-write responses use the existing stable write statuses with a
channel-specific action:

- `confirmed`: relay accepted the event; the response includes `event_id`.
- `refused`: local validation or relay refusal established no storage.
- `unknown`: the write may have landed; the response includes a refresh hint
  and never authorizes an automatic retry.

Archive, update, role and membership commands return the target channel even
when the event id is unknown. The command is not successful merely because the
local list changed; readback or an explicit unknown state remains visible.

## TUI interaction

The channel header always shows the active community, channel name and one of
`Connected`, `Archived`, `Switching`, `Retry` or `Could not connect`. Actions
use the existing `Ctrl+P` command palette so they do not conflict with `C`
(community picker), `c` (conversation picker) or composer text.

### Create and edit

1. `Ctrl+P` → **Create channel** opens Name, Type, Visibility and Description.
2. Submit enters `Creating…`; duplicate submit is disabled.
3. `Created` refreshes the current community roster and opens the canonical
   channel id. `Failed` keeps editable fields. `Unknown` asks for an explicit
   refresh and never creates a second channel automatically.
4. `Ctrl+P` → **Edit channel** exposes name, description and visibility only;
   changing channel type is not offered.

### Archive and restore

- **Archive channel** opens a confirmation naming the channel and community.
  It is disabled while another write for that channel is pending or unknown.
- On confirmation, the channel becomes read-only only after relay confirmation
  and metadata refresh. The view then returns to Inbox; the per-channel draft
  is retained but marked `archived`.
- **Archived channels** is an explicit filter in the channel picker. It shows
  accessible archived channels with their last known metadata and offers
  **Unarchive** to an owner/admin.
- A failed archive leaves the channel active. An unknown archive leaves the
  current view with an explicit warning and blocks community switching until a
  refresh establishes the result.

### Member management

`Ctrl+P` → **Manage members** opens a full-screen overlay in narrow layouts and
a centered panel in wide layouts. It contains:

- current members, display name/avatar when available, role and short pubkey;
- **Add members**, which accepts relay-visible identities or an exact hex
  pubkey, supports multi-select and requires a role before confirmation;
- each member's **Change role** and **Remove** actions when the current role
  permits them; removing someone requires explicit confirmation;
- **Leave channel** as a separate self-action, with a last-owner warning.

Same-name identities are never selected by display name alone. The row shows a
short key and the confirmation names the canonical pubkey. A member search
failure is distinct from “no matching members”; the user can paste an exact
identity instead.

After a confirmed or partial batch, the overlay refreshes membership. It shows
per-identity results and does not hide successful adds because one target was
refused. A generation change closes the operation with `cancelled_generation`
and does not apply late results to the new community.

## WebUI interaction

Wide WebUI exposes the same actions from the channel header and member sidebar.
Narrow WebUI exposes them in the channel action sheet; the app bar keeps the
community selector visible. The browser submits intent to the running session
and never handles private keys, auth tags or relay URLs.

- Channel create/edit uses the shared form and field validation.
- Archive/unarchive uses a labeled confirmation modal and updates the list only
  after confirmed readback. Archived channels remain available through an
  **Archived** filter.
- Member management shows role, profile and key-disambiguation rows. Add uses
  a searchable picker with an exact-key fallback; remove and role changes have
  explicit confirmations. A batch panel shows confirmed, refused and unknown
  rows independently.
- Every tab receives the process-level `community_id + generation` state.
  Old member results, archive errors, drafts and optimistic rows are discarded
  when the generation changes.
- If any tab has a pending or unknown channel/member write, switching community
  is blocked with the operation and target named. The user must refresh or
  resolve that state first.

TUI and WebUI may differ in panel placement and pointer/keyboard controls, but
they must use the same field names, role vocabulary, confirmation wording,
write statuses and permission errors.

## Implementation slices

| Issue | Depends on | Completion condition |
| --- | --- | --- |
| L1. Shared channel domain and readback | Existing profiles/session generation | `ChannelRef`, states, roles, archived filtering and member projection are community-scoped and covered by fixtures. |
| L2. Client builders and write outcomes | L1, Buzz protocol builders | Create/update/archive/member/leave events use one write classifier; refused and unknown never mutate local truth. |
| L3. CLI lifecycle and members | L2 | All P0 commands, selectors, JSON fields, exit codes and partial batch results are scriptable. |
| L4. TUI actions | L2, L3 contracts | Create/edit/archive/restore/member overlay works at 24×6, 40×10, 79×12 and 80×12 with draft and generation guards. |
| L5. Web actions | L2, L3 contracts | Wide/narrow header/sidebar/action-sheet flows match TUI semantics and synchronize across tabs. |
| L6. Live dual-community acceptance | L3, L4, L5 | Two relays/communities verify roles, archived filtering, member changes, same-name isolation and unknown-write handling. |
| L7. P1 discovery and destructive lifecycle | L6 | Join/search, delete, TTL/topic/purpose have separate permission and confirmation contracts. |

L1–L3 can proceed before the visual work. L4 and L5 must consume the same
session actions rather than implementing a second permission or write state
machine. L6 is required before calling the feature complete; fake-relay tests
alone cannot prove relay authorization or eventual readback.

## Acceptance matrix

1. Create the same channel name in two communities. Each gets a distinct
   canonical channel id, roster entry, message destination, draft and recent
   channel; no cache or event crosses the community boundary.
2. Owner/admin can edit name and description, archive, restore and manage
   members. A member, guest or bot sees only the actions their role permits,
   and a relay refusal remains visible even if a stale UI enabled an action.
3. Archive removes the channel from the normal list, preserves history and
   membership, blocks sending, and restores it through the Archived filter only
   after confirmed unarchive/readback.
4. Add, role-change and remove operations identify the target by canonical
   pubkey. Same-name members remain distinguishable; the last owner cannot be
   removed or demoted.
5. A multi-target add reports confirmed, refused and unknown identities
   independently, refreshes membership, never retries an unknown target and
   stops safely on a community/generation change.
6. CLI, TUI and WebUI expose identical fields, role vocabulary, `community`,
   `channel_id`, `status` and error semantics. `--include-archived` is explicit;
   permission failures are not empty lists.
7. Pending or unknown channel/member writes block community switching. Late
   relay events, drafts, member suggestions and errors from the old generation
   cannot appear in the target community.
8. Real relay evidence reads stored command tags, canonical ids, membership
   roles, archive metadata and visibility from a second identity. Unit and
   fake-relay tests supplement but do not replace this evidence.

## Non-goals

This slice does not add automatic invites from mentions, global cross-community
member search, offline writes, cross-community aggregation, a second active
relay per process, silent owner transfer, or destructive delete in the P0
archive flow. Those would change permission and recovery semantics and need
their own acceptance contract.
