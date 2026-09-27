# Read a complete message

Status: M1 implemented in `buzzx-visual-spec` on the reader implementation
branch. The full release remains incomplete: conversation filters, message
search, context results and incremental history are not implemented. This
spec is the contract for the reader and safe return; the implementation is
based on merged commit 88595b95b94875533e69704cda4c03f6a31eb9c2.

## Problem and priority

A message can be taller than the timeline. The current renderer shows its
beginning, while navigation moves between events rather than within the body.
At 40x10, a 60-line result exposes its heading and first five numbered lines.
PgDn, G and j cannot reach its final decision, in either channel or thread.
This was reproduced with the actual merged binary and a local fake relay.

Reading the result is necessary before answering it. This reader is now a
required component of [find and resume a conversation](tui-context.md), which
plans discovery, history and safe return as one release. Its early delivery
provides a stable destination for that larger flow; it is not the whole release.
The reproduced reading failure is evidence of a capability gap, not a claim
that most messages are long.

## Proposed flow

In channel, DM, search-context or thread navigation, press `v` on a loaded, confirmed row to
open its reader at the beginning. The reader replaces the whole terminal at
every supported size. Use the same entry for short and long rows; it is not
an automatic popup. The existing timeline remains a preview.

The navigation hints include `v read`; full help names `v: read full message`.
When the focused row is taller than the content region, retain this affordance
before secondary navigation hints. Do not add a new line or a selectable
truncation item. A short row can still be opened through v or help discovery.

Read the whole text, then Esc returns to the saved view and focused event.
Enter starts a reply to the displayed event through the originating view's
existing composer. It does not send anything and does not choose a newer row
that arrived while reading. A nested thread reply retains its existing root
and parent semantics. A reply does not imply a notification to its author.

## Keys and isolation

These bindings apply only while the reader is open:

| Key | Result |
| --- | --- |
| j / Down | Scroll one displayed text line down |
| k / Up | Scroll one displayed text line up |
| PgDn / PgUp | Scroll one content viewport minus one line, with a minimum step of one |
| g / Home | Beginning of this row's content |
| G / End | End of this row's content |
| Enter | Return to the origin and compose a reply to this event, subject to the guards below |
| Esc / v | Close reader and restore the origin |
| ? | Open reader help; Esc returns to the same reading position |
| q / Ctrl+C | Use the existing quit behavior |

Clamp at both ends. No key crosses into another event from inside the reader.
Channel shortcuts, c, a, f, t, i, r, e and d do nothing while it is open;
keys never leak through to the covered timeline. In a composer, v is literal
text. Help retains its existing scrolling and close keys; it cannot reply.

Outside this view, keys continue to navigate events or conversations. The
companion release spec owns incremental history and explicit jump-to-latest
behavior; G inside this reader always means the end of this message.

## Content and visual layout

Use revision 5's context, content, actions and state positions. There is no
sidebar, per-message frame, empty composer, second focus cursor or new palette.
The metadata line names the author and relevant existing row state. It stays
visible as the body moves, so an action never loses its author context.

| Region | Rows | Content |
| --- | --- | --- |
| Context | 1 | Message / origin conversation; preserve Message when the name shortens |
| Metadata | 1 | Author, age and relevant state; allocate semantic labels before the name/age |
| Scrollable text | Height minus 4 | Existing rendered body, attachment labels and reaction lines |
| Actions | 1 | Scroll, reply when available, back, help |
| State | 1 | Highest-priority existing state and visible content range |

At 24x6, two text rows remain. At 40x10, six remain. At 80x12, eight remain.
At 120x30, twenty-six remain. Insets reduce before useful text: no extra
padding at 24 columns; use the shared two-cell text inset at larger widths.
The first example is a 40x10 reader, not an implementation screenshot:

```text diagram
Message / #release
Build  2m
The visual repair is complete.
The full checks passed.
One acceptance item remains:
verify in a light terminal.
Do not treat fake-relay results
as evidence for the live write path.
j/k scroll  Enter reply  Esc back  ?
connected | 1-6/60
```

The range counts displayed content lines after wrapping, including attachment
labels/reaction text, excluding pinned regions. `1-6/60` means text lines,
never replies, events, unread messages or task progress. Empty content shows
`No text` with no invented line count. If state and range cannot both fit,
state wins and help carries the range. End adds no extra content or border.

Keep every body character's readable representation and original blank lines.
Preserve indentation and existing text formatting; soft-wrap long lines and
URLs instead of adding horizontal scrolling. Tabs expand to the next four-cell
stop from the content edge. Measure terminal cells and keep grapheme clusters
intact, including combining marks and emoji sequences. Existing safe text
handling still applies: content is never terminal control instructions.

Full labels and status reasons are available in reader help. The range makes
no claim that the enclosing conversation or thread history is complete. Media
remain existing text labels; this adds neither Markdown rendering nor link
activation, attachment download, clipboard integration or external programs.

## Identity, drafts and return

Keep the originating conversation, surface (channel, search context or thread), Inbox filter,
focused event and viewport. Bind reader content and Enter to an event identity,
not an array index, display name, time or screen line.

Opening/closing the reader does not change a retained draft. If a nonempty
draft exists, Enter does not retarget or resume it from this view. Remain in
reader, show `Draft kept; Esc back`, and explain in help that the user should
return and finish or clear the existing draft first. This deliberately makes
Enter's reader meaning unambiguous: reply to the message being read.

With an empty buffer, Enter closes the reader, restores its origin and opens
that view's normal reply composer. Replace any empty buffer's stale reply/edit
target with the displayed confirmed event. Use the origin's existing cancellation,
send, refusal, uncertainty and thread-leave rules; do not add another draft store.

Pending or unconfirmed local rows cannot open the reader in this first slice:
show `Wait for confirmation` or `Unconfirmed message` respectively. No resend
is offered. A confirmed deleted placeholder has no readable original text and
shows `Message deleted` with back/help only. Other non-replyable row kinds can
be read but retain their existing action restrictions. When reply is unavailable,
omit the Enter hint and explain its reason in the state/help.

Closing after the source event has been deleted restores the nearest surviving
neighbor, as in thread return. If there is none, restore the empty origin. An
authoritative loss of channel access removes its content and exposes back/help;
return to the existing accessible-conversation fallback, never send to a new one.

## Live updates and resize

Other events can arrive and update background lists, but cannot replace the
reader's target or move its text. Returning restores the saved event, even if
it used to be the newest. Do not auto-follow new events while this view is open.

An authorized edit to this event replaces its body and resets reading to the
start with `Updated; at start`. This simple, visible reset avoids silently
moving a line offset into unrelated revised text. A reaction-only update does
not reset the body anchor. A confirmed deletion clears the body immediately,
shows `Message deleted`, and disables reply. Missing from a bounded refresh is
not itself proof of deletion; retain the loaded row with the appropriate state.

Connection loss keeps loaded text readable and shows reconnecting/stale state.
It cannot erase a draft, claim fresh content, or enable a forbidden publication.
Entering the existing reply composer may still prepare text; its current send
guards remain in force. Opening the reader does not fetch a new history page.

Resizing keeps the top visible content anchor by source position, then rewraps
at the new width. Do not retain only a rendered-line number across width changes.
If the final viewport is shorter than a page, clamp to the last valid page.
Below 24x6, the existing size message takes over; recovery restores the same
source position, target and origin. An edit received while too small still
uses the explicit update/reset rule.

Reader open, scroll, end, help and resize do not advance channel read markers.
Background arrivals remain governed by unread rules for a covered timeline.
Returning may advance reading only when the normal visible-timeline conditions
are met. This cannot retract a marker already advanced before v was pressed,
and reaching the end is not a task-complete acknowledgement.

## Implementation boundary

This is local presentation and input state. Reuse loaded rows, existing edit/
delete projections, reply semantics, permission checks, connection state and
composer. Rendering stays pure; the state layer owns the target, origin and
reading position. No relay API, query, signed event, persisted reader state or
new identity relationship is introduced.

The existing channel/thread preview is intentionally retained. Do not silently
change the manual's event-navigation rules while adding reader line navigation.
Introduce the new entry in help and link it from the manual on implementation.

## Acceptance before implementation is called done

| ID | Reproduced workflow | Required observation |
| --- | --- | --- |
| R1 | Open the 60-line fixture at 40x10; j and PgDn to end | Every numbered line and the final decision are reachable; no lost blank/code/URL text |
| R2 | Repeat through channel, DM and nested thread | One reader design, correct origin on Esc, correct root/parent on reply |
| R3 | Reach end then go upward | Clamped motion, correct range, overlap on page changes, no switch to adjacent event |
| R4 | Open and exit at 120x30, 80x12, 79x12, 40x10, 24x6 | Header/author/body/actions/state all fit; minimum has two body rows |
| R5 | Resize wide/narrow/too-small and back mid-paragraph | Same source text near the top; target and saved origin retained |
| R6 | New row arrives while reading an old one; Enter | View does not jump; reply targets original event; original draft guards apply |
| R7 | Open with a retained draft; Enter; Esc | No draft mutation or retarget, no write; origin is restored |
| R8 | Edit, react to or delete the displayed event remotely | Edit reset is explicit, reaction does not jump, deletion clears content and blocks reply |
| R9 | Disconnect; restore connection; lose channel access | Honest loaded/stale state and existing send restrictions; revoked content not exposed |
| R10 | Open/scroll/help/end/resize with new background messages | No reader-triggered read-marker publication and no false channel-wide read claim |
| R11 | Type v in composer; press unrelated action keys in reader | v is text in composer; reader actions do not leak; help restores offset |
| R12 | Long author/state, CJK, combining marks, emoji, long URL | State survives labels; all text reachable; no split cluster or border/footer collision |

Run the package's full checks and an actual terminal workflow. Capture the
fixture's last line, the reply's stored event references and returned origin,
not only a screenshot of the first page. Include dark/light defaults, ANSI
and NO_COLOR. Product success is reaching the decision and replying to the
intended message; these are future acceptance conditions, not completed tests.

The [interactive review](assets/tui-reading.html) illustrates opening, scrolling,
return and reply at several sizes. It is a design simulation, with fixture text
and no relay calls. It does not certify real terminal width or network behavior.
