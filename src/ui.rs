//! Rendering only. `ui.rs` reads `App` state and draws; it never mutates it
//! and never touches the network. The layout mode follows the frame size, and
//! one-column modes reuse the same state as the wide one.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};

use crate::app::{App, ConnState, Mode};
use crate::layout::{self, LayoutMode};

fn no_color() -> bool {
    static NO_COLOR: std::sync::LazyLock<bool> =
        std::sync::LazyLock::new(|| std::env::var("NO_COLOR").is_ok());
    *NO_COLOR
}

fn author_style() -> Style {
    if no_color() {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    }
}

fn pending_style() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

fn mention_style() -> Style {
    if no_color() {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    }
}

fn highlight_style() -> Style {
    Style::default().add_modifier(Modifier::REVERSED)
}

fn conn_word(state: ConnState) -> &'static str {
    match state {
        ConnState::Connecting => "connecting",
        ConnState::Connected => "connected",
        ConnState::Reconnecting => "reconnecting",
    }
}

/// Seconds to a short human age, coarse on purpose.
fn age(created_at: u64, now: u64) -> String {
    let secs = now.saturating_sub(created_at);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

/// The lines one row renders as: the header, the wrapped body, attachments,
/// and reactions. One-column modes ask for the compact markers, so a row
/// spends its width on the message instead of on words like `(reply)`.
fn row_lines(row: &crate::content::Row, now: u64, width: u16, compact: bool) -> Vec<Line<'static>> {
    let (reply, broadcast, edited) = if compact {
        (" <", " @c", " (ed)")
    } else {
        (" (reply)", " @channel", " (edited)")
    };
    let mut header: Vec<Span<'static>> = Vec::new();
    if row.mentions_me {
        header.push(Span::styled("*".to_owned(), mention_style()));
    }
    header.push(Span::styled(row.author.clone(), author_style()));
    header.push(Span::raw(" "));
    header.push(Span::raw(age(row.created_at, now)));
    if row.pending {
        header.push(Span::styled(" ...".to_owned(), pending_style()));
    }
    if row.uncertain {
        header.push(Span::styled(" ?".to_owned(), mention_style()));
    }
    if row.parent_id.is_some() {
        header.push(Span::raw(reply));
    }
    if row.broadcast {
        header.push(Span::raw(broadcast));
    }
    if row.mentions_me {
        header.push(Span::styled(" @you".to_owned(), mention_style()));
    }
    if row.edited {
        header.push(Span::raw(edited));
    }
    let mut lines = vec![Line::from(header)];

    let body_style = if row.pending || row.uncertain {
        pending_style()
    } else {
        Style::default()
    };
    for line in row.body.split('\n') {
        for chunk in wrap(line, width) {
            lines.push(Line::styled(chunk, body_style));
        }
    }
    if let Some(attachment) = &row.attachment {
        lines.push(Line::styled(
            format!("[file] {attachment}"),
            pending_style(),
        ));
    }
    if !row.reactions.is_empty() {
        let counters: Vec<String> = row
            .reactions
            .iter()
            .map(|(emoji, count)| match (*count, compact) {
                (1, _) => emoji.clone(),
                (n, true) => format!("{emoji}{n}"),
                (n, false) => format!("{emoji} x{n}"),
            })
            .collect();
        lines.push(Line::styled(counters.join(" "), pending_style()));
    }
    lines
}

/// Greedy word wrap on characters, simple and predictable for a chat body.
fn wrap(line: &str, width: u16) -> Vec<String> {
    let width = width.max(8) as usize;
    if line.is_empty() {
        return vec![String::new()];
    }
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in line.split(' ') {
        let candidate_len = current.len() + word.len() + !current.is_empty() as usize;
        if candidate_len > width && !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        // A word longer than a line is split on character boundaries; chat
        // bodies can carry long URLs and hashes.
        let mut rest: &str = word;
        while rest.chars().count() > width {
            let take: String = rest.chars().take(width).collect();
            out.push(take);
            rest = &rest[rest
                .char_indices()
                .nth(width)
                .map(|(i, _)| i)
                .unwrap_or(rest.len())..];
        }
        current.push_str(rest);
    }
    out.push(current);
    out
}

/// The visible slice of one composer line around the cursor, so a narrow
/// composer keeps the insertion point on screen. The draft itself is
/// unchanged; this is a view of it.
fn cursor_window(line: &str, cursor: usize, width: usize) -> String {
    let width = width.max(1);
    let cursor = cursor.min(line.len());
    let skipped = line[..cursor].chars().count();
    let start = skipped.saturating_sub(width.saturating_sub(1));
    line.chars().skip(start).take(width).collect()
}

/// The typing line for the open channel: one merged sentence for everyone
/// composing there. Empty when nobody is, so the row costs no space then.
///
/// The wide layout spends its width on the display names; the one-column
/// layouts use the compact `@name` form.
fn typing_line(names: &[String], compact: bool) -> String {
    let shown: Vec<String> = names
        .iter()
        .take(2)
        .map(|name| {
            if compact {
                format!("@{name}")
            } else {
                name.clone()
            }
        })
        .collect();
    let extra = names.len().saturating_sub(shown.len());
    let mut who = shown.join(", ");
    if extra > 0 {
        who.push_str(&format!(" +{extra}"));
    }
    match names.len() {
        0 => String::new(),
        1 => format!("{who} typing…"),
        _ => format!("{who} are typing…"),
    }
}

/// The typing line for the channel on screen.
fn typing_text(app: &App, now: u64, compact: bool) -> String {
    let names = app
        .channels
        .get(app.selected)
        .map(|entry| app.typing_names(entry.id, now))
        .unwrap_or_default();
    typing_line(&names, compact)
}

/// One dim line between the timeline and the composer. It draws nothing when
/// it has nothing to say.
fn draw_typing(frame: &mut Frame, text: &str, area: Rect) {
    if text.is_empty() || area.is_empty() {
        return;
    }
    frame.render_widget(
        Paragraph::new(Line::styled(text.to_owned(), pending_style())),
        area,
    );
}

/// Draw the whole interface for one frame.
pub fn draw(frame: &mut Frame, app: &App, now: u64) {
    let area = frame.area();
    let mode = layout::mode(area.width, area.height);
    match mode {
        LayoutMode::TooSmall => draw_size_message(frame, area),
        LayoutMode::Wide => draw_wide(frame, app, now, area),
        LayoutMode::Narrow => draw_narrow(frame, app, now, area),
        LayoutMode::Minimal => draw_minimal(frame, app, now, area),
    }
    if app.picker.is_some() {
        draw_picker(frame, app, now, area);
    }
    if app.help {
        draw_help(frame, area, mode);
    }
}

/// Channels, timeline, and composer side by side. This is the desktop shape,
/// and its key behavior is the one docs/tui-use.md documents first.
fn draw_wide(frame: &mut Frame, app: &App, now: u64, area: Rect) {
    let outer = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(area);
    let main = Layout::horizontal([Constraint::Length(22), Constraint::Min(40)]).split(outer[0]);

    draw_channel_list(frame, app, now, main[0]);
    let typing = typing_text(app, now, false);
    let column = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(u16::from(!typing.is_empty())),
        Constraint::Length(3),
    ])
    .split(main[1]);
    draw_header(frame, app, column[0], false);
    draw_timeline(frame, app, column[1], now, false);
    draw_typing(frame, &typing, column[2]);
    draw_composer(frame, app, column[3], false);
    draw_status(frame, app, outer[1], true);
}

/// One column: header, timeline, composer, status hint. The channel list is
/// behind `c`.
fn draw_narrow(frame: &mut Frame, app: &App, now: u64, area: Rect) {
    let typing = typing_text(app, now, true);
    let column = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(u16::from(!typing.is_empty())),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .split(area);
    draw_header(frame, app, column[0], true);
    draw_timeline(frame, app, column[1], now, false);
    draw_typing(frame, &typing, column[2]);
    draw_composer(frame, app, column[3], true);
    draw_status(frame, app, column[4], false);
}

/// One column with compact rows. The composer takes a single line, and only
/// while it is being used.
fn draw_minimal(frame: &mut Frame, app: &App, now: u64, area: Rect) {
    let composer_rows = u16::from(app.mode == Mode::Composer);
    let typing = typing_text(app, now, true);
    let column = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(u16::from(!typing.is_empty())),
        Constraint::Length(composer_rows),
        Constraint::Length(1),
    ])
    .split(area);
    draw_header(frame, app, column[0], true);
    draw_timeline(frame, app, column[1], now, true);
    draw_typing(frame, &typing, column[2]);
    if composer_rows == 1 {
        draw_composer_line(frame, app, column[3]);
    }
    draw_status(frame, app, column[4], false);
}

/// The channel and its loading state; the compact modes add the connection
/// state here, because their status line carries the relay's answer.
fn draw_header(frame: &mut Frame, app: &App, area: Rect, with_conn: bool) {
    let entry = app.channels.get(app.selected);
    let name = entry.map(|c| c.name.as_str()).unwrap_or("no channel");
    let loading = entry.map(|c| c.loading).unwrap_or(false);
    let channel = if loading {
        format!("#{name} loading")
    } else {
        format!("#{name}")
    };
    let text = if with_conn {
        format!("{channel} · {}", conn_word(app.conn))
    } else {
        channel
    };
    frame.render_widget(Paragraph::new(text), area);
}

/// One channel list line, `number name` plus the activity marker. The marker
/// is reserved first and the name is clipped to the columns that remain: a
/// name that runs long must not push the marker off the row, because a clipped
/// marker is a silent one.
fn channel_item(number: usize, name: &str, active: bool, width: usize) -> Line<'static> {
    let marker = if active { " …" } else { "" };
    let prefix = format!("{number} ");
    let budget = width.saturating_sub(prefix.chars().count() + marker.chars().count());
    let name: String = name.chars().take(budget).collect();
    Line::raw(format!("{prefix}{name}{marker}"))
}

/// The columns a list row has: the area minus its borders and the two the
/// highlight symbol takes.
fn list_row_width(area: Rect) -> usize {
    area.width.saturating_sub(4) as usize
}

fn draw_channel_list(frame: &mut Frame, app: &App, now: u64, area: Rect) {
    let width = list_row_width(area);
    let items: Vec<ListItem> = app
        .channels
        .iter()
        .enumerate()
        .map(|(index, channel)| {
            let number = (index + 1).min(9);
            ListItem::new(channel_item(
                number,
                &channel.name,
                app.is_typing(channel.id, now),
                width,
            ))
        })
        .collect();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("channels"))
        .highlight_style(highlight_style())
        .highlight_symbol("> ");
    let mut state = ListState::default();
    if !app.channels.is_empty() {
        state.select(Some(app.selected));
    }
    frame.render_stateful_widget(list, area, &mut state);
}

/// The timeline, drawn as a line window instead of a widget list. A widget
/// list drops any item taller than its area, and in a chat one long message
/// is exactly that; this keeps the focused row's own lines on screen.
fn draw_timeline(frame: &mut Frame, app: &App, area: Rect, now: u64, compact: bool) {
    if area.is_empty() {
        return;
    }
    let body_width = area.width.saturating_sub(2);
    let rows = app
        .channels
        .get(app.selected)
        .map(|e| e.rows.as_slice())
        .unwrap_or(&[]);
    if rows.is_empty() {
        return;
    }

    // Flatten the rows, and remember where each row starts, so the window can
    // be placed in lines rather than in whole rows.
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut starts: Vec<usize> = Vec::with_capacity(rows.len() + 1);
    for row in rows {
        starts.push(lines.len());
        lines.extend(row_lines(row, now, body_width, compact));
    }
    starts.push(lines.len());

    let focus = app.focus.min(rows.len() - 1);
    let viewport = area.height as usize;
    let height = starts[focus + 1] - starts[focus];
    let offset = if height > viewport {
        // Taller than the view: show the row from its head, where its author
        // and age are.
        starts[focus]
    } else {
        // Otherwise show its last line and fill upward.
        starts[focus + 1].saturating_sub(viewport)
    };

    let focused = highlight_style();
    let visible: Vec<Line<'static>> = lines
        .into_iter()
        .skip(offset)
        .take(viewport)
        .enumerate()
        .map(|(index, line)| {
            let at = offset + index;
            let row = match starts.binary_search(&at) {
                Ok(index) => index,
                Err(index) => index.saturating_sub(1),
            };
            let symbol = if row == focus && at == starts[focus] {
                "> "
            } else {
                "  "
            };
            let mut spans = vec![Span::raw(symbol)];
            spans.extend(line.spans);
            let line = Line::from(spans);
            if row == focus {
                line.style(focused)
            } else {
                line
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(visible), area);
}

fn composer_hint(app: &App, compact: bool) -> String {
    if app.composer.edit.is_some() {
        if compact {
            "edit · enter save · esc cancel".to_owned()
        } else {
            "editing your message - enter to save, esc to cancel".to_owned()
        }
    } else if let Some(reply) = &app.composer.reply {
        if compact {
            format!("reply {} · enter send", reply.author)
        } else {
            format!("reply to {} - enter to send, esc to clear", reply.author)
        }
    } else if app.mode == Mode::Composer {
        if compact {
            "new message · enter send".to_owned()
        } else {
            "new message - enter to send, alt+enter newline".to_owned()
        }
    } else if compact {
        "i compose · enter reply".to_owned()
    } else {
        "i compose - enter reply".to_owned()
    }
}

fn draw_composer(frame: &mut Frame, app: &App, area: Rect, compact: bool) {
    let hint = composer_hint(app, compact);
    let block = Block::default().borders(Borders::ALL).title(hint);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if compact {
        // The narrow composer box has one inner row: follow the cursor
        // instead of scrolling whole lines off the top.
        let (line, col) = app.composer.cursor;
        let text = app.composer.lines.get(line).cloned().unwrap_or_default();
        frame.render_widget(
            Paragraph::new(cursor_window(&text, col, inner.width as usize)),
            inner,
        );
        return;
    }
    let visible = inner.height as usize;
    let lines: Vec<Line> = app
        .composer
        .lines
        .iter()
        .map(|l| Line::raw(l.clone()))
        .collect();
    let skip = app
        .composer
        .cursor
        .0
        .saturating_sub(visible.saturating_sub(1));
    let shown: Vec<Line> = lines.into_iter().skip(skip).collect();
    frame.render_widget(Paragraph::new(shown), inner);
}

/// The minimal composer: one line, with the target it is aimed at.
fn draw_composer_line(frame: &mut Frame, app: &App, area: Rect) {
    let prompt = if app.composer.edit.is_some() {
        "e> "
    } else if app.composer.reply.is_some() {
        "r> "
    } else {
        "> "
    };
    let (line, col) = app.composer.cursor;
    let text = app.composer.lines.get(line).cloned().unwrap_or_default();
    let width = area.width.saturating_sub(prompt.len() as u16) as usize;
    frame.render_widget(
        Paragraph::new(format!("{prompt}{}", cursor_window(&text, col, width))),
        area,
    );
}

fn draw_status(frame: &mut Frame, app: &App, area: Rect, wide: bool) {
    let status = if wide {
        let mode = match app.mode {
            Mode::Navigation => "nav",
            Mode::Composer => "composer",
        };
        format!(
            "status: {} {} | {} | mode: {mode} | ?=help",
            conn_word(app.conn),
            app.relay_label,
            app.status
        )
    } else {
        let mode = match app.mode {
            Mode::Navigation => "nav",
            Mode::Composer => "compose",
        };
        format!("{mode} · {}", app.status)
    };
    frame.render_widget(Paragraph::new(status), area);
}

fn draw_size_message(frame: &mut Frame, area: Rect) {
    let text = vec![
        Line::raw(format!(
            "needs {} cols x {} rows",
            layout::MIN_WIDTH,
            layout::MIN_HEIGHT
        )),
        Line::raw(format!("now {} cols x {} rows", area.width, area.height)),
        Line::raw("resize, or q to quit"),
    ];
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: true }), area);
}

fn draw_picker(frame: &mut Frame, app: &App, now: u64, area: Rect) {
    let width = list_row_width(area);
    let items: Vec<ListItem> = app
        .channels
        .iter()
        .enumerate()
        .map(|(index, channel)| {
            ListItem::new(channel_item(
                index + 1,
                &channel.name,
                app.is_typing(channel.id, now),
                width,
            ))
        })
        .collect();
    let block = Block::default()
        .borders(Borders::ALL)
        .title("channels")
        .title_bottom(Line::raw("enter select · esc close"));
    let list = List::new(items)
        .block(block)
        .highlight_style(highlight_style())
        .highlight_symbol("> ");
    let mut state = ListState::default();
    state.select(app.picker);
    frame.render_widget(Clear, area);
    frame.render_stateful_widget(list, area, &mut state);
}

/// The key help. The wide layout has the room for the detailed text; the
/// one-column layouts get the terse one, sized to what they can show.
fn help_text(mode: LayoutMode) -> &'static str {
    match mode {
        LayoutMode::Wide => WIDE_HELP,
        _ => COMPACT_HELP,
    }
}

const WIDE_HELP: &str = "\
navigation
  j k up down    switch channel    1-9 jump
  c              channel picker    Esc close
  g G PgUp PgDn  move the focused row
  i Tab          compose new       Enter  reply
  r e d          react / edit / delete
  ?              help              q quit
composer
  Enter send     Alt+Enter newline
  Esc leaves the composer, keeps the text";

const COMPACT_HELP: &str = "\
i compose  Enter send
j k move row  c picker
Enter reply  r react
e edit  d delete
1-9 jump  g G start/end
PgUp/PgDn move ten rows
? help  q quit
Esc close  Alt+Enter nl";

fn draw_help(frame: &mut Frame, area: Rect, mode: LayoutMode) {
    let text = help_text(mode);
    frame.render_widget(Clear, area);
    if mode != LayoutMode::Wide {
        // No border: at the smallest sizes every row carries a key.
        frame.render_widget(Paragraph::new(text), area);
        return;
    }
    let lines: Vec<&str> = text.lines().collect();
    let content = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0) as u16;
    let width = (content + 2).min(area.width);
    let height = (lines.len() as u16 + 2).min(area.height);
    let popup = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(text).block(Block::default().borders(Borders::ALL).title("keys")),
        popup,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_ages_render_coarsely() {
        assert_eq!(age(95, 100), "5s");
        assert_eq!(age(100, 160), "1m");
        assert_eq!(age(0, 7200), "2h");
        assert_eq!(age(0, 2 * 86_400), "2d");
    }

    #[test]
    fn a_future_timestamp_clamps_to_zero_instead_of_panicking() {
        assert_eq!(age(1_000, 100), "0s");
    }

    #[test]
    fn wrap_breaks_long_lines_and_keeps_short_ones() {
        assert_eq!(wrap("abc def", 10), vec!["abc def"]);
        assert_eq!(wrap("aaa bbb ccc", 7), vec!["aaa bbb", "ccc"]);
        assert_eq!(wrap("", 10), vec![""]);
    }

    #[test]
    fn a_word_longer_than_the_row_breaks_without_overflowing() {
        let url = "https://example.test/a/very/long/path/that/keeps/going";
        for chunk in wrap(url, 22) {
            assert!(chunk.chars().count() <= 22, "{chunk:?} is wider than 22");
        }
        assert_eq!(wrap(url, 22).concat(), url, "only the wrapping changed");
    }

    #[test]
    fn the_composer_window_keeps_the_cursor_on_screen() {
        assert_eq!(cursor_window("hello", 5, 10), "hello");
        // The cursor sits past the right edge: the window slides with it and
        // gives the last column to the cursor itself.
        assert_eq!(cursor_window("abcdefghij", 10, 4), "hij");
        assert_eq!(cursor_window("abcdefghij", 5, 4), "cdef");
        // A multi-byte draft keeps its characters intact.
        assert_eq!(cursor_window("aé日x", 7, 3), "日x");
    }

    fn chat_app(rows: Vec<crate::content::Row>) -> App {
        let mut app = App::new(&nostr::Keys::generate(), "http://relay.test");
        app.conn = ConnState::Connected;
        app.status = "connected".to_owned();
        app.channels.push(crate::app::ChannelEntry {
            id: uuid::Uuid::new_v4(),
            name: "general".to_owned(),
            rows,
            seen: Default::default(),
            loading: false,
        });
        app
    }

    fn message_row(n: usize, body: &str) -> crate::content::Row {
        crate::content::Row {
            event_id: format!("{n:064x}"),
            pubkey: "p".into(),
            author: "alice".into(),
            created_at: 100,
            body: body.to_owned(),
            kind: 9,
            root_id: None,
            parent_id: None,
            broadcast: false,
            mentions_me: false,
            reactions: Vec::new(),
            attachment: None,
            pending: false,
            uncertain: false,
            edited: false,
        }
    }

    fn frame_text(app: &App, width: u16, height: u16) -> String {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test backend");
        terminal
            .draw(|frame| draw(frame, app, 130))
            .expect("test frame");
        let buffer = terminal.backend().buffer().clone();
        let mut text = String::new();
        for y in 0..height {
            for x in 0..width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }

    #[test]
    fn a_long_channel_name_gives_up_columns_instead_of_the_activity_marker() {
        // 18 columns is the sidebar's row width at 80x12.
        let plain = channel_item(5, "buzzx-tui-build", false, 18).to_string();
        assert_eq!(plain, "5 buzzx-tui-build");
        let active = channel_item(5, "buzzx-tui-build", true, 18).to_string();
        assert!(active.ends_with('…'), "{active:?} keeps the marker");
        assert_eq!(active.chars().count(), 18, "{active:?} fills the row");
        let short = channel_item(1, "dm", true, 18).to_string();
        assert_eq!(short, "1 dm …");
    }

    #[test]
    fn the_typing_line_merges_names_and_counts_the_rest() {
        let names = |list: &[&str]| -> Vec<String> { list.iter().map(|n| n.to_string()).collect() };
        assert_eq!(typing_line(&names(&[]), false), "");
        assert_eq!(typing_line(&names(&["Ada"]), false), "Ada typing…");
        assert_eq!(typing_line(&names(&["Ada"]), true), "@Ada typing…");
        assert_eq!(
            typing_line(&names(&["Ada", "Bo"]), true),
            "@Ada, @Bo are typing…"
        );
        assert_eq!(
            typing_line(&names(&["Ada", "Bo", "Cy", "Di"]), true),
            "@Ada, @Bo +2 are typing…"
        );
        assert_eq!(
            typing_line(&names(&["Ada", "Bo", "Cy"]), false),
            "Ada, Bo +1 are typing…"
        );
    }

    #[test]
    fn the_typing_line_sits_above_the_composer_and_the_list_marks_activity() {
        let mut app = chat_app(vec![message_row(0, "hello")]);
        let active = app.channels[0].id;
        app.channels.push(crate::app::ChannelEntry {
            id: uuid::Uuid::new_v4(),
            name: "quiet".to_owned(),
            rows: Vec::new(),
            seen: Default::default(),
            loading: false,
        });
        app.apply(
            crate::session::ChatEvent::Profiles(vec![("agent-a".into(), "Agent A".into())]),
            130,
        );
        app.apply(
            crate::session::ChatEvent::Typing {
                channel: active,
                pubkey: "agent-a".into(),
            },
            130,
        );

        let text = frame_text(&app, 80, 12);
        let typing_row = text
            .lines()
            .find(|line| line.contains("typing"))
            .expect("the typing line renders");
        assert!(
            typing_row.contains("Agent A typing"),
            "the wide layout spells the display name out: {typing_row:?}"
        );
        assert!(text.contains("i compose"), "the composer keeps its hint");
        assert!(text.contains("1 general …"), "{text}");
        assert!(!text.contains("2 quiet …"), "a quiet channel stays plain");
    }

    #[test]
    fn one_column_layouts_show_typing_without_losing_the_composer() {
        let mut app = chat_app(vec![message_row(0, "hello world")]);
        let id = app.channels[0].id;
        app.apply(
            crate::session::ChatEvent::Profiles(vec![("agent-a".into(), "agent-a".into())]),
            130,
        );
        app.apply(
            crate::session::ChatEvent::Typing {
                channel: id,
                pubkey: "agent-a".into(),
            },
            130,
        );
        app.mode = Mode::Composer;
        app.composer.set_text("hi");

        let narrow = frame_text(&app, 40, 10);
        assert!(narrow.contains("@agent-a typing…"), "{narrow}");
        assert!(narrow.contains("enter send"), "the composer keeps its hint");

        // 24x6 is the floor: header, one timeline row, typing, composer, and
        // status must all fit at once.
        let minimal = frame_text(&app, 24, 6);
        assert!(minimal.contains("@agent-a typing…"), "{minimal}");
        assert!(
            minimal.contains("> hi"),
            "the composer prompt survives: {minimal}"
        );
        assert!(
            minimal.contains("hello world"),
            "the focused row survives: {minimal}"
        );
    }

    #[test]
    fn the_picker_marks_the_channels_someone_is_composing_in() {
        let mut app = chat_app(vec![message_row(0, "hello")]);
        let second = uuid::Uuid::new_v4();
        app.channels.push(crate::app::ChannelEntry {
            id: second,
            name: "second".to_owned(),
            rows: Vec::new(),
            seen: Default::default(),
            loading: false,
        });
        app.apply(
            crate::session::ChatEvent::Typing {
                channel: second,
                pubkey: "agent-b".into(),
            },
            130,
        );
        app.picker = Some(0);

        let text = frame_text(&app, 40, 10);
        assert!(text.contains("2 second …"), "{text}");
        assert!(!text.contains("1 general …"), "{text}");
    }

    #[test]
    fn a_row_taller_than_the_timeline_still_shows_its_head() {
        // A widget list drops an item taller than its area; the timeline
        // window must not, or one long message blanks the screen.
        let body = "the migration is on main so the schema is already applied everywhere";
        let app = chat_app(vec![message_row(0, body)]);
        let text = frame_text(&app, 24, 6);
        assert!(text.contains("> alice"), "focus marker on the author line");
        assert!(
            text.contains("the migration"),
            "the head of the body:\n{text}"
        );
    }

    #[test]
    fn the_window_follows_the_focused_row() {
        let rows = (0..12)
            .map(|n| message_row(n, &format!("message {n} body")))
            .collect();
        let mut app = chat_app(rows);
        app.focus = 0;
        let oldest = frame_text(&app, 40, 10);
        assert!(oldest.contains("message 0 body"), "focused row visible");
        assert!(!oldest.contains("message 11"), "rows below stay out");
        app.focus = 11;
        let newest = frame_text(&app, 40, 10);
        assert!(newest.contains("message 11 body"));
        assert!(!newest.contains("message 0 body"));
    }

    #[test]
    fn a_row_that_mentions_me_keeps_its_age_and_markers() {
        let mut row = message_row(0, "hi");
        row.mentions_me = true;
        row.parent_id = Some("root".into());
        let header = row_lines(&row, 110, 40, true)[0].to_string();
        assert!(header.starts_with("*alice 10s"), "{header}");
        assert!(header.contains(" <"), "{header}");
        assert!(header.contains("@you"), "{header}");
    }

    #[test]
    fn compact_rows_shorten_the_markers_they_add() {
        let mut row = crate::content::Row {
            event_id: "e".into(),
            pubkey: "p".into(),
            author: "alice".into(),
            created_at: 0,
            body: "hi".into(),
            kind: 9,
            root_id: Some("root".into()),
            parent_id: Some("root".into()),
            broadcast: true,
            mentions_me: false,
            reactions: vec![("+".into(), 2)],
            attachment: None,
            pending: false,
            uncertain: false,
            edited: true,
        };
        let render = |row: &crate::content::Row, compact| -> Vec<String> {
            row_lines(row, 0, 40, compact)
                .iter()
                .map(|line| line.to_string())
                .collect()
        };
        let wide = render(&row, false);
        let compact = render(&row, true);
        assert!(wide[0].contains("(reply)") && wide[0].contains("@channel"));
        assert!(!compact[0].contains("(reply)") && !compact[0].contains("@channel"));
        assert!(compact[0].contains('@') || compact[0].contains('<'));
        assert_eq!(wide.last().unwrap(), "+ x2");
        assert_eq!(compact.last().unwrap(), "+2");
        row.parent_id = None;
        row.broadcast = false;
        row.edited = false;
        row.reactions.clear();
        let plain = render(&row, true);
        assert_eq!(plain.len(), 2, "a plain row is the header and the body");
        assert!(plain[0].starts_with("alice 0s"), "author and age stay");
    }
}
