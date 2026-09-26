# Planned TUI visual hierarchy

Status: product review draft; not implemented. Based on main `2af8daf`.
This slice makes existing conversation, thread, Inbox and Agents surfaces
consistent and easier to scan. Interaction and data semantics remain in
[tui-use.md](tui-use.md). This document owns the proposed visual treatment;
where older layout drawings differ, these proposals apply only after this
slice is implemented. No new navigation, themes, panels or protocol is added.

## Direction

Use quiet structure and explicit focus. Compare two options: more bordered
cards around each message improve separation but consume scarce terminal rows;
a shared header, minimal separators and whitespace preserve reading space.
Choose the latter. Keep each author's identity visible; do not group consecutive
messages or collapse content in this slice.

## Visual roles

Use the terminal's default background and foreground rather than forcing a dark
RGB palette. The accompanying dark mockup illustrates roles, not fixed colors.

| Role | Treatment | Non-color cue |
| --- | --- | --- |
| Body | Default foreground, normal weight | Indented under author |
| View/conversation title | Bold default foreground | Explicit channel, Thread, Inbox, or Agents label |
| Selected conversation | Reverse label and leading cursor | `>`; reserve unread state separately |
| Focused message | Cyan and bold author line; body stays normal | `>` in the message gutter |
| Other author | Bold default foreground | Author on its own line except compact rows |
| Time, separators, key hints | Dim default foreground | Secondary position, no semantic dependence on dim |
| Direct mention | Yellow and bold mention signal/header | Existing `@` signal; do not tint entire body |
| Pending or uncertain | Dim pending row; normal uncertain text | Explicit Sending or Unconfirmed text |
| Failure | Red and bold short label | Explicit Failed plus actionable reason |
| Partial, unknown, reconnecting | Yellow short label | Partial, Unknown or Reconnecting words |
| Agent state | Normal foreground; warning/error roles when applicable | Existing state words; no color-only online dot |

Respect existing NO_COLOR handling: omit hue, keep markers and bold/reverse.
Do not dim message bodies, reply targets, required errors or Agent state words.
No blinking, animation, gradients, custom fonts, rounded chat bubbles, or emoji
as the only status indicator. Selected conversation and focused message can
coexist: their different treatments distinguish location from action target.

## Shared structure

At wide sizes keep the existing sidebar allocation, but use a single quiet
vertical separator instead of a complete surrounding box. In one-column
layouts the conversation picker remains the entry to Inbox. A thread always
uses its existing full-screen layout. Agents keeps its existing list/detail
flow; no side-by-side redesign.

Use one header row for location. Keep the timeline gutter two cells wide.
Message headers contain author, age, and existing reply/root indicators; bodies
wrap under the header. Use one blank line between messages only when at least
12 terminal rows are available. Below that, remove decorative gaps first.
Never reserve blank rows that would hide the focused content or input target.

Navigation has no empty composer rectangle. While composing, replace the box
and its duplicated title with one target label and the existing input area.
A channel new message says New message; replies say Reply to <author>; editing
says Edit own message. A thread never labels its input New message. Changing
mode must not clear or retarget the buffer.

Reserve the bottom row for mode and state: Nav, Reply or Edit followed by a
short meaningful result, for example Reply | Failed: unknown name. The row
above holds mode-specific keys. In a compact composer the explicit target,
input, keys and status each keep one row, with one context row and one header.
When there is no error, omit the relay URL from the main status; retain it in
existing help. Do not repeat Connected in both header and footer.

Existing thread state priority remains authoritative. For other views use
write failure/uncertainty before connection failure, then incomplete data,
then transient success. Keep obscured details available in existing help;
do not remove the state just because a higher-priority state is shown.
Long text clips at cell boundaries with `...`; preserve mode/state words and
clip names first. Help exposes full labels and actionable errors. Required
errors must not disappear behind decorative hints.

## Proposed terminal frames

Sample data, not screenshots. Each frame fits the named cell budget; trailing
padding is omitted. `*` below stands for the existing unread dot, not a new
signal. Numeric shortcuts and unread candidate counts retain their meaning.

### Channel reading: 80x12

```text diagram
Inbox: All          | #buzzx-tui
 Channels           |   Alice  2m
>1 @ 2 buzzx-tui     |   Can we ship the thread view?
 2 * 3 general      |
 3     design       | > Buzzx Build  1m
 DMs                |   Checks passed. Ready for review.
 4 * 1 Alice        |
 5     Buzzx Build  |   Product  30s
                    |   I will verify the live evidence.
                    |
c: chats  f: filter  | t: thread  Enter: reply  i: write  ?: help
Nav | Connected
```

### Reply: 40x10

```text diagram
#buzzx-tui
  Alice  2m
  Can we ship the thread view?
> Build  1m
  Checks passed. Ready for review.

Reply to Build
> Please share the live evidence.
Enter: send  Esc: clear target
Reply | Connected
```

The channel Esc hint above follows the existing channel behavior. The thread
composer uses Esc: nav instead. Do not visually imply these are the same action.

### Thread reply: 24x6

```text diagram
Thread #buzzx...
> Build: Checks passed.
Reply to Build
> Please share evidence
Enter:send Esc:nav
Reply | Connected
```

### Inbox picker: 24x6

```text diagram
Inbox: For you
 Channels
>1 @ 2 buzzx-tui
 DMs
 4 * 1 Alice
Enter:open f:filter Esc
```

Picker rows scroll within this budget. When incomplete/read-sync errors exist,
reserve the penultimate row for the short state and scroll list rows instead;
keep the bottom action row. The empty state is shown only after lookup succeeds.

### Agents list: 24x6

```text diagram
My agents
> Build     Working
  Product   Unknown
  Research  Working

Enter:details Esc:back
```

Names above are sample display names. Preserve the existing Agent state
vocabulary; long states may wrap and consume another list row. Do not replace
No active turn observed with Idle, or use green to imply healthy execution.
In failures the spare row carries the error; otherwise it remains list space.

## Visual acceptance and delivery

Compare actual before/after terminal captures at 80x12, 79x12, 40x10, 24x6,
and a comfortable desktop size such as 120x30. In one review sheet show channel
navigation, reply, thread, filtered Inbox and Agents using the same fixtures.
Also capture unknown/read-sync failure, long names, a wrapped message, pending,
uncertain write and a mention-resolution error. No state is proven by a mockup.

Verify dark and light terminal defaults and NO_COLOR. The reviewer must be
able to identify location, focused row, input destination and any error without
hue. Test CJK, combining characters and emoji with a terminal-cell-aware
renderer: no overlapping columns, clipped cursor or displaced status.

No keyboard mapping, focus/viewport, read marker or recipient behavior may
change as a side effect. Use existing regressions plus the full repository
checks, and exercise the real TUI loop required by AGENTS.md. Final acceptance
requires actual rendering evidence, integration and installed-version identity.
