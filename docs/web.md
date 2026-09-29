# Using `buzzx web`

Status: implemented local browser surface with relay-backed navigation,
inspection, tab-isolated drafts and shared write/read-state semantics.
Community selection and in-process switching are implemented; browser profile
management remains in `buzzx community` and live relay/browser checks below
are still required before claiming a release.

## Outcome and scope

A person runs `buzzx web` and uses Buzz in a browser with the same product
capabilities as `buzzx tui`. They use their existing identity, saved
community profiles, conversations and permissions. They install no separate
web application.

Parity means the same available operations, destinations, observed information
and result meanings. It does not mean drawing terminal cells in a browser or
requiring terminal key sequences. Mouse, touch, text selection and ordinary
browser navigation are first-class interactions.

The [shared catalog](interactive.md#shared-capability-catalog) and its linked
behavior owners define the whole scope. This document owns browser-specific
presentation and lifecycle. [The Web design](../design/web.md) records the
implementation boundaries; [the reference comparison](../design/client-reference.md)
explains the choices informed by Buzz Desktop and Mobile.

The first release includes every catalog row, including conversation lookup,
message search, exact context, history paging, complete-message reading and
safe return. Delivery may be incremental, but basic chat alone is not feature
parity. Shared exclusions are recorded only in
[the product boundary](interactive.md#reference-features-outside-this-release).

## Start and stop

With an identity already configured:

```sh
buzzx web
buzzx web --community <id>
```

The command resolves the same configuration as TUI, starts a local listener
on an available loopback port, prints an access link, and attempts to open the
default browser once. Web assets ship with buzzx: no Node runtime, development
server, external asset service or separate install is needed to run it.

The listener is available only on `127.0.0.1`. Its selected port is printed
in the access link. A second process gets its own port and access link.
`buzzx web --community <id>` starts on that saved profile; without the flag
the process uses the active community. `--community` combined with `--relay`
or `BUZZ_RELAY_URL` is refused as `conflicting_relay_selector` before any
listener starts. Selection precedence is owned by
[the configuration reference](configuration.md#selection).

The terminal shows the public identity, the active community's name and
relay host, the access link and `Ctrl+C to stop`. The link opens only this
running process. It grants access to that identity's session and should stay
private. It contains no Nostr private key. The browser never asks the user
to paste that key, and no page shows an auth tag's content.

If there is no usable identity, the command exits with the existing
configuration error and points to `buzzx login`. Browser opening is attempted
only after configuration and listener startup succeed. A bind failure is a
startup failure with a reason; it never prints a usable-link claim.

On a machine without a graphical browser, print the link and keep running.
The same behavior applies when browser launch fails. An SSH user may forward
the printed loopback port and open the link on their own machine; this does
not expose the listener to a network interface. This release defines a local
client, not a public or multi-user hosting mode.

The page can load while the relay is unavailable. It distinguishes
`Connecting`, `Reconnecting` and an explicit authentication failure from an
empty Inbox. The identity stays fixed for the process lifetime. The active
community may be switched inside the process from the community selector;
the process still holds exactly one relay connection at a time, so a switch
replaces that connection instead of adding a second one.

Closing a tab closes that view, not the process. `Ctrl+C` stops the process
and invalidates its browser sessions. Open pages show `Session stopped` and
disable writes. A later run requires its new access link. Stopping web does
not log out of buzzx or remove saved credentials.

## Communities and switching

`buzzx web` runs on one saved community profile: the one named by
`--community`, else the active community. Because the process holds exactly
one relay connection, the browser's community choice is process-wide: every
tab of this run shares the active profile.

Wide screens put the community selector at the top of the Inbox sidebar;
narrow screens show `Community / Channel` in the app bar. The selector lists
saved profiles with their name and relay host and switches the one active
connection. It does not show cross-community unread totals or mix another
profile's channels into the Inbox. Add, rename and remove profiles with
`buzzx community`; the browser never accepts relay URLs or signing material.

Switching to another saved profile follows one visible sequence:

1. Any tab can start the switch. A pending or uncertain write in any tab
   blocks it, naming the tab and the operation.
2. Every tab receives the switching state and clears its visible target; the
   previous community's timeline is never shown as the target's content.
3. On success every tab receives the new community and view generation, then
   restores that profile's recent view or the Inbox. A late update from the
   old community is dropped, not merged.
4. On failure every tab shows the target profile's error and remains on that
   target until the person reselects a profile; selecting it again retries.
   The process does not fall back to the previous profile or pick another one.

Drafts, reply/edit targets, selection and scroll belong to their own tab and
community: a switch suspends them with the previous profile instead of
carrying them across. The [Web design](../design/web.md#community-switching)
owns the generation and broadcast mechanism.

Adding a community - a new relay URL or an auth tag - is a terminal action.
The selector offers no form for it; use `buzzx community add` and refresh the
page to load a newly saved profile. There is no cross-community Inbox, search,
mention or forwarding.

## Page shape

The main surface is Inbox/conversation. Search, exact result context, focused
thread and a full-message reader form an inspection journey; My agents, Agent
detail and Help return to their invoking view. There is no dashboard before
the Inbox. Keep Search messages visible in the conversation/thread header and
available while composing; Find conversation belongs to the Inbox list.

At 900 CSS pixels and wider, show a 260-pixel Inbox sidebar, a flexible
conversation column and a composer at the bottom. Keep the reading column
at most 880 pixels wide within its region. Below 900 pixels, show one surface
at a time: a conversation list or the selected conversation, with an explicit
Inbox back action. Thread uses the conversation region on wide screens and
the full content region on narrow screens; it never creates a third column.

```text diagram
+------------------+--------------------------------------------------+
| buzzx            | #engineering          Search   My agents   Help  |
| Community: work v|                                                  |
| Find conversation|                                                  |
| All Unread       | Connected                                        |
| For you          +--------------------------------------------------+
| Channels         | Alice                              10:42         |
| > engineering @2 | Can we review the change?                        |
|   general        | Reply   Open thread   Like                       |
| DMs              | ------------------------------------------------ |
|   Sam         1  | Build                              10:43         |
|                  | Checks passed.                                   |
|                  | Reply   Open thread   Like                       |
|                  |                                      Latest v  |
|                  +--------------------------------------------------+
|                  | New message in #engineering                      |
|                  | [ Write a message...                           ] |
| identity / relay | Enter sends. Shift+Enter newline.         Send   |
+------------------+--------------------------------------------------+
```

```text diagram
+------------------------------------+
| < #engineering / Thread       Help |
| Alice - root                       |
| Can we review the change?           |
| ---------------------------------- |
| Build                              |
| Checks passed.                     |
| Reply   Like   More                 |
|                                    |
| Reply to Build                     |
| [ Write a reply...               ] |
|                             Send   |
+------------------------------------+
```

These are product wireframes with sample content, not screenshots. The narrow
thread back action returns to its saved channel position, not the Inbox.

Search replaces the content region. The same form stacks vertically on narrow
screens; a result opens context in that region, and Back returns to the result.

```text diagram
+-----------------------------------------------------------+
| < Back to #engineering                     Search messages |
| [ release decision                               ] Search |
| Scope: #engineering   Author: Anyone   Time: All           |
| 8 results returned                                        |
| > #engineering / Alice / Sep 25                            |
|   We will release after the migration...                   |
|   Open context                                            |
+-----------------------------------------------------------+
| Draft kept in #engineering                Return to draft  |
+-----------------------------------------------------------+

+-----------------------------------------------------------+
| < Search results           #engineering / Search context  |
| Load older                                                |
|   Sam: The migration is ready.                             |
| > Alice: We will release after the migration...            |
|   Reply   Open thread   Read message                       |
| Load newer                                Jump to latest  |
| Draft kept in #engineering                Return to draft  |
+-----------------------------------------------------------+
```

Use the TUI's calm reading hierarchy: clear author labels, secondary times
and metadata, weak message separators, and explicit state words. Use a system
sans-serif body at 16 pixels with 1.5 line height, monospace for code, an
8-pixel spacing scale and 44-pixel touch targets. Human and Agent messages
use the same layout. Selected conversation, focused action and unread
attention remain distinct; color is never their only signal.

Follow the browser's light/dark preference without adding a theme setting.
Message text wraps without page-wide horizontal overflow. Code blocks can
scroll within their own area. Long names and reasons have a readable detail
surface. At 360 CSS pixels and at 200% zoom, every action remains reachable;
the software keyboard must not cover the target or send control.

## Read and navigate

Start with the same initial conversation selection as TUI. Loading, empty,
incomplete and failed lists have different messages. Filters do not fabricate
a zero unread count when coverage is unknown. Hidden DMs and archived
channels follow the existing visibility rules.

Find conversation filters the accessible listed channels/DMs by name using
the [shared ranking and filter rules](tui-context.md#c1-find-a-conversation).
Keep the active Inbox filter visible. It is a local list filter, not message
search or a way to discover/join a new channel. Clearing it restores the list;
opening a result obeys the existing draft/navigation guards.

Incoming content follows the viewport only when the person is already at the
latest message. When reading older content, retain the visible event and
offset and offer `Jump to latest`. Preserve selection, scroll and draft
through resize, in-app navigation and transient disconnection. At a loaded
boundary expose Load older or Load newer; reaching it may request the same
single page automatically. Repeated input while loading does not enqueue
duplicate requests. Keep the readable window on failure and offer Retry read.
Jump to latest requests the newest window and changes position only on success.
The [shared history rules](tui-context.md#c4-read-earlier-and-later-history)
own page/window bounds, ordering, same-second saturation and boundary evidence.
The initial page is not a permanent limit on how much history can be read.

A browser adds one requirement to read progress: its document must be visible
and focused and the latest loaded content must actually be in view. A hidden
tab, a minimized window, a covering view, an unloaded timeline, search context,
thread or full-message reader cannot advance a channel frontier. Receiving
data is not reading it.
Existing incomplete-coverage and unsynced-marker states remain visible.

Open thread resolves the selected row's containing root even when it is
outside the channel's loaded window. Keep the root and replies together,
offer adjacent pages under the shared history rules, and preserve live
edits, deletions and reactions. A deleted root is a placeholder, not an empty
thread. Loading failure offers Retry read and Back, never an empty success.

While a thread is open, the wide sidebar is visible for context but its
navigation, filters and My agents are disabled with `Return to channel first`.
Do not let another control silently change a thread draft's destination.
Ordinary thread entry and return follow the existing nonempty-draft and
pending-write guards. A thread entered through search is part of that inspection
journey and may be read with the origin draft suspended; writing still obeys
the shared draft guard. Back restores the actual invoking channel or search
context, including selection and viewport. A deleted anchor uses the nearest
surviving row.

## Search, inspect and resume

Search messages opens a dedicated view scoped to the current conversation,
including when invoked from its thread or composer. Preserve the invoking
surface, draft, reply/edit target, text cursor and focus. The current query,
applied filters, selected result and result scroll position survive inspection.

Provide a query field and separate Scope, Author and Time controls using the
[shared search options](tui-context.md#c2-search-messages). Scope distinguishes
this conversation, a chosen listed conversation and all accessible listed
conversations. Authors bind to a public key, with full identity details for
duplicate names; allow the same exact-key input as TUI. Apply Search or Enter
explicitly; changing a form field alone does not replace the results.

Each result shows conversation, author, time, excerpt and known thread
relationship. Keep selection by event id and retain the relay's ordering until
another search is submitted. Display loading, empty, failed, unsupported and
bounded results distinctly. A failed query can retain previous results only
with their previous-query label. Limits and coverage wording follow the shared
contract; do not offer unsupported result pagination or claim archive totals.

Open a hit in Search context at that exact event, with surrounding messages
and Older/Newer controls. Reply, Open thread and Read message use the focused
confirmed row. A missing/deleted/revoked hit shows an explicit unavailable
state and Back; it never silently opens the latest channel message instead.
Do not use the clicked display name or array index as the action destination.

Read message opens the complete text in one scrollable content region, with
author and origin visible and Reply/Back controls. It is available for any
confirmed readable row, regardless of its length. Native line/page scrolling,
text selection and zoom must expose its final character. The
[reader specification](tui-reading.md) owns live-edit reset, deletion, logical
text anchors and reply guards. The browser may already show complete text
inline; the focused reader still supplies a stable target and return route.

Back unwinds the actual Search -> Context -> Thread -> Reader journey.
Return to draft goes to its saved origin only when doing so cannot abandon a
new local draft or pending operation. A suspended nonempty draft allows
inspection but blocks replacing it with a reply elsewhere; show
`Draft kept; return to draft`. Known access revocation removes affected content
and returns to accessible navigation while preserving unrelated drafts.

## Browser navigation and view lifetime

Browser Back follows the same inspection and draft guards. It cannot bypass them and
silently turn a thread reply into a channel message. Refreshing, closing the
tab or leaving the application uses the browser's unsaved-work prompt when
supported if a draft or pending write exists. Drafts are kept only in the
current view's memory; reload, close and process restart do not promise draft
recovery. A pending result can still be unknown after leaving.

Multiple tabs have independent navigation, composer and reply/edit targets.
An action in one never operates on another tab's selection or draft. Tabs
share confirmed relay content and read progress, not their UI selection.

## Compose and act

The composer always names the conversation and its mode: `New message`,
`Reply to <author>` or `Edit your message`. A reply may show a short excerpt
to distinguish messages by the same author. Selecting or scrolling a
different row does not retarget a draft. Per-conversation drafts and explicit
reply/edit targets survive switching conversations within the view.

Send is a visible button. Enter sends, Shift+Enter inserts a newline, and
IME composition never sends. Escape closes the active menu or help first;
inside a thread composer it leaves text entry while retaining the destination,
as in TUI. Outside inputs, Tab and Shift+Tab move focus and Enter/Space
activate buttons. Do not capture browser shortcuts such as Ctrl/Cmd+L, F, R,
T or W. Help explains these browser controls rather than copying TUI keys.

Every confirmed message exposes Reply and Open thread. A message's action
menu offers thumbs-up, Edit and Delete only where allowed. Own reactions
can be removed. Actions bind to that message's identity, not the last visible
row. Pending and uncertain local rows cannot be action targets. Deletion
has no added confirmation flow; it stays inside the labeled own-message
menu and follows the existing write contract.

Mention resolution runs on send using the existing rules. The `@` suggestion
popover is draft-local and lists current-channel members; selection inserts
readable text and the send-time resolver remains authoritative.
When resolution fails, keep the draft and target, name the unresolved fragment
next to the composer, and expose complete selectable exact references for
ambiguous names. Automatic invitation and hidden recipient inference remain
out of scope. Channel creation follows the shared form and write states in
[mentions-and-channel-creation.md](mentions-and-channel-creation.md).
Editing keeps its existing notification semantics.

On submit, show pending feedback and prevent a second submission of that
operation. Composer submissions remain bound to their originating conversation;
disable a second composer submission there until its result arrives. Restore
a refused draft only there, without replacing text in another conversation.
A confirmed relay result replaces the pending row once. Confirmed
means stored, not read by the recipient or started by an Agent.

| Write result | Browser behavior |
| --- | --- |
| Rejected or not submitted | Show the reason; composer writes restore the attempted text and target, other actions undo their optimistic presentation |
| Confirmed | Use the canonical event identity; reconcile any live echo once |
| Uncertain | Keep an explicit unconfirmed result; do not restore a resend-ready draft or retry |

These meanings apply to send, reply, edit, delete and reaction. While a composer
submission is pending, its text is read-only so a refused result cannot
overwrite a newer draft. An uncertain
edit or deletion is not reported as completed or definitely rolled back.
Its target and outcome remain inspectable in status detail. Network recovery,
refresh, repeated clicks and tab reconnection must never replay a write.

Loss of the browser-to-process connection disables writes but retains the
current draft and loaded content. A disconnected relay shows stale content;
publication follows the same session eligibility and outcome rules as TUI,
without queuing offline operations. Lost membership or a deleted target blocks the
affected action with a reason and keeps the draft available to copy.

## My agents and help

My agents shows the same verified owned roster, ordering, status vocabulary
and evidence age as TUI. Select an Agent to see observed contexts, then open
an accessible channel. Inaccessible contexts reveal no private names or ids.
Unavailable owner observation leaves chat usable. It does not become an empty
successful roster, an idle claim or a management panel.

Help is available from each surface and returns focus to the invoking control.
It includes full identity and relay labels, current state reasons and the
controls available there. Dialogs contain keyboard focus; closing them does
not fire actions behind them. Announce connection and write failures to
assistive technology without reading every incoming message aloud.

## Acceptance before calling Web complete

1. A configured user starts from the shipped buzzx binary with `buzzx web`
   or `buzzx web --community <id>`, opens its printed link, and reaches the
   same visible conversations as TUI on that community. Browser launch
   failure and missing identity have actionable outcomes.
2. Exercise every [shared catalog](interactive.md#shared-capability-catalog)
   row and [synchronized journey](interactive.md#synchronized-acceptance)
   with the same identity, relay and source
   revision in both surfaces. Record differences explicitly; omitted rows
   prevent a feature-parity claim.
3. In Chromium and Firefox, complete read/send/reply/react/edit/delete and
   thread return at 1440x900 and 390x844. Check 360-pixel width, 200% zoom,
   keyboard-only navigation, IME entry and long content separately.
4. Verify stored channel/root/parent/recipient identities and canonical ids
   through authenticated relay readback, including mentions in an existing DM.
   A screenshot of a pending row is not proof of a stored message.
5. Find a conversation, search messages, open exact context, page history,
   inspect a thread and read a complete report before returning to an existing
   draft. Exercise live traffic, resize and reconnect during this journey.
   No focus jump, draft loss, wrong destination or duplicate write is allowed.
6. Leave a tab hidden, unfocused or covered and read a thread. Its channel
   frontier must not advance. Then view the latest channel content and verify
   the existing cross-client marker behavior, including unknown/unsynced states.
7. Force rejected and lost-answer outcomes for each write type. Confirm the
   correct state and that reconnection or repeated submission sends nothing
   again automatically. Test two tabs with different targets concurrently.
8. Exercise thread load error, partial coverage, deleted root, lost membership,
   Agent access failure and stale observation without false empty/success claims.
9. Verify session stop/restart, refresh with unsaved work and credential
   isolation using the [Web boundary checks](../design/web.md#verification).

10. With two saved communities, switch from a second tab while the first
    reads history. Every tab clears to the switching state and receives the
    new generation; the previous community's timeline, unread and a late
    event from the old generation never appear as target content.
11. Attempt a switch while any tab has a pending or uncertain write: the
    switch is blocked and names the tab and operation. Force a failed switch
    and confirm every tab shows the target error and Retry without falling
    back to another profile.
12. Confirm that no page, dialog or response displays the private key or an
    auth tag's content, and that adding a community is offered only as a
    terminal instruction.

Run the repository's required checks at the delivered source state. Record
browser version, viewport, binary revision and real-relay evidence separately
from CI. This specification supplies acceptance requirements, not evidence
that a Web client already works.
