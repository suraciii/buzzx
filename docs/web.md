# Using `buzzx web`

Status: proposed product specification. The command is not implemented.
The parity baseline is main commit `88595b95b94875533e69704cda4c03f6a31eb9c2`.

## Outcome and scope

A person runs `buzzx web` and uses Buzz in a browser with the capabilities
already available in `buzzx tui`. They use their existing identity, relay,
conversations and permissions. They install no separate web application.

Parity means the same available operations, destinations, observed information
and result meanings. It does not mean drawing terminal cells in a browser or
requiring terminal key sequences. Mouse, touch, text selection and ordinary
browser navigation are first-class interactions.

The [TUI manual](tui-use.md) owns the existing conversation behaviors. This
document owns the browser-specific presentation and lifecycle. The
[Web design](../design/web.md) records the implementation boundaries.

The first release includes every row of the following matrix. Delivery may be
incremental, but basic chat alone is not feature parity.

| Capability | Browser entry | Existing behavior to preserve |
| --- | --- | --- |
| Identity and relay | Start from the terminal; identity label in the header | [Configuration](configuration.md): precedence, auth tag and one relay per run |
| Channels and existing DMs | Inbox sidebar or conversation list | [Inbox](tui-use.md#inbox-and-read-state): labels, visibility and stable ordering |
| All, Unread, For you | Three Inbox filter buttons | Same candidates, unknown state and retained selection |
| Read progress | Read the latest channel content | Same marker and coverage rules; thread reading does not clear channel unread |
| Live timeline and long reading | Scroll, select text, jump to latest | Same loaded rows, overlays and focus preservation |
| New message and reply | Composer and per-message Reply | Same channel, root and parent; explicit target above input |
| Mentions | Type complete names or exact references | [Mention resolution](tui-use.md#mentions-when-sending), including blocked sends |
| Reactions | Reaction chips and thumbs-up action | Add/remove own default reaction; display observed reaction counts |
| Edit and delete | Own-message action menu | Same ownership, target and write-result rules |
| Focused thread | Open thread on a confirmed row | [Thread reading](tui-use.md#planned-focused-thread-reading), including draft guards |
| Typing | Above the channel composer and in Inbox rows | [Typing](tui-use.md#typing-indicators): receive-only, channel-scoped and expiring |
| My agents | Header action, list, then detail | [Agents overview](tui-use.md#agents-overview): owned roster, observed work and accessible contexts |
| Help and status | Help action and persistent status area | Full labels, actionable reasons, connection and write outcomes |

Rendered text, diffs, forum rows and attachment metadata retain the existing
row meanings. Dedicated forum navigation, attachment upload or inline media,
search, DM creation, channel administration, Agent control, notifications and
huddles are outside this parity release. The browser does not add them merely
because it can display richer controls.

## Start and stop

With an identity already configured:

```sh
buzzx web
```

The command resolves the same configuration as TUI, starts a local listener
on an available loopback port, prints an access link, and attempts to open the
default browser once. Web assets ship with buzzx: no Node runtime, development
server, external asset service or separate install is needed to run it.

The listener is available only on `127.0.0.1`. Its selected port is printed
in the access link. A second process gets its own port and access link. No
new flags, environment variables or config fields are introduced in this
slice; the existing global identity and relay flags still apply.

The terminal shows the public identity, relay, access link and `Ctrl+C to
stop`. The link opens only this running process. It grants access to that
identity's session and should stay private. It contains no Nostr private key.
The browser never asks the user to paste that key.

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
empty Inbox. The identity and relay stay fixed for the process lifetime;
changing them requires restarting with the desired configuration.

Closing a tab closes that view, not the process. `Ctrl+C` stops the process
and invalidates its browser sessions. Open pages show `Session stopped` and
disable writes. A later run requires its new access link. Stopping web does
not log out of buzzx or remove saved credentials.

## Page shape

There are five surfaces: Inbox/conversation, focused thread, My agents, Agent
detail and Help. The latter three return to the same conversation context.
There is no dashboard or separate home page before the Inbox.

At 900 CSS pixels and wider, show a 260-pixel Inbox sidebar, a flexible
conversation column and a composer at the bottom. Keep the reading column
at most 880 pixels wide within its region. Below 900 pixels, show one surface
at a time: a conversation list or the selected conversation, with an explicit
Inbox back action. Thread uses the conversation region on wide screens and
the full content region on narrow screens; it never creates a third column.

```text diagram
+------------------+--------------------------------------------------+
| buzzx            | #engineering                  My agents    Help  |
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

Incoming content follows the viewport only when the person is already at the
latest message. When reading older content, retain the visible event and
offset and offer `Jump to latest`. Preserve selection, scroll and draft
through resize, in-app navigation and transient disconnection. The top of the
loaded window is labeled as such; do not imply that all history was fetched
or add infinite history pagination in this release.

A browser adds one requirement to read progress: its document must be visible
and focused and the latest loaded content must actually be in view. A hidden
tab, a minimized window, a covering view, an unloaded timeline or a focused
thread cannot advance a channel frontier. Receiving data is not reading it.
Existing incomplete-coverage and unsynced-marker states remain visible.

Open thread resolves the selected row's containing root even when it is
outside the channel's loaded window. Keep the root and replies together,
show partial coverage when the existing bound is reached, and preserve live
edits, deletions and reactions. A deleted root is a placeholder, not an empty
thread. Loading failure offers Retry read and Back, never an empty success.

While a thread is open, the wide sidebar is visible for context but its
navigation, filters and My agents are disabled with `Return to channel first`.
Do not let another control silently change a thread draft's destination.
Thread entry and return follow the TUI's existing nonempty-draft and
pending-write guards. When allowed, Back restores channel selection, filter
and viewport; a deleted anchor uses the nearest surviving row.

Browser Back follows these same in-app guards. It cannot bypass them and
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

Mention resolution runs on send using the existing rules. When it fails,
keep the draft and target, name the unresolved fragment next to the composer,
and expose complete selectable exact references for ambiguous names. There
is no mention picker, automatic invitation or hidden recipient inference.
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
Web disables publication until its session can publish again and does not
queue offline operations. Lost membership or a deleted target blocks the
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

1. A configured user starts from the shipped buzzx binary with `buzzx web`,
   opens its printed link, and reaches the same visible conversations as TUI.
   Browser launch failure and missing identity have actionable outcomes.
2. Exercise every parity-matrix row with the same identity, relay and source
   revision in both surfaces. Record differences explicitly; omitted rows
   prevent a feature-parity claim.
3. In Chromium and Firefox, complete read/send/reply/react/edit/delete and
   thread return at 1440x900 and 390x844. Check 360-pixel width, 200% zoom,
   keyboard-only navigation, IME entry and long content separately.
4. Verify stored channel/root/parent/recipient identities and canonical ids
   through authenticated relay readback, including mentions in an existing DM.
   A screenshot of a pending row is not proof of a stored message.
5. Read older content during live traffic; open/return from a thread; switch
   conversations with drafts; resize and reconnect. No focus jump, draft loss,
   wrong destination or duplicate write is allowed in these in-app flows.
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

Run the repository's required checks at the delivered source state. Record
browser version, viewport, binary revision and real-relay evidence separately
from CI. This specification supplies acceptance requirements, not evidence
that a Web client already works.
