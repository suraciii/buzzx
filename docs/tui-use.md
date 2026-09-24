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
|           |                                             |
|           +---------------------------------------------+
|           | reply to bob - tab to edit, enter to send   |
+-----------+---------------------------------------------+
 status: connected https://relay.example            Tab=edit  ?=help
```

Three regions. The channel list on the left shows the identity's channels, the
active one highlighted. The timeline on the right shows the selected channel's
messages. The composer at the bottom is the text input.

Unread state is not shown in the first phase. It needs a read marker on the
relay (kind 30078) plus the events observed while a channel is not selected,
and neither exists yet. When both do, each row gains a count.

## Keys

Keys have two modes. The timeline starts in navigation mode.

In navigation mode:

- `j` or the down arrow selects the next channel.
- `k` or the up arrow selects the previous channel.
- `1` through `9` jump to the channel with that number.
- `g` or `Home` scrolls to the oldest loaded message.
- `G` or `End` scrolls to the newest message.
- `PgUp` and `PgDn` scroll ten messages.
- `i`, `Tab`, or `Enter` opens the composer for a new message. Enter also
  sets the reply target to the message nearest the bottom of the timeline,
  which is the one you are looking at.
- `r` reacts with the default thumbs-up emoji to the message nearest the
  bottom.
- `e` edits the identity's own message nearest the bottom.
- `d` deletes the identity's own message nearest the bottom.
- `?` toggles the key help.
- `q` quits.

In composer mode:

- `Esc` leaves the composer and keeps the typed text.
- `Enter` sends.
- `Alt+Enter` inserts a newline.
- The up and down arrows move the cursor between lines.
- The left and right arrows, `Home`, and `End` move the cursor within a line.
- `Backspace` deletes back.

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

## Terminal requirements

The TUI needs a terminal that reports a size and supports the alternate
screen. It needs at least 80 columns and 12 rows. Below that it shows a
message asking for a larger window instead of a broken layout.

Raw mode is entered at start and restored at exit, including on error. A
terminal that is left in raw mode after a crash is a bug.
