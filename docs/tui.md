# buzzx TUI

## Problem

`buzzx tui` currently rejects terminals smaller than 80 columns by 12 rows.
That prevents people using a phone terminal, a split SSH window, or a compact
console from using the chat client. The product needs to preserve the core
conversation loop on narrow screens without squeezing the desktop layout into
unreadable columns.

This specification defines the TUI capability, including its responsive presentation. It changes the presentation layout only. Channel membership,
timeline rows, focus behavior, composer semantics, relay transport, and event
contracts remain the same as described in [tui-use.md](tui-use.md) and
[render-contract.md](../design/render-contract.md).

## Layout modes

The client selects a mode from the terminal's reported width and height. It
does not infer a device type.

| Terminal size | Mode | Behavior |
| --- | --- | --- |
| 80 columns × 12 rows or larger | Wide | Existing three regions: channels, timeline, composer. |
| 40–79 columns and 10 rows or larger | Narrow | One-column timeline. `c` opens a full-screen channel picker. Composer stays at the bottom. |
| 24–39 columns and 6–9 rows | Minimal | One-column timeline with compact author, age, reaction, and status text. |
| Below 24 columns or 6 rows | Too small | Start the session and show a size message. Keep listening for resize; switch modes automatically when the terminal is large enough. |

Narrow and minimal modes never introduce horizontal scrolling. Message bodies
wrap to the available width. Long words and URLs may break at character
boundaries; the underlying message remains unchanged.

The top line contains the current channel and the connection state. The middle
area contains the focused timeline row and its surrounding rows. The composer
and a one-line status/mode hint remain at the bottom. Descriptions, connection
details, and help are overlays so they do not consume permanent space.

## Unified keys

The action meaning is identical in every layout. Letter keys are the primary
path for phone terminals; desktop keys remain aliases.

| Action | Primary key | Alias |
| --- | --- | --- |
| Move focus / select item | `j` / `k` | Up / Down |
| Open channel picker | `c` | `1`–`9` jump directly |
| Compose a new message | `i` | Tab |
| Reply to the focused row | `r` | Enter in navigation mode |
| Send | Enter in composer | — |
| Insert newline | Alt+Enter in composer | — |
| Back / close overlay | Esc | — |
| Help | `?` | — |
| Quit | `q` | — |

`j/k` always mean previous/next item in a selectable list. Numeric shortcuts
remain direct channel jumps when the channel exists.

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

1. A 40×10 terminal can select a channel, scroll, send a new message, reply,
   and quit.
2. A terminal below 24×6 starts, displays the size message, and recovers
   automatically after resize; focus, draft, and reply target survive.
3. A 79-column terminal has no horizontal overflow and wraps long URLs and
   words.
4. An 80×12 terminal keeps the current wide layout and existing key behavior.
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
