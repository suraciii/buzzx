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
| `>=112` columns × `>=16` rows | Desk | A 24-column conversation sidebar, including its divider, beside the main channel surface. The sidebar is open by default and `Ctrl+S` toggles it. |
| `>=80` columns × `>=12` rows, but below Desk width or height | Wide | Full-width timeline, composer, and status line; no permanent sidebar. |
| 40–79 columns and `>=10` rows | Narrow | The same one-column timeline. `c` opens the full-screen conversation switcher. Composer stays at the bottom. |
| 24–39 columns and `>=6` rows | Minimal | One-column timeline with compact rows and a one-line composer while composing. |
| Below 24 columns or 6 rows | Too small | Start the session and show a size message. Keep listening for resize; switch modes automatically when the terminal is large enough. |

Desk's sidebar and main surface are two panes of one channel page. `h` and `l`
move operation focus between them; `j/k`, `Home/End`, `g/G` and `PgUp/PgDn`
move only the focused pane's cursor. `Enter` opens a sidebar conversation.
The sidebar cursor is not timeline focus; moving it does not change selection,
viewport, event focus, or read marker. `Esc` from the sidebar returns focus to
the timeline. `Ctrl+S` closes the sidebar without changing channel state and
reopens it with its session cursor and filter retained.

The sidebar uses the existing `Sections`, labels, signals, ordering and
`All`/`Unread`/`For you` filter. The full-screen switcher opened with `c` is
the same conversation data and filter behavior, not a second list. Existing
visibility behavior, including the Archived filter where it is exposed,
remains part of that shared list contract.

Below Desk the layout is timeline first: the header names the open
conversation and carries the Inbox answer, and the timeline keeps the
available width. The conversation list is the switcher opened with `c`,
which groups rows under `Channels` and `DMs`. A row signal carries the unread
message count when known (`● 3`, `@ 2`), `?` for unknown, `Read` for a
switcher row retained after it stops matching, or no signal. Its signal cell
is reserved before the label is clipped; typing adds `…` after the label.
Read-state details and limits are in
[shared-core.md](../design/shared-core.md#inbox-and-read-state).

## Unified keys

The action meaning depends on the layout and on whether the sidebar, switcher,
command palette, or help is open. `j` and `k` move the focused pane's cursor:
timeline rows in the timeline pane and conversations in the Desk sidebar. `c`
opens the conversation switcher at every width, and `Ctrl+P` opens the command
palette. `f` cycles the existing Inbox filters, including `Archived` where
available, in navigation mode; `f` or Tab cycles them in the switcher. Help
opens with `?`; while open, `j`/`k`, `PgUp`/`PgDn`, and `g`/`G` scroll it, and
`Esc` or `?` closes it.

| Action | Primary key | Alias |
| --- | --- | --- |
| Move focus / select item | `j` / `k` | Up / Down |
| Open conversation switcher | `c` | — |
| Open the command palette | `Ctrl+P` | — |
| Open the Agents overlay | `a` | — |
| Change Inbox filter | `f` | Tab in the switcher |
| Compose a new message | `i` | Tab |
| Reply to the focused row | Enter | — |
| Send | Enter in composer | — |
| Insert newline | Alt+Enter in composer | — |
| Back / close overlay | Esc | — |

`j` and `k` move the switcher cursor, typing filters the list by name, `Enter`
opens the highlighted conversation and `Esc` closes the switcher. Other
navigation keys include
`g`/`Home` for the oldest loaded message, `G`/`End` for the newest,
`PgUp`/`PgDn` for ten rows, and `r`, `e`, and `d` for react, edit, and delete.

`a` opens the Agents overlay: the Agents the signed-in identity owns, one row
per Agent with its working state. `j`/`k` select, `Enter` opens the selected
Agent's detail, `Esc` goes back a level and then closes the overlay, and `a`
closes it from either level. In the detail, `j`/`k` select one observed
working context and `Enter` opens that conversation. The overlay takes the
whole terminal in every layout mode, because a status word and its meaning
must not be swapped for a squeezed second column. What each state means is in
[tui-use.md](tui-use.md#agents-overview).

## Mode isolation

Key handling is mode-dependent. In navigation mode, the channel picker, and
the Agents overlay, the keys above perform actions, and an open overlay
isolates the keys it does not use: no conversation action fires behind it. In
composer mode every printable character, including `j`, `k`, `c`, `a`, `i`,
`r`, `q`, and `?`, is inserted into the draft. The composer reserves only
Enter (send), Alt+Enter (newline), cursor keys, Home/End, Backspace, and Esc
(leave while retaining the draft). The bottom hint identifies the current
mode.

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
7. A 24×6, 40×10, 79×12, and 80×12 terminal can open the Agents overlay,
   select an Agent, open an observed working conversation, and return, with
   the draft, reply target, and read state unchanged.
8. Existing event rendering, optimistic sends, and relay error behavior remain
   unchanged.

## Delivery order

1. Replace the global 80×12 startup rejection with layout selection and the
   lower 24×6 size message.
2. Add the narrow channel picker and compact row rendering while reusing the
   existing focus and composer state.
3. Recompute layout on resize and preserve draft/focus/reply state.
4. Verify manually at 24×6, 40×10, 79×12, and 80×12, then run the repository's
   full checks.
