# Interactive capabilities

Status: shared product contract for TUI and the implemented Web surface.
The implementation baseline is the current working tree. Web runtime behavior
is implemented in `src/web.rs`; source presence alone is not acceptance
evidence, so browser and relay checks remain required. Community profiles,
selectors and in-process switching are implemented in the current baseline;
profile management remains a CLI responsibility and live multi-relay
acceptance still requires the checks below.

## One product, two interactive surfaces

`buzzx tui` and `buzzx web` serve the same job: find a conversation, read the
work, act on an explicit message and resume without losing intent. They share
the available capabilities, permissions, destinations, data coverage and
outcome meanings. Each uses controls suited to its input device and screen.

This is an ongoing product requirement. Web is not frozen at the earlier
`88595b9` TUI snapshot. A capability added to either interactive surface belongs
in this catalog and must have an entry, error behavior and acceptance path in
both. Temporary implementation gaps are recorded as incomplete; they do not
silently become permanent differences between the products.

The [Web specification](web.md) owns browser controls and lifecycle. The
[TUI manual](tui-use.md) owns terminal controls. Detailed behavior has one
home in the linked specifications below: their functional rules apply to both
surfaces, while terminal keys, cell budgets and terminal rendering remain
TUI-specific. A browser document must not invent a different limit, recipient
rule or reply target to make its interface simpler.

The [Desktop/Mobile reference](../design/client-reference.md) records what we
adopt from the other Buzz clients and where buzzx deliberately has a narrower
boundary. Those clients are reference implementations, not an obligation to
duplicate their entire feature set.

Both surfaces share one community model: one identity, saved community
profiles, and one active relay connection per process. Switching replaces
the active community inside the running surface; it never opens a second
relay connection or merges communities. Adding, renaming and removing a
community stays a terminal action through `buzzx community`; the interactive
selectors list and switch saved profiles without accepting signing material.

## Shared capability catalog

Every row is required for the first Web release and subsequent synchronized
interactive releases. TUI entries refer to existing controls at the baseline;
Web entries define the required browser equivalent.

| Capability and behavior owner | TUI entry | Web entry |
| --- | --- | --- |
| [Configured identity and one active relay](configuration.md#community-profiles) | `buzzx tui` | `buzzx web`, session identity header |
| [Community context and profile switching](configuration.md#selection) | Community line beside the channel; `C` picker to switch saved profiles | Community selector in the Inbox sidebar or app bar |
| [Find a listed conversation](tui-context.md#c1-find-a-conversation) | `/` in picker | Find conversation field beside Inbox filters |
| [Message search, scope, author and time](tui-context.md#c2-search-messages) | `/`; Ctrl+f from composer | Search messages button and filter form |
| [Exact result context](tui-context.md#c3-open-the-result-in-context) | Enter on hit | Open result at its event |
| [Older/newer history and latest](tui-context.md#c4-read-earlier-and-later-history) | Boundary navigation, `[`/`]` in context, `G` | Load older/newer and Jump to latest |
| [Read a complete message](tui-reading.md) | `v`, line/page scroll | Read message, native text scroll |
| [Focused threads](tui-use.md#focused-thread-reading) | `t`, Enter, `i` | Open thread, Reply, Reply to root |
| [Safe inspection and return](tui-context.md#c6-resume-work-without-changing-intent) | Esc through visited views | Back through visited views; Return to draft |
| [New messages and replies](tui-use.md#keys) | Composer, explicit target | Composer and Reply button |
| [Verified mentions](tui-use.md#mentions-when-sending) | Resolve plain text on send | Same resolution; inline correction detail |
| [Reaction, own edit and own delete](tui-use.md#keys) | `r`, `e`, `d` | Reaction chip and message action menu |
| [Read progress and unknown coverage](../design/shared-core.md#inbox-and-read-state) | Presented latest ordinary timeline | Same frontier; visible, focused browser view |
| [Incoming typing](tui-use.md#typing-indicators) | Channel line/list signal | Channel line/list signal |
| [Owned Agent summaries](tui-use.md#agents-overview) | `a`, Agent detail | My agents, Agent detail |
| [Confirmed, refused and uncertain writes](../design/shared-core.md#failures) | Row/status/help | Row/status/detail |
| [Responsive layout and state preservation](tui.md) | Terminal resize, help | Responsive panes, zoom, keyboard/touch, help |

The search/context/history/reader rows are present in the latest TUI and are
required Web scope, not optional follow-up work. See the baseline
[client](../src/client.rs), [session commands](../src/session.rs),
[state machine](../src/app.rs) and [key mappings](../src/keys.rs).

## Shared user journey

```text diagram
 Conversation and draft
      |
      +-> Find conversation -> Open conversation
      |
      +-> Search -> Exact context -> Thread -> Complete message
              <--------------- Back ------------------+
      |
 Resume original conversation, focus and draft
```

Inspection keeps the origin draft and its destination. Reply always targets
the selected confirmed event; a nonempty protected draft prevents an action
that would replace or retarget it. Safe return restores event identity,
logical reading position, filters and composing state. The detailed guards
and deleted/inaccessible-target behavior belong to
[safe return](tui-context.md#c6-resume-work-without-changing-intent).

The ordinary latest timeline, a search sample, historical context and a
single-message reader must remain distinguishable. A bounded result is not
the whole archive. A read failure is not an empty result. Reading a thread or
search context is not evidence that the containing channel is read.

Confirmed content and read markers come from the shared relay. Selection,
viewport and unsent drafts remain local to the running view. Switching input
surface does not change who receives a message or turn an uncertain write
into a safe retry.

## Allowed presentation differences

- A terminal uses key modes, cell wrapping and a dedicated full-message
  reader. A browser uses labeled controls, selectable text and scrolling;
  it also offers a focused reader with the same target and return behavior.
- Wide screens can keep navigation beside content. Narrow screens show one
  active surface with explicit Back. Hidden controls remain reachable through
  the equivalent list or action menu; no capability is desktop-width-only.
- Browser document visibility/focus and terminal presentation are different
  signals for the same reading requirement. Neither data arrival nor a covered
  timeline establishes read progress.
- Browser tab refresh/close and terminal quit end that view's temporary state.
  Neither surface promises cross-restart drafts in this release.

Permissions, query scope, accessible content, reply routing, mentions,
history completeness, draft guards, Agent state meaning and write outcomes
are product behavior and may not diverge by surface. A different keyboard
shortcut or pane arrangement is not a different capability.

## Reference features outside this release

Dedicated forum authoring/navigation, media upload/download or inline playback,
DM creation, member administration, persisted drafts, thread-follow management,
per-message/thread unread controls, Agent configuration/control, workflows,
projects and huddles are not introduced here. Existing forum rows and
attachment labels remain readable as before.

Any future addition from that set needs a shared user outcome and usable
controls in both surfaces. A Web-only attachment button or TUI-only search
path cannot count as a completed buzzx capability. Browser startup and
terminal setup are surface-specific entry mechanisms rather than separate
collaboration capabilities.

## Synchronized acceptance

Use the same buzzx source revision, relay, authorized identity and seeded
conversation fixtures for TUI and Web. Record each catalog row separately as
implemented, verified, failed or not run for each surface. Missing Web
implementation prevents a parity claim even when all TUI tests pass.

Both surfaces must complete these representative journeys:

1. Find a channel and an existing DM by name under each Inbox filter; unknown
   coverage stays unknown and selection does not move during live traffic.
2. Suspend a reply draft, search by keyword and by exact author with explicit
   scope/time, inspect a result, and return to the original draft unchanged.
3. Open an old nested reply outside the initial window, inspect its surrounding
   messages and containing thread, then reply with the same channel/root/parent
   and explicit recipient set in both surfaces.
4. Traverse older and newer channel, context and thread pages, including a
   thread beyond its initial page; exercise same-second saturation and failure.
   No silent gaps, false beginning/end or success-on-error is acceptable.
5. Read the final line of a long report, resize midway, receive an edit or
   deletion, and return to the correct origin. Reply still targets that event.
6. Receive live traffic while reading older history, a search context, thread
   or reader. Do not jump to latest or clear unrelated channel unread.
7. Exercise every write with confirmation, refusal and a lost answer. Compare
   destination, authorization and outcome semantics, never raw ids of separate
   sends. No recovery path automatically repeats an uncertain operation.
8. Open My agents, inspect stale/unavailable work and an accessible context,
   then return with draft and reading state intact.

9. Save two community profiles, switch between them, and verify the target
   community's timeline, unread, drafts and write destinations never mix; a
   failed switch keeps the picker open with a retry and never falls back to
   another profile silently.

Run these paths at the TUI sizes required by its specifications and at the
browser sizes/input modes required by [web.md](web.md). Store the exact
binary/source revision, fixtures, surface, observed results and remaining gaps.
Keep CI, source comparison and real-relay/browser evidence distinct.
