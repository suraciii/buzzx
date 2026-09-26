# TUI visual language

Status: revision 4, product review draft. Not implemented. This replaces the
visual direction in revisions 1-3. Previous images remain historical artifacts.
The [review atlas](assets/tui-design-system.html) contains four boards and dark,
light and NO_COLOR studies. Its controls are for reviewing the design, not new
client controls. Sample messages and Agent states are invented fixtures.

![Conversation design study](assets/tui-system-language.png)

## Product character

buzzx is a quiet place to collaborate from a terminal. People read a shared
conversation, decide what needs their attention, and write to an explicit
destination. The visual language should make those three acts recognizable
without giving every message a panel or every Agent a special treatment.

The UI is calm in normal use, precise about identity and destination, and
explicit when its knowledge is incomplete. A conversation is the primary
surface. Navigation supports it; runtime information does not take it over.
This follows the [product contract](../core.md).

## Review of the previous direction

Revision 3 styled isolated screens before defining their shared grammar. The
selected conversation, selected message and input all used blue emphasis, so
several regions competed to be the action target. A highlighted message kept
its accent while the composer was active. The proposed author/body gap broke
message grouping, and the 26-cell roomy example did not demonstrate the
compact sidebar allocation.

The sample also replaced the existing unread dot with an unexplained asterisk.
It mainly demonstrated a spacious dark channel and a narrow reply; light,
NO_COLOR, list/detail, refused sends and uncertain sends were left as future
checks. Passing repository checks could not establish that the design worked.

Choose a shared layout and semantic emphasis before styling individual views.
Keep one message grammar, one list selection, one composer, and one state area.
The specimens below demonstrate these components together and under pressure.

## Shared anatomy

Every view has four stable positions: context at the top, content in the
remaining space, available keys near the bottom, and the highest-priority state
on the last row. A composer occupies the bottom of content only while writing.
Help and overlays replace the active surface; they do not introduce another
permanent column. The current [keys and behavior](tui-use.md) remain authoritative.

| Surface | Context | Main content | Action destination |
| --- | --- | --- | --- |
| Channel or DM | Conversation name | Continuous timeline, sidebar in wide mode | New message or explicit reply/edit target |
| Thread | Conversation / Thread | Root and replies in one timeline | Explicit root, reply or edit target |
| Inbox picker | Inbox / active filter | Channels and DMs with one selection | Selected conversation |
| My agents | My agents | Names and observed states | Selected Agent's details |
| Agent detail | My agents / name | State, evidence age, accessible contexts | Selected accessible channel |
| Help | Help / current context | Full labels, reasons and available keys | Return to previous view |

The channel sidebar is the embedded Inbox list. Opening the picker enlarges
that same list, not a separate product. Agent detail uses the same selection
row for accessible contexts. Unavailable contexts remain plain, non-actionable
text. These views do not gain search fields, tabs or management buttons.

## Visual grammar

Three meanings use different treatments. They may coexist without merging:

- **Place:** the current conversation has a neutral filled selection row. Its
  name also appears in the context header. It never gets the action accent.
- **Action:** in reading mode a two-cell gutter with `>` and an accented author
  identifies the targeted row. In writing mode emphasis moves to the composer
  target and cursor; the timeline keeps ordinary author styling. The saved row
  focus still exists but no longer competes visually with the active editor.
- **Attention:** a direct mention uses the verified row's `@you` label and the
  Inbox's existing signal. Unknown, stale and failed states have explicit words.
  Their treatment survives selection and does not tint the message body.

An Inbox, Agent or context list has one filled selected row and a leading `>`
when it is the active picker. The embedded sidebar uses its existing number
slot; reverse video supplies a non-color selection cue. In the timeline a
message is never a full-width inverse block or a filled card. Focus moves
without changing its line count or padding.

While composing, the target describes where the draft will go. It is not
inferred from the author nearest the bottom of the timeline. A reply target
does not mean that author will receive a notification. Mention styling follows
signed recipients, not a string that merely resembles an `@name`.

## Type, spacing and alignment

Use the terminal's single monospace size. Author and context labels are bold;
body text is normal foreground; age, reply metadata and hints are secondary.
All authors use the same treatment. No synthetic human/assistant bubble style,
role color, avatar or inferred bot badge is added.

An author line is immediately followed by its body, always on the same left
edge. At 16 rows or more, one blank row separates message groups and list items
may have one intervening blank row. Below 16 rows, remove decorative gaps.
Existing newlines in message content are content and are not removed.

The channel sidebar stays 22 cells wide. Content starts with a two-cell focus
gutter and one additional cell after the sidebar; an overlay uses a two-cell
outer inset plus its focus gutter. At widths below 40, reduce outer padding
before reducing body width. At 24 columns, padding can be zero. At 24x6,
composition uses exactly the six rows illustrated in the density studies.

Leave no blank row inside a short composer between its target and input. A
roomy composer can have one. The cursor is a terminal cell after the insertion
point, not an extra icon. Use at most one neutral input surface and a short
left rule beside its target; remove full box borders. At small sizes the
target and cursor establish the input region without decoration.

Conversation signal slots retain the existing five-cell reservation. Names
shorten only after allocating their state. In an Agent list, align state words
at a common column where they fit; otherwise wrap the state under the name.
Do not abbreviate `No active turn observed` to a different claim. Message
bodies wrap; labels may clip; complete names and reasons remain in help/detail.
Measure terminal cells, including wide and combining characters.

## Palette and terminal defaults

Color reinforces a structure that is already legible without it. The following
are controlled review values, not a theme setting or a requirement to override
the user's terminal background. There are three neutral surfaces and three
semantic foreground accents; there are no per-page palettes.

| Role | Dark study | Light study | No reliable palette / NO_COLOR |
| --- | --- | --- | --- |
| Canvas / body | `#14181a` / `#e7ebe8` | `#fafaf7` / `#25332e` | Terminal defaults |
| Navigation / input surface | `#1d2326` | `#eeefeb` | Default background, spacing and target label |
| Selected list row | `#303a3e` | `#dce3de` | Reverse default foreground/background |
| Secondary text | `#a1ada8` | `#56645e` | Default foreground, secondary position |
| Action accent | `#9bd5c9` | `#1a6960` | Bold, `>` or input target; underline optional |
| Attention | `#edc57d` | `#835506` | Bold plus explicit signal/state |
| Failure | `#f29da5` | `#ad3545` | Bold reason and correction |

The portable baseline uses default foreground/background, bold, reverse
selection and explicit labels. Where the terminal offers a known contrasting
palette, the semantic accents and neutral surfaces can refine that baseline.
Do not hardcode dark-study fills over an unknown background or infer terminal
lightness from the OS. No new color-detection protocol or theme configuration
is part of this proposal. If the fill cannot be made reliable, ship the
portable baseline; do not quietly claim the colored study is implemented.

Body, labels, state and key hints should reach 4.5:1 contrast in controlled
palettes. Selection cannot hide a mention/error label; its words remain even
if a colored label must use the selected foreground. Normal connection and
Working do not use green health indicators. Unknown is readable secondary
text, never a disabled-looking label.

## Component states

Use one state treatment across surfaces. Business meanings and transition
rules live in [the manual](tui-use.md); these rules describe their appearance.

| Component state | Appearance | Information retained |
| --- | --- | --- |
| Reading | Bare transcript, small row focus, no empty input region | Author, body, action target |
| New message / Reply / Edit | Same input surface, explicit target, cursor | Draft and destination |
| Pending send | Pending label at attempted row, secondary text | Body remains legible |
| Refused write | Failure reason in state row; restored draft in composer | Target, correction and route to full help |
| Uncertain write | `? Unconfirmed` on attempted row; attention state below | Attempt remains visible, no retry invitation |
| Reconnecting | Loaded content remains; stale/reconnecting state | Existing context, disabled publication reason |
| Loading | Plain loading label in content | No premature empty-state claim |
| Empty | Plain factual label, such as No agents | Back or meaningful next action |
| Read failure | Reason with available read retry/back hint | Previous content only when labeled stale |
| Partial or unknown | Explicit coverage/state label | No invented zero or completion claim |
| Deleted thread root | Root deleted in the root's place | Remaining replies and valid targets |

Full errors stay in the existing help surface. While composing, `?` is text;
show the correct Esc-to-navigation route before suggesting help. Channel and
thread Esc labels follow their distinct existing behavior. In minimal mode the
last row can omit a redundant mode name to preserve the meaningful state.
Do not collapse two active states by discarding the lower-priority one; the
manual owns priority and how full details remain available.

## Density and specimens

The [shared-surface board](assets/tui-system-surfaces.png) shows thread reply,
Inbox, Agent list and detail. The [resilience board](assets/tui-system-resilience.png)
shows light and NO_COLOR reading, small replies, an uncertain send and a
refused mention. The [density board](assets/tui-system-density.png) shows the
80/79-column boundary, editing, a deleted root while disconnected, and wrapped
Agent state at 24x6. These are visual studies of separate fixtures, not a
continuous interaction recording.

The [existing layout modes](tui.md#layout-modes) own the breakpoints. Height pays
for spacing; width determines whether the conversation list can sit beside the
timeline. A full-screen thread stays one column at every width. Header, target,
input, keys and state must fit before allocating optional blank rows. Do not
shrink text or preserve a wide layout by squeezing each region.

## Alternatives and reference basis

A panel-heavy dashboard offers many visible boundaries but spends scarce rows
on framing and makes unrelated tasks compete. A bare transcript minimizes
chrome but leaves multi-conversation navigation and write targets too implicit.
Choose a continuous transcript with a quiet navigation region and one explicit
action area. This is the smallest composition that covers the existing tasks.

The inspected official [OpenCode image](https://github.com/anomalyco/opencode/blob/dev/packages/web/src/assets/lander/screenshot.png),
[pi image](https://github.com/badlogic/pi-mono/blob/main/packages/coding-agent/docs/images/interactive-mode.png)
and [Oh My Pi image](https://github.com/can1357/oh-my-pi/blob/main/assets/python.webp)
show useful transcript/input separation. Their runtime controls and two-role
conversation model do not define Buzzx's multiplayer message semantics.

Buzz's own [neutral theme previews](https://github.com/block/buzz/blob/93114c9c65138397de39729fde0a816eb9f314ab/desktop/src/shared/theme/ThemePreviewFrame.tsx)
and [mobile navigation roles](https://github.com/block/buzz/blob/93114c9c65138397de39729fde0a816eb9f314ab/mobile/lib/shared/theme/buzz_theme.dart)
support neutral content and secondary metadata. The terminal translation uses
alignment and small semantic accents, not their gradient or pixel dimensions.
References were inspected as images or source, not tested as live products.

## Review and implementation acceptance

Review the atlas with its annotations covered: can a reader identify where they
are, which row an action targets, where a draft will go, and which state needs
attention? Changing pages should not require relearning those cues. Removing
color must leave each answer intact. The self-review supports this direction;
it is not user-test evidence.

Before implementation is accepted, compare real captures against these studies
at 120x30, 80x12, 79x12, 40x10 and 24x6. Include long author names, ambiguous
identities, CJK, combining characters, emoji and URLs. Check dark/light/default
terminal palettes and NO_COLOR. Exercise loading, empty, stale, partial,
refused, uncertain and deleted-target states without losing the draft or hiding
its destination. In particular, verify selection and state contrast together.

The atlas checks geometry and appearance of static fixtures only. Runtime
wrapping, cursor movement, resize, terminals' palette behavior and actual
write outcomes remain unverified. Implementation still requires repository
checks, the real terminal workflow required by [AGENTS.md](../AGENTS.md), and
reviewed captures from the usable binary. A spec or browser image is not that
evidence.
