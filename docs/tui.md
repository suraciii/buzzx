# buzzx TUI

## Problem

`buzzx tui` currently rejects terminals smaller than 80 columns by 12 rows.
That prevents people using a phone terminal, a split SSH window, or a compact
console from using the chat client. The product needs to preserve the core
conversation loop on narrow screens without squeezing the desktop layout into
unreadable columns.

This specification defines the TUI capability, including its responsive
presentation. It also documents the Inbox surfaces and interactions that the
layout must preserve: conversation rows, unread filters, read-state signals,
picker behavior, and the conversation on screen. Relay read-state behavior is
defined in [shared-core.md](../design/shared-core.md#inbox-and-read-state).

## Layout modes

The client selects a mode from the terminal's reported width and height. It
does not infer a device type.

| Terminal size | Mode | Behavior |
| --- | --- | --- |
| 80 columns × 12 rows or larger | Wide | Inbox sidebar, timeline, and composer. |
| 40–79 columns and 10 rows or larger | Narrow | One-column timeline. `c` opens the full-screen conversation picker. Composer stays at the bottom. |
| 24–39 columns and 6 rows or larger | Minimal | One-column timeline with compact rows and a one-line composer while composing. |
| Below 24 columns or 6 rows | Too small | Start the session and show a size message. Keep listening for resize; switch modes automatically when the terminal is large enough. |

Narrow and minimal modes never introduce horizontal scrolling. Message bodies
wrap to the available width. Long words and URLs may break at character
boundaries; the underlying message remains unchanged.

The wide layout shows `Inbox: <filter>` in the sidebar and the selected
conversation on the right. The sidebar groups rows under `Channels` and
`DMs`; one-column modes keep the Inbox in the picker opened with `c`. The
picker and wide list use `All`, `Unread`, and `For you`. A row signal carries
the unread message count when known (`● 3`, `@ 2`), `?` for unknown, `Read` for
a picker row retained after it stops matching, or no signal. Its signal cell
is reserved before the label is clipped; typing adds `…` after the label.
Read-state details and limits are in
[shared-core.md](../design/shared-core.md#inbox-and-read-state).

## Unified keys

The action meaning depends on the layout and on whether the picker or help is
open. `j` and `k` move between conversations in the wide Inbox and between
timeline rows in one-column modes. In one-column modes, `c` opens the
conversation picker. `f` cycles `All`, `Unread`, and `For you` in navigation
mode; `f` or Tab cycles them in the picker. Help opens with `?`; while open,
`j`/`k`, `PgUp`/`PgDn`, and `g`/`G` scroll it, and `Esc` or `?` closes it.
Help displays the full selected conversation label.

| Action | Primary key | Alias |
| --- | --- | --- |
| Move focus / select item | `j` / `k` | Up / Down |
| Open conversation picker | `c` | — |
| Change Inbox filter | `f` | Tab in picker |
| Compose a new message | `i` | Tab |
| Reply to the focused row | Enter | — |
| Send | Enter in composer | — |
| Insert newline | Alt+Enter in composer | — |
| Back / close overlay | Esc | — |

`j` and `k` move the picker cursor. `Enter` opens the highlighted
conversation and `Esc` closes the picker. Other navigation keys include
`g`/`Home` for the oldest loaded message, `G`/`End` for the newest,
`PgUp`/`PgDn` for ten rows, and `r`, `e`, and `d` for react, edit, and delete.

## Mode isolation

Key handling is mode-dependent. In navigation mode and the channel picker,
the keys above perform actions. In composer mode every printable character,
including `j`, `k`, `c`, `i`, `r`, `q`, and `?`, is inserted into the draft.
The composer reserves only Enter (send), Alt+Enter (newline), cursor keys,
Home/End, Backspace, and Esc (leave while retaining the draft). The bottom
hint identifies the current mode.

## Resize and failure behavior

On resize, recompute the layout while retaining the selected channel, focused
row, draft text, and reply target. A reconnect or a failed send must not clear
the draft. When a too-small terminal grows to 24×6 or larger, replace the size
message with the appropriate layout without restarting the session.

## Acceptance criteria

1. A 40×10 terminal can select a conversation, scroll, send, reply, and quit.
2. A terminal below 24×6 starts, displays the size message, and recovers
   automatically after resize; focus, draft, and reply target survive.
3. A 79-column terminal has no horizontal overflow and wraps long URLs and
   words.
4. An 80×12 terminal keeps the wide layout and existing key behavior.
5. A phone keyboard without Tab, function keys, or mouse can complete the core
   flow with `j/k`, `c`, `i`, `r`, Enter, Esc, and `q`.
6. Help opens and closes in every mode without changing focus or draft text.
7. Existing event rendering, optimistic sends, and relay error behavior remain
   unchanged.

## Delivery order

1. Replace the global 80×12 startup rejection with layout selection and the
   lower 24×6 size message.
2. Add the narrow channel picker and compact row rendering while reusing the
   existing focus and composer state.
3. Recompute layout on resize and preserve draft/focus/reply state.
4. Verify manually at 24×6, 40×10, 79×12, and 80×12, then run the repository's
   full checks.
