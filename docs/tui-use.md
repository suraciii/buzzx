# Using `buzzx tui`

`buzzx tui` opens a live terminal chat session against one Buzz relay.

Start it:

```text literal
export BUZZ_PRIVATE_KEY=<hex-or-nsec>
buzzx tui
```

The relay defaults to `http://localhost:3000`. See
[configuration.md](configuration.md) for other ways to set identity and relay.

## Layout

The client picks its layout from the terminal's size and never infers a device
type. The session is the same state in every layout: the selected channel, the
focused row, the draft, and the reply target survive a resize.

| Terminal | Layout | What it shows |
| --- | --- | --- |
| 80 by 12 or larger | Wide | The three regions below. |
| 40 to 79 columns, 10 rows or larger | Narrow | One column: the timeline. `c` opens the channel list over the screen, and the composer keeps a box at the bottom. |
| 24 to 39 columns, 6 rows or larger | Minimal | One column with compact rows and a one-line composer while composing. |
| Smaller than 24 columns or 6 rows | Too small | A size message. Resize the terminal and the session continues. |

### Wide

```text diagram
+-----------+---------------------------------------------+
| channels  | #general                                     |
|           |                                             |
| 1 general | alice         2m                            |
| 2 random  | the migration is on main                    |
| 3 eng     |                                             |
|           | bob           1m                            |
|           | @tyler can you look                         |
|           | +2 reactions                                |
|           | Agent A typing...                           |
|           +---------------------------------------------+
|           | reply to bob - enter to send, esc to clear  |
+-----------+---------------------------------------------+
 status: connected https://relay.example | sent | mode: nav | ?=help
```

The channel list on the left shows the identity's channels, the active one
highlighted. The timeline on the right shows the selected channel's messages.
The composer at the bottom is the text input. The typing line appears above the
composer while another identity is composing, and takes no row when nobody is.
The diagram's `...` is the client's single-character ellipsis; the line itself
is documented under [typing indicators](#typing-indicators).

### Narrow and minimal

One column: the timeline, with the channel and the connection state on the top
line and the composer and a status hint at the bottom. The channel list is an
overlay, because a single column has no room for a permanent sidebar. `c`
opens it full screen, `j` and `k` move, `Enter` opens the channel, and `Esc`
closes it. In minimal the rows carry compact markers, and the composer is one
line while it is being used, so the timeline keeps as many rows as the
terminal has.

A message body wraps to the column's width. Long words and URLs break at
character boundaries; the message itself is unchanged.

### Too small

Below 24 columns or 6 rows the client shows the size it needs and the size it
has, and keeps running. Resizing the terminal brings the layout back with the
selection, the draft, and the reply target intact.

The session always exposes one focused row in the timeline. Scrolling moves the
focus with the viewport and keeps the focused row visible. Actions that target a
row use this focus, so a reply, reaction, edit, or delete never acts on an
unrelated message at the bottom of the timeline. When a channel opens, focus is
the newest loaded row. If there are no rows, `Enter` opens a new-message
composer without a reply target.

Unread state is not shown in the first phase. It needs a read marker on the
relay (kind 30078) plus the events observed while a channel is not selected,
and neither exists yet. When both do, each row gains a count.

## Keys

Keys have two modes. The timeline starts in navigation mode.

In navigation mode:

- `j` or the down arrow moves to the next item. In the wide layout that is the
  next channel, whose list is on screen; in the one-column layouts it is the
  next row of the timeline.
- `k` or the up arrow moves to the previous item, the same way.
- `1` through `9` jump to the channel with that number, in every layout.
- `c` opens the channel picker. `Enter` opens the highlighted channel and
  `Esc` closes the picker.
- `g` or `Home` focuses the oldest loaded message.
- `G` or `End` focuses the newest message.
- `PgUp` and `PgDn` move ten rows.
- `i` or `Tab` opens the composer for a new message.
- `Enter` opens the composer with the focused row as the reply target. The
  reply target is shown in the composer and can be cleared with `Esc` before
  sending.
- `r` reacts with the default thumbs-up emoji to the focused row. Pressing it
  again on your own reaction removes it.
- `e` edits the identity's own focused row.
- `d` deletes the identity's own focused row.
- `?` toggles the key help, and `Esc` closes it.
- `q` quits.

In composer mode every printable character, `j`, `k`, `c`, `i`, `r`, and `?`
included, is typed into the draft. The composer reserves `Enter` (send),
`Alt+Enter` (newline), the cursor keys, `Home`, `End`, `Backspace`, and `Esc`
(clear the reply or edit target, then leave the composer with the draft kept).

The composer has no external editor integration in the first phase. A long
message is typed in an editor and sent with
`buzz messages send --channel <uuid> --file <path>`, which is the same relay
write the composer performs.

## What the timeline shows

Each message is two or more lines: the author and the age, then the body, then
reactions. Messages are grouped by the presence or absence of a thread
reference. A reply carries a marker naming what it answers.

A row that carries the identity's own `p` tag is highlighted. A broadcast
reply carries an `@channel` marker. An attachment shows the `imeta` tag's
filename and a marker; the body keeps its markdown line.

A pending send renders dimmed with a `...` marker until the relay answers. If
the relay refuses the send, the row disappears, the composer text returns, and
the status line shows the relay's reason.

## Typing indicators

When another identity is composing in the selected channel, one dim line
appears between the timeline and the composer. It shows a display name per
identity, at most two, then `+N` for the rest:

```text literal
Agent A typing…
Agent A, Agent B are typing…
Agent A, Agent B +2 are typing…
```

The wide layout writes the names as they are. The one-column layouts use the
compact `@` form, `@Agent A typing…`. When nobody is composing, the line is
gone and the timeline gets the row back.

Channels where someone is composing carry a `…` after the name in the channel
list, and the picker shows the same marker, so activity in a channel you are
not reading is still visible. The marker is not an unread count and it does not
reorder the list.

Indicators are ephemeral: they are never part of the history, they are not
counted as unread, and they do not survive a restart. One expires 8 seconds
after the last repeat from its author, and it ends immediately when that
author's message arrives, when the connection drops (the status line shows
`reconnecting`), or when the channel closes or leaves your channel list. If the
relay refuses a channel's typing feed, the status line says so and the rest of
the client keeps working. Another client signed in with the same identity does
not show up as a typist in your session.

Two limits are worth knowing. Typing is shown per channel, not per thread, so a
reply in progress looks the same as a new message in progress. And the line
reports typing only: whether an agent has accepted a request, is running a
tool, or has failed needs its own status event from the agent's runtime, which
does not exist yet.

## Status line

The bottom line shows three things: the connection state, the relay's answer to
the last write, and the mode. The connection state is one of `connecting`,
`connected`, `reconnecting`, or `failed`.

## What `buzzx tui` does not do

- It does not render images, video, or audio. Attachments show as filename
  lines.
- It does not join huddles. Voice needs the relay's separate audio WebSocket,
  which is out of scope for the first phase.
- It does not search. Use the `buzz` CLI.
- It does not show DMs in the channel list. DMs arrive and render, but the
  list is channel membership. Since DMs do not appear there, an unread DM is
  not visible until you switch to it, which is one of the things a
  later phase fixes.
- It does not configure the relay, manage members, or moderate. Use the
  `buzz` CLI or the desktop app.
- It does not show your own read state on other devices. Read
  markers sync through the relay; the display of that state is a later phase.
- It does not publish a typing indicator of its own. The line above the
  composer shows other identities composing; publishing your own is a later
  phase, and the event contract on the relay already accepts it.

## Terminal requirements

The TUI needs a terminal that reports a size and supports the alternate
screen. It runs at 24 columns by 6 rows and larger, in the layout the size
allows. Below that it shows the required size instead of a broken layout, and
it switches to a layout as soon as the terminal grows.

Raw mode is entered at start and restored at exit, including on error. A
terminal that is left in raw mode after a crash is a bug.
