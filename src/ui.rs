//! Rendering only. `ui.rs` reads `App` state and draws; it never mutates it
//! and never touches the network. The layout mode follows the frame size, and
//! one-column modes reuse the same state as the wide one.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};

use crate::agents;
use crate::app::{AgentStatus, App, ConnState, Context, Marker, Mode, Sections};
use crate::content::short_pubkey;
use crate::layout::{self, LayoutMode};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use uuid::Uuid;

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

/// What a row's place in the view is, when it is the head of a thread.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RootMark {
    /// Not a thread root: the channel timeline's ordinary case.
    None,
    /// The thread's head: it carries the `[root]` label.
    Root,
    /// The thread's head after a deletion: the placeholder replaces its body.
    Deleted,
}

/// The lines one row renders as: the header, the wrapped body, attachments,
/// and reactions. One-column modes ask for the compact markers, so a row
/// spends its width on the message instead of on words like `(reply)`.
fn row_lines(
    row: &crate::content::Row,
    now: u64,
    width: u16,
    compact: bool,
    mark: RootMark,
) -> Vec<Line<'static>> {
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
    if mark != RootMark::None {
        // The root label belongs to the root row and nowhere else: it is not
        // a second pinned header.
        header.push(Span::styled("  [root]".to_owned(), pending_style()));
    }
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
    if mark == RootMark::Deleted {
        // The replies that named it stay readable; only its own content is
        // gone, and no root-targeted write is offered.
        lines.push(Line::styled("Root deleted".to_owned(), pending_style()));
    } else {
        for line in row.body.split('\n') {
            for chunk in wrap(line, width) {
                lines.push(Line::styled(chunk, body_style));
            }
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
        // The thread is a surface of its own: it replaces the whole column
        // rather than covering part of the timeline.
        _ if app.thread.open => draw_thread(frame, app, now, area, mode),
        LayoutMode::Wide => draw_wide(frame, app, now, area),
        LayoutMode::Narrow => draw_narrow(frame, app, now, area),
        LayoutMode::Minimal => draw_minimal(frame, app, now, area),
    }
    if app.agents.open {
        draw_agents(frame, app, now, area);
    }
    if app.picker.is_some() {
        draw_picker(frame, app, now, area);
    }
    if app.help {
        draw_help(frame, app, area, mode);
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

/// The focused thread: one full-screen timeline at every size, with the
/// conversation named in the header and the way back next to it.
///
/// Nothing channel-scoped is drawn here. Typing is per channel, so it cannot
/// say who is replying to this thread, and it is left out.
fn draw_thread(frame: &mut Frame, app: &App, now: u64, area: Rect, mode: LayoutMode) {
    let compact = mode != LayoutMode::Wide;
    let minimal = mode == LayoutMode::Minimal;
    let composing = app.mode == Mode::Composer;
    // The one-row input is the minimal shape; wider terminals keep the
    // existing multi-line composer while every other region still fits.
    let boxed = composing && !minimal;
    let column = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(u16::from(composing)),
        Constraint::Length(if boxed { 3 } else { u16::from(composing) }),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(area);
    draw_thread_header(frame, app, column[0], compact);
    draw_thread_timeline(frame, app, column[1], now, compact);
    if composing {
        draw_thread_target(frame, app, column[2]);
        if boxed {
            draw_composer(frame, app, column[3], compact);
        } else {
            draw_composer_line(frame, app, column[3]);
        }
    }
    draw_thread_keys(frame, app, column[4], compact);
    draw_thread_status(frame, app, column[5], compact);
}

/// `Thread / #channel` and the way back. The back hint is reserved before the
/// conversation name is shortened, so the exit never clips away.
fn draw_thread_header(frame: &mut Frame, app: &App, area: Rect, compact: bool) {
    let label = match app
        .channels
        .iter()
        .find(|entry| entry.id == app.thread.channel)
    {
        Some(entry) if entry.is_dm() => app.label(entry),
        Some(entry) => format!("#{}", app.label(entry)),
        None => app.thread.channel.to_string(),
    };
    let head = if compact {
        format!("Thread {label}")
    } else {
        format!("Thread / {label}")
    };
    let back = if compact { "Esc:back" } else { "Esc: back" };
    let room = (area.width as usize).saturating_sub(back.len() + 1);
    let head = clip_with_ellipsis(&head, room);
    let pad = room.saturating_sub(head.width()) + 1;
    frame.render_widget(
        Paragraph::new(format!("{head}{}{back}", " ".repeat(pad))),
        area,
    );
}

/// The thread's own rows: the root first, then the loaded replies in
/// chronological order. The root scrolls like every other row.
fn draw_thread_timeline(frame: &mut Frame, app: &App, area: Rect, now: u64, compact: bool) {
    if app.thread.rows.is_empty() {
        // Nothing is claimed about an unloaded thread: an empty-thread claim
        // would be a false result while the read is still out.
        return;
    }
    // The head label belongs to the root row itself: a live reply that
    // arrived before the root did is not the head of this thread.
    let is_root =
        app.thread.rows.first().map(|row| row.event_id.as_str()) == Some(app.thread.root.as_str());
    let head = match (is_root, app.thread.root_deleted) {
        (false, _) => RootMark::None,
        (true, true) => RootMark::Deleted,
        (true, false) => RootMark::Root,
    };
    draw_rows(
        frame,
        &app.thread.rows,
        app.thread.focus,
        area,
        now,
        compact,
        head,
    );
}

/// What the composer is aimed at, stated rather than inferred from the
/// focused row.
fn draw_thread_target(frame: &mut Frame, app: &App, area: Rect) {
    if app.composer.edit.is_some() {
        frame.render_widget(Paragraph::new("Edit own message"), area);
        return;
    }
    if let Some(reply) = &app.composer.reply {
        frame.render_widget(Paragraph::new(format!("Reply to {}", reply.author)), area);
        return;
    }
    frame.render_widget(Paragraph::new("New message"), area);
}

fn draw_thread_keys(frame: &mut Frame, app: &App, area: Rect, compact: bool) {
    let keys = match (app.mode == Mode::Composer, compact) {
        (false, false) => "j/k: move  Enter: reply  i: root  ?: help",
        (false, true) => "Enter:reply i:root ?help",
        // Leaving a thread composer is one press, unlike the channel's, and
        // the hint says so.
        (true, false) => "Enter: send  Esc: cancel compose",
        (true, true) => "Enter:send Esc:nav",
    };
    frame.render_widget(Paragraph::new(keys), area);
}

/// The thread's status. One transient message at a time, in the order the
/// reader needs it: a write outcome, then the read state, then coverage.
fn draw_thread_status(frame: &mut Frame, app: &App, area: Rect, compact: bool) {
    frame.render_widget(Paragraph::new(thread_status(app, compact)), area);
}

fn thread_status(app: &App, compact: bool) -> String {
    if let Some(notice) = &app.thread.notice {
        return notice.clone();
    }
    if app.thread.send_pending() {
        return "Sending...".to_owned();
    }
    if app.thread.loading && app.thread.rows.is_empty() {
        return "Loading thread...".to_owned();
    }
    if app.thread.root_deleted {
        return if compact {
            "Root deleted".to_owned()
        } else {
            "Root deleted; replies remain".to_owned()
        };
    }
    if app.thread.failed.is_some() {
        return if compact {
            "Load failed; t retry".to_owned()
        } else {
            "Thread load failed; t retry, Esc back".to_owned()
        };
    }
    if app.thread.stale() || app.conn != ConnState::Connected {
        return if compact {
            "reconnecting; stale".to_owned()
        } else {
            format!("{}; stale", conn_word(app.conn))
        };
    }
    if app.thread.partial {
        return if compact {
            "Partial; limit reached".to_owned()
        } else {
            "Partial thread: reply limit reached".to_owned()
        };
    }
    if app.thread.loaded() && app.thread.rows.len() <= 1 {
        return "No replies yet".to_owned();
    }
    conn_word(app.conn).to_owned()
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
    let loading = entry.map(|c| c.loading).unwrap_or(false);
    let channel = match entry {
        // A DM is not a channel; the octothorpe would claim it is one, and the
        // people in it are the name.
        Some(entry) => {
            let label = app.label(entry);
            match (entry.is_dm(), loading) {
                (true, false) => label,
                (true, true) => format!("{label} loading"),
                (false, false) => format!("#{label}"),
                (false, true) => format!("#{label} loading"),
            }
        }
        None => "no conversation".to_owned(),
    };
    let text = if with_conn {
        format!("{channel} · {}", conn_word(app.conn))
    } else {
        channel
    };
    frame.render_widget(Paragraph::new(text), area);
}

/// One conversation list line: `number signal name`, plus the typing marker.
/// Every signal column is reserved before the name is clipped, because a
/// clipped marker is a silent one: a long name gives up columns instead. The
/// unread count lives inside that column for the same reason.
fn conversation_item(
    number: Option<usize>,
    signal: Marker,
    count: usize,
    name: &str,
    typing: bool,
    width: usize,
) -> Line<'static> {
    let column = match number {
        Some(number) => format!("{number} "),
        None => "  ".to_owned(),
    };
    let symbol = match signal {
        Marker::None => "",
        other => other.text(),
    };
    let cell = if count > 0 {
        format!("{symbol} {count}")
    } else {
        symbol.to_owned()
    };
    let signal = format!("{cell:<SIGNAL_COLUMNS$}");
    let typing = if typing { " …" } else { "" };
    let budget = width
        .saturating_sub(column.chars().count() + signal.chars().count() + typing.chars().count());
    let name: String = name.chars().take(budget).collect();
    Line::raw(format!("{column}{signal}{name}{typing}"))
}

/// The columns the signal cell owns: one symbol, up to two digits of count,
/// and the space that separates it from the name. Fixed so names line up
/// whatever the marker is.
const SIGNAL_COLUMNS: usize = 5;

/// One row of a conversation list: a section heading, a conversation, or the
/// one line an empty view shows instead.
enum ListRow {
    Heading(&'static str),
    Conversation(Uuid),
    Empty(&'static str),
    Note(String),
}

/// The rows a list shows. Sections are in a fixed order, and a view with
/// nothing in it says what its emptiness means instead of showing headings.
fn list_rows(app: &App, view: &Sections) -> Vec<ListRow> {
    let mut rows = Vec::new();
    if view.channels.is_empty() && view.dms.is_empty() {
        rows.push(ListRow::Empty(app.empty_view()));
    } else {
        for (heading, ids) in [("Channels", &view.channels), ("DMs", &view.dms)] {
            if ids.is_empty() {
                continue;
            }
            rows.push(ListRow::Heading(heading));
            rows.extend(ids.iter().copied().map(ListRow::Conversation));
        }
    }
    // What the Inbox cannot promise belongs under the list it applies to: a
    // read the relay has not been told about, a local baseline, or
    // conversations it could not list at all.
    if let Some(note) = app.inbox_footer() {
        rows.push(ListRow::Note(note));
    }
    rows
}

/// A note under a list, wrapped to the row it is shown in. It carries the
/// continuation indent so a wrapped note still reads as one sentence.
fn note_lines(text: &str, width: usize) -> Vec<Line<'static>> {
    let budget = width.saturating_sub(2).max(1);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current = String::new();
    for word in text.split(' ') {
        let mut piece = current.clone();
        if !piece.is_empty() {
            piece.push(' ');
        }
        piece.push_str(word);
        if piece.chars().count() > budget && !current.is_empty() {
            lines.push(Line::raw(std::mem::take(&mut current)));
            current = word.to_owned();
        } else {
            current = piece;
        }
    }
    if !current.is_empty() {
        lines.push(Line::raw(current));
    }
    if let Some(first) = lines.first_mut() {
        *first = Line::raw(format!("· {first}"));
    }
    lines
}

/// Where one conversation sits in a list's rows, so the highlight and the
/// picker's cursor land on the conversation and not on a heading.
fn item_index(rows: &[ListRow], conversation: Option<Uuid>) -> Option<usize> {
    let conversation = conversation?;
    rows.iter()
        .position(|row| matches!(row, ListRow::Conversation(id) if *id == conversation))
}

/// The list items a conversation list is made of.
fn list_items(app: &App, rows: &[ListRow], width: usize, now: u64) -> Vec<ListItem<'static>> {
    rows.iter()
        .map(|row| match row {
            ListRow::Heading(name) => ListItem::new(Line::raw(*name)),
            ListRow::Empty(text) => ListItem::new(Line::raw(*text)),
            ListRow::Note(text) => ListItem::new(note_lines(text, width)),
            ListRow::Conversation(id) => {
                let entry = app.entry(*id);
                match entry {
                    Some(entry) => ListItem::new(conversation_item(
                        app.shortcut(entry.id),
                        app.marker(entry),
                        app.unread_count(entry),
                        &app.label(entry),
                        app.is_typing(entry.id, now),
                        width,
                    )),
                    None => ListItem::new(Line::raw("")),
                }
            }
        })
        .collect()
}

/// The columns a list row has: the area minus its borders and the two the
/// highlight symbol takes.
fn list_row_width(area: Rect) -> usize {
    area.width.saturating_sub(4) as usize
}

fn draw_channel_list(frame: &mut Frame, app: &App, now: u64, area: Rect) {
    let width = list_row_width(area);
    let view = app.view().clone();
    let rows = list_rows(app, &view);
    let items = list_items(app, &rows, width, now);
    let selected = app.channels.get(app.selected).map(|entry| entry.id);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(inbox_title(app));
    let list = List::new(items)
        .block(block)
        .highlight_style(highlight_style())
        .highlight_symbol("> ");
    let mut state = ListState::default();
    state.select(item_index(&rows, selected));
    frame.render_stateful_widget(list, area, &mut state);
}

/// The list heading. It names the filter, and says with `?` when the list
/// cannot promise it holds every conversation.
fn inbox_title(app: &App) -> Line<'static> {
    let mark = if app.inbox_incomplete() { " ?" } else { "" };
    Line::raw(format!("Inbox: {}{mark}", app.filter.name()))
}

/// The timeline, drawn as a line window instead of a widget list. A widget
/// list drops any item taller than its area, and in a chat one long message
/// is exactly that; this keeps the focused row's own lines on screen.
fn draw_timeline(frame: &mut Frame, app: &App, area: Rect, now: u64, compact: bool) {
    let rows = app
        .channels
        .get(app.selected)
        .map(|e| e.rows.as_slice())
        .unwrap_or(&[]);
    draw_rows(frame, rows, app.focus, area, now, compact, RootMark::None);
}

/// One row list, focused row kept visible. The channel timeline and the
/// thread view use it: they differ in where the rows come from and in the
/// label the head of the list carries, not in how a row reads.
fn draw_rows(
    frame: &mut Frame,
    rows: &[crate::content::Row],
    focus: usize,
    area: Rect,
    now: u64,
    compact: bool,
    head: RootMark,
) {
    if area.is_empty() {
        return;
    }
    let body_width = area.width.saturating_sub(2);
    if rows.is_empty() {
        return;
    }

    // Flatten the rows, and remember where each row starts, so the window can
    // be placed in lines rather than in whole rows.
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut starts: Vec<usize> = Vec::with_capacity(rows.len() + 1);
    for (index, row) in rows.iter().enumerate() {
        starts.push(lines.len());
        let mark = if index == 0 { head } else { RootMark::None };
        lines.extend(row_lines(row, now, body_width, compact, mark));
    }
    starts.push(lines.len());

    let focus = focus.min(rows.len() - 1);
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
    let view = app.picker_view();
    let rows = list_rows(app, &view);
    let items = list_items(app, &rows, width, now);
    // The hint stays even when the list is empty: a filter with nothing in it
    // still has to be escapable and changeable.
    // The operation hint lives in the bottom border, at any size: a filter
    // with nothing in it still has to be escapable and changeable.
    // The hint names the three actions a filter with nothing in it still has
    // to offer. It gives up words, never an action: at 24 columns the inner
    // border is 22 cells, and a clipped hint reads as a missing key.
    // The width that matters is the border's inner cells: the hint is the
    // block's bottom title, so a hint longer than those cells is cut.
    let inner = area.width.saturating_sub(2);
    let hint = if inner >= 41 {
        "enter open · f filter · esc close · ? help"
    } else if inner >= 28 {
        "enter open f filter esc close"
    } else {
        "enter f filter esc"
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(inbox_title(app))
        .title_bottom(Line::raw(hint));
    let list = List::new(items)
        .block(block)
        .highlight_style(highlight_style())
        .highlight_symbol("> ");
    let mut state = ListState::default();
    state.select(item_index(&rows, app.picker.as_ref().map(|p| p.cursor)));
    frame.render_widget(Clear, area);
    frame.render_stateful_widget(list, area, &mut state);
}

/// The Agents overlay: the owned roster on the list level, one Agent's
/// observed work on the detail level. Both take the whole terminal at every
/// size: a squeezed second column would clip the status words, and those words
/// are the whole answer.
fn draw_agents(frame: &mut Frame, app: &App, now: u64, area: Rect) {
    frame.render_widget(Clear, area);
    if app.agents.cursor.level == agents::Level::Detail {
        draw_agent_detail(frame, app, now, area);
    } else {
        draw_agent_list(frame, app, now, area);
    }
}

/// How much of a row a name may take before the status it carries stops
/// fitting. The status words carry the meaning of the row, so they are the
/// part that is never clipped.
const AGENT_NAME_MIN: usize = 8;

fn draw_agent_list(frame: &mut Frame, app: &App, now: u64, area: Rect) {
    let width = list_row_width(area);
    let roster = match &app.agents.roster {
        agents::Load::Loaded(roster) => Some(roster),
        _ => None,
    };
    let mut items: Vec<ListItem<'static>> = Vec::new();
    if let Some(roster) = roster
        && !roster.agents.is_empty()
    {
        for agent in &roster.agents {
            let status = app.agent_status(&agent.pubkey, now);
            items.push(ListItem::new(Text::from(agent_row(
                &agent.name,
                &status,
                width,
            ))));
        }
    }
    // A roster that could not be read, and one that could but cannot be
    // promised whole, say which of the two they are: the list shows its own
    // incompleteness rather than a shorter list.
    let states = roster_state_lines(&app.agents.roster);
    if !states.is_empty() {
        items.push(ListItem::new(Text::from(
            states
                .into_iter()
                .map(|line| Line::styled(line, pending_style()))
                .collect::<Vec<Line<'static>>>(),
        )));
    }
    let count = app.agent_count();
    let title = if roster.is_some() {
        format!("Agents ({count})")
    } else {
        "Agents".to_owned()
    };
    // The selected Agent is the one a duplicate name can be told apart from:
    // the list shows its short key while its row is the selected one.
    let selected = app
        .selected_agent()
        .map(|agent| format!("{} {}", agent.name, short_pubkey(&agent.pubkey)))
        .filter(|label| (label.chars().count() + title.chars().count() + 4) as u16 <= area.width);
    let mut block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_bottom(Line::raw(agents_hint(area.width, false)));
    if let Some(label) = selected {
        block = block.title_top(Line::raw(label).alignment(Alignment::Right));
    }
    let list = List::new(items)
        .block(block)
        .highlight_style(highlight_style())
        .highlight_symbol("> ");
    let mut state = ListState::default();
    state.select((count > 0).then_some(app.agents.cursor.cursor));
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_agent_detail(frame: &mut Frame, app: &App, now: u64, area: Rect) {
    let Some(agent) = app.selected_agent() else {
        // The roster moved under the cursor: the list level says why.
        draw_agent_list(frame, app, now, area);
        return;
    };
    let width = area.width.saturating_sub(2) as usize;
    let status = app.agent_status(&agent.pubkey, now);
    let mut body: Vec<Line<'static>> = Vec::new();
    body.push(Line::from(vec![
        Span::styled("name         ", pending_style()),
        Span::raw(clip(&agent.name, width.saturating_sub(13))),
    ]));
    body.push(Line::from(vec![
        Span::styled("key          ", pending_style()),
        Span::raw(clip(&short_pubkey(&agent.pubkey), width.saturating_sub(13))),
    ]));
    body.push(Line::from(vec![
        Span::styled("status       ", pending_style()),
        Span::styled(
            clip(&agent_status_word(&status), width.saturating_sub(13)),
            agent_status_style(&status),
        ),
    ]));
    if let Some(at) = app.agent_last_signal(&agent.pubkey) {
        // The age of the evidence, so the claim can be inspected instead of
        // believed.
        body.push(Line::from(vec![
            Span::styled("last signal  ", pending_style()),
            Span::raw(format!("{} ago", age(at, now))),
        ]));
    }
    if let Some(at) = app.agent_last_failure(&agent.pubkey) {
        body.push(Line::styled(
            format!("Last observed turn failed {} ago", age(at, now)),
            pending_style(),
        ));
    }
    let contexts = app.agent_contexts(&agent.pubkey);
    let mut focus = None;
    match &status {
        AgentStatus::Working(_) => {
            body.push(Line::raw(""));
            body.push(Line::raw("contexts"));
            for (at, context) in contexts.iter().enumerate() {
                let (text, actionable) = context_row(context);
                let selected = at == app.agents.cursor.context;
                if selected {
                    focus = Some(body.len());
                }
                let line = match (selected, actionable) {
                    (true, _) => Line::styled(format!("> {text}"), highlight_style()),
                    (false, true) => Line::raw(format!("  {text}")),
                    // Work outside the user's channels is still evidence of
                    // work; it is just not a destination.
                    (false, false) => Line::styled(format!("  {text}"), pending_style()),
                };
                body.push(line);
            }
        }
        AgentStatus::Typing(channel) => {
            let name = app
                .entry(*channel)
                .map(|entry| app.label(entry))
                .unwrap_or_else(|| short_pubkey(&channel.to_string()));
            body.push(Line::from(vec![
                Span::styled("typing in    ", pending_style()),
                Span::raw(clip(&name, width.saturating_sub(13))),
            ]));
        }
        AgentStatus::NoTurn => body.push(Line::styled(
            "no turn is observed in this feed; background work is not ruled out",
            pending_style(),
        )),
        AgentStatus::Unknown => body.push(Line::styled(
            "no live observation: a quiet Agent is unknown here, not idle",
            pending_style(),
        )),
    }
    let actionable = matches!(app.agents.cursor.level, agents::Level::Detail)
        && matches!(
            contexts.get(app.agents.cursor.context),
            Some(Context::Channel { .. })
        )
        && matches!(status, AgentStatus::Working(_));
    let block = Block::default()
        .borders(Borders::ALL)
        .title(clip(&format!("Agent {}", agent.name), width))
        .title_bottom(Line::raw(agents_hint(area.width, actionable)));
    let rows = area.height.saturating_sub(2) as usize;
    let window = scroll_window(body.len(), rows, focus.unwrap_or(0));
    let visible: Vec<Line<'static>> = body
        .into_iter()
        .take(window.end)
        .skip(window.start)
        .collect();
    frame.render_widget(
        Paragraph::new(visible)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// One observed context: the text the detail shows, and whether opening it is
/// something the user may do.
fn context_row(context: &Context) -> (String, bool) {
    match context {
        Context::Channel { name, .. } => (name.clone(), true),
        // The name, the id and the content of a conversation the user is not
        // in stay hidden; the fact of the work does not.
        Context::Unavailable => ("Unavailable context".to_owned(), false),
        Context::Unknown => ("Location unknown".to_owned(), false),
    }
}

/// The status of one Agent in the words the contract names the states with.
fn agent_status_word(status: &AgentStatus) -> String {
    match status {
        AgentStatus::Working(1) => "Working".to_owned(),
        AgentStatus::Working(turns) => format!("Working ({turns})"),
        AgentStatus::Typing(_) => "Typing".to_owned(),
        AgentStatus::NoTurn => "No active turn observed".to_owned(),
        AgentStatus::Unknown => "Unknown".to_owned(),
    }
}

/// Only the strongest evidence is highlighted: a working turn is a claim about
/// execution, typing is a weaker one, and the two quiet states are not claims
/// at all.
fn agent_status_style(status: &AgentStatus) -> Style {
    match status {
        AgentStatus::Working(_) => author_style(),
        AgentStatus::Typing(_) => mention_style(),
        AgentStatus::NoTurn | AgentStatus::Unknown => pending_style(),
    }
}

/// One row of the Agent list: the name, and the status it carries. The status
/// is placed first and the name is clipped around it, because a clipped name
/// is still a name while a clipped status is a different state.
fn agent_row(name: &str, status: &AgentStatus, width: usize) -> Vec<Line<'static>> {
    let text = agent_status_word(status);
    let style = agent_status_style(status);
    let budget = width.saturating_sub(text.chars().count() + 1);
    if budget >= AGENT_NAME_MIN {
        let name = clip(name, budget);
        let pad = budget - name.chars().count() + 1;
        return vec![Line::from(vec![
            Span::raw(format!("{name}{}", " ".repeat(pad))),
            Span::styled(text, style),
        ])];
    }
    vec![
        Line::raw(clip(name, width)),
        Line::from(vec![Span::styled(format!("  {text}"), style)]),
    ]
}

/// What the Agents list says about the roster it does not show as rows. The
/// contract's words, one state per sentence: an empty answer, a refusal, a
/// failed read and an answer that is not the whole roster are not the same
/// result.
fn roster_state_lines(load: &agents::Load) -> Vec<String> {
    let roster = match load {
        agents::Load::Unknown => return vec!["Loading agents".to_owned()],
        agents::Load::Unavailable(reason) => {
            return vec![
                "Agent overview unavailable for this identity".to_owned(),
                reason.clone(),
            ];
        }
        agents::Load::Failed(reason) => {
            return vec!["Cannot load agents".to_owned(), reason.clone()];
        }
        agents::Load::Loaded(roster) => roster,
    };
    let mut lines = Vec::new();
    if roster.agents.is_empty() {
        lines.push("No agents".to_owned());
    }
    if !roster.incomplete() {
        return lines;
    }
    if roster.unverified > 0 {
        lines.push(format!(
            "Ownership unverified: {} record(s) naming you did not verify",
            roster.unverified
        ));
    }
    if roster.foreign > 0 {
        lines.push(format!(
            "List incomplete: {} record(s) signed by someone else",
            roster.foreign
        ));
    }
    if roster.unreadable > 0 {
        lines.push(format!(
            "List incomplete: {} record(s) could not be read",
            roster.unreadable
        ));
    }
    if roster.truncated {
        lines.push("List incomplete: the read stopped at its page limit".to_owned());
    }
    lines
}

/// The key hint in the overlay's bottom border. It gives up words before
/// actions: a clipped hint reads as a missing key.
fn agents_hint(width: u16, open_channel: bool) -> String {
    let inner = width.saturating_sub(2);
    if open_channel {
        if inner >= 42 {
            "enter open channel · j k select · esc back".to_owned()
        } else if inner >= 24 {
            "enter open · esc back".to_owned()
        } else {
            "enter · esc".to_owned()
        }
    } else if inner >= 34 {
        "j k select · enter detail · esc back".to_owned()
    } else if inner >= 18 {
        "enter · esc back".to_owned()
    } else {
        "esc back".to_owned()
    }
}

/// Cut a label to the cells a row has for it.
fn clip(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// Clip a label to `max` cells, saying that it was clipped. A shortened name
/// without a mark reads as the whole name.
fn clip_with_ellipsis(text: &str, max: usize) -> String {
    if text.width() <= max {
        return text.to_owned();
    }
    if max <= 3 {
        let mut out = String::new();
        let mut width = 0;
        for ch in text.chars() {
            let next = ch.width().unwrap_or(0);
            if width + next > max {
                break;
            }
            out.push(ch);
            width += next;
        }
        return out;
    }
    let limit = max - 3;
    let mut out = String::new();
    let mut width = 0;
    for ch in text.chars() {
        let next = ch.width().unwrap_or(0);
        if width + next > limit {
            break;
        }
        out.push(ch);
        width += next;
    }
    out.push_str("...");
    out
}

/// The lines of a body that a box of `rows` shows, centered on the line that
/// must stay visible.
fn scroll_window(len: usize, rows: usize, focus: usize) -> std::ops::Range<usize> {
    let rows = rows.min(len);
    if rows == 0 {
        return 0..0;
    }
    let start = focus.saturating_sub(rows / 2).min(len - rows);
    start..start + rows
}

/// The key help. The wide layout has the room for the detailed text; the
/// one-column layouts get the terse one, sized to what they can show. The
/// selected conversation's full label is part of it: a row clipped to the
/// terminal width is not the whole name, and the help text may wrap and
/// scroll to show it.
fn help_text(app: &App, mode: LayoutMode) -> String {
    let mut text = match (app.thread.open, mode) {
        (true, LayoutMode::Wide) => THREAD_WIDE_HELP.to_owned(),
        (true, _) => THREAD_COMPACT_HELP.to_owned(),
        (false, LayoutMode::Wide) => WIDE_HELP.to_owned(),
        (false, _) => COMPACT_HELP.to_owned(),
    };
    if app.thread.open {
        text.push_str(&format!("\nthread / {}\n", app.thread.root));
        if let Some(reason) = &app.thread.failed {
            text.push_str(&format!("  last read failed: {reason}\n"));
        }
        if app.thread.partial {
            text.push_str(
                "  the reply query reached its limit: this view is not\n  complete history.\n",
            );
        }
        if let Some(notice) = &app.thread.notice {
            text.push_str(&format!("  last outcome: {notice}\n"));
        }
    }
    if !app.thread.open {
        text.push_str(&format!(
            "\nfilter: {} (f cycles All, Unread, For you)\n",
            app.filter.name()
        ));
        if let Some(entry) = app.focused_entry() {
            text.push_str(&format!("selected: {}\n", app.label(entry)));
        }
    }
    if let Some(block) = &app.mention_block {
        // The last send never reached the relay: the full references stay
        // here, where the text scrolls, until the draft is sent again.
        text.push_str("\nthe last send was blocked by the mention check\n");
        text.push_str(&format!("  {}\n", block.summary));
        for detail in &block.details {
            text.push_str(&format!("  {detail}\n"));
        }
    }
    text.push_str(
        "read state is shared with other terminals through the relay.\n\
         if a publish fails this terminal keeps its place and shows\n\
         `Read here; not synced`; a restart may then show those\n\
         messages unread again.\n\
         a marker has second resolution, so a message delivered late\n\
         inside the frontier's second stays unread until it is shown.\n",
    );
    text
}

const WIDE_HELP: &str = "\
navigation
  j k up down    switch conversation   1-9 jump
  c              conversation picker   Esc close
  a              Agents: owned roster, working now
  f              Inbox filter
  g G PgUp PgDn  move the focused row
  i              compose new           Enter  reply
  r e d          react / edit / delete
  ?              help                  q quit
composer
  Enter send     Alt+Enter newline
  Esc leaves the composer, keeps the text
picker
  j k move  f Tab filter  enter open  esc close
agents
  j k select  enter detail  enter open channel
  esc back    ? help        a close
signals
  ● unread   @ unread mention   ? unknown   … typing";

const COMPACT_HELP: &str = "\
i compose  Enter send
j k move row  c picker
a agents  f filter
Enter reply  r react
e edit  d delete
1-9 jump  g G start/end
PgUp/PgDn move ten rows
? help  j k scroll  q quit
Esc close  Alt+Enter nl
● unread  @ mention  ? unknown";

const THREAD_WIDE_HELP: &str = "\
thread
  j k up down    move focused row    g G PgUp PgDn ends
  Enter          reply to focused row    i Tab reply to root
  t              retry failed read
  r e d          react / edit / delete on focused row
  Esc            back to the channel
  ?              help                  q quit
composer
  Enter send     Alt+Enter newline
  Esc leaves composing in one press and keeps its target";

const THREAD_COMPACT_HELP: &str = "\
thread: j k move  g G ends  PgUp/PgDn
Enter reply  i/Tab root  t retry read
r/e/d react/edit/delete  Esc back
? help  q quit
composer: Esc leaves; Enter sends";

/// Wrap the help text to a width, so the popup can be sized to what it holds
/// and scrolled by line rather than by paragraph.
fn help_lines(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    for line in text.lines() {
        if line.is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut current = String::new();
        for word in line.split(' ') {
            // A word wider than the terminal - a full `nostr:npub1…`
            // reference - breaks across lines rather than being clipped, so
            // every character of the help text stays reachable by scrolling.
            if word.chars().count() > width {
                if !current.is_empty() {
                    lines.push(std::mem::take(&mut current));
                }
                let mut chunk = String::new();
                for ch in word.chars() {
                    chunk.push(ch);
                    if chunk.chars().count() == width {
                        lines.push(std::mem::take(&mut chunk));
                    }
                }
                current = chunk;
                continue;
            }
            let mut piece = current.clone();
            if !piece.is_empty() {
                piece.push(' ');
            }
            piece.push_str(word);
            if piece.chars().count() > width && !current.is_empty() {
                lines.push(std::mem::take(&mut current));
                current = word.to_owned();
            } else {
                current = piece;
            }
        }
        lines.push(current);
    }
    lines
}

fn draw_help(frame: &mut Frame, app: &App, area: Rect, mode: LayoutMode) {
    frame.render_widget(Clear, area);
    if mode != LayoutMode::Wide {
        // No border: at the smallest sizes every row carries a key, and the
        // text scrolls by j and k.
        let rows = area.height as usize;
        let lines = help_lines(&help_text(app, mode), area.width as usize);
        let scroll = (app.help_scroll as usize).min(lines.len().saturating_sub(rows));
        let visible: Vec<Line<'static>> = lines
            .into_iter()
            .skip(scroll)
            .take(rows)
            .map(Line::raw)
            .collect();
        frame.render_widget(Paragraph::new(visible), area);
        return;
    }
    let lines = help_lines(&help_text(app, mode), area.width.saturating_sub(4) as usize);
    let content = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0) as u16;
    let width = (content + 2).min(area.width);
    let height = (lines.len() as u16 + 2).min(area.height);
    let popup = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    let rows = height.saturating_sub(2) as usize;
    let scroll = (app.help_scroll as usize).min(lines.len().saturating_sub(rows));
    let visible: Vec<Line<'static>> = lines.into_iter().skip(scroll).map(Line::raw).collect();
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(visible).block(Block::default().borders(Borders::ALL).title("keys")),
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
        let mut entry = App::stub_entry(uuid::Uuid::new_v4(), "general");
        entry.rows = rows;
        app.channels.push(entry);
        app.stub_roster();
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

    /// A conversation with a thread on screen: its root, one reply, and the
    /// read that delivered both.
    fn thread_app(own_root: bool) -> (App, nostr::Keys) {
        use nostr::{EventBuilder, Kind, Timestamp};
        let keys = nostr::Keys::generate();
        let mut app = App::new(&keys, "http://relay.test");
        app.conn = ConnState::Connected;
        app.status = "connected".to_owned();
        let entry = App::stub_entry(uuid::Uuid::new_v4(), "general");
        let id = entry.id;
        app.channels.push(entry);
        app.stub_roster();
        // The root is signed by the identity itself when the test needs to
        // delete it, which only its own author may do.
        let author = if own_root {
            keys.clone()
        } else {
            nostr::Keys::generate()
        };
        let root = EventBuilder::new(Kind::Custom(9), "the root")
            .tags(vec![nostr::Tag::parse(["h", &id.to_string()]).unwrap()])
            .custom_created_at(Timestamp::from(100))
            .sign_with_keys(&author)
            .unwrap();
        let root_id = root.id.to_hex();
        let reply = EventBuilder::new(Kind::Custom(9), "the reply")
            .tags(vec![
                nostr::Tag::parse(["h", &id.to_string()]).unwrap(),
                nostr::Tag::parse(["e", &root_id, "", "root"]).unwrap(),
                nostr::Tag::parse(["e", &root_id, "", "reply"]).unwrap(),
            ])
            .custom_created_at(Timestamp::from(110))
            .sign_with_keys(&nostr::Keys::generate())
            .unwrap();
        let me = app.me.clone();
        app.channels[0].rows = vec![
            crate::content::row_from_event(&root, &me),
            crate::content::row_from_event(&reply, &me),
        ];
        app.focus = 0;
        app.handle(crate::keys::Action::OpenThread, 130);
        app.apply(
            crate::session::ChatEvent::Thread {
                channel: id,
                root: root_id,
                request: 1,
                events: vec![root, reply],
                partial: false,
            },
            130,
        );
        app.take_outbox();
        (app, keys)
    }

    #[test]
    fn the_thread_frame_names_the_conversation_and_reserves_the_way_back() {
        let (app, _keys) = thread_app(false);
        let text = frame_text(&app, 80, 12);
        assert!(text.contains("Thread / #general"), "{text}");
        assert!(text.contains("Esc: back"), "{text}");
        assert!(text.contains("[root]"), "the root is labelled: {text}");
        assert!(text.contains("the root"), "{text}");
        assert!(text.contains("the reply"), "{text}");
        assert!(text.contains("j/k: move"), "the key row: {text}");
    }

    #[test]
    fn the_thread_frame_at_the_floor_keeps_every_region() {
        let (mut app, _keys) = thread_app(false);
        let reading = frame_text(&app, 24, 6);
        assert!(reading.contains("Thread #general"), "{reading}");
        assert!(reading.contains("Esc:back"), "{reading}");
        assert!(reading.contains("Enter:reply"), "{reading}");
        assert!(!reading.contains("typing"), "no channel typing: {reading}");

        app.handle(crate::keys::Action::ThreadReplyRoot, 131);
        let composing = frame_text(&app, 24, 6);
        assert!(composing.contains("Reply to "), "{composing}");
        assert!(composing.contains("Enter:send"), "{composing}");
        // One context row survives next to the target and the input, and it
        // is the focused row.
        assert!(composing.contains("> "), "{composing}");
        assert!(composing.contains("[root]"), "{composing}");
    }

    #[test]
    fn a_long_conversation_name_is_clipped_after_the_back_hint() {
        let (mut app, _keys) = thread_app(false);
        app.channels[0].name = "a-very-long-conversation-name-indeed".into();
        let text = frame_text(&app, 24, 6);
        assert!(text.contains("Esc:back"), "the exit survives: {text}");
        assert!(text.contains("..."), "the clipped name says so: {text}");
        assert!(!text.contains("indeed"), "{text}");
    }

    #[test]
    fn an_unloaded_thread_claims_loading_and_never_an_empty_thread() {
        let (mut app, _keys) = thread_app(false);
        app.apply(
            crate::session::ChatEvent::Thread {
                channel: app.thread.channel,
                root: app.thread.root.clone(),
                request: 1,
                events: Vec::new(),
                partial: false,
            },
            130,
        );
        // A read that carried nothing at all is a real answer of an empty
        // thread, so ask for the state before the read instead: a fresh view.
        app.thread.rows.clear();
        app.thread.loading = true;
        app.thread.notice = None;
        let text = frame_text(&app, 80, 12);
        assert!(text.contains("Loading thread..."), "{text}");
        assert!(!text.contains("No replies yet"), "{text}");
    }

    #[test]
    fn a_deleted_root_keeps_a_placeholder_and_says_so() {
        let (mut app, _keys) = thread_app(true);
        app.thread.focus = 0;
        app.handle(crate::keys::Action::DeleteRow, 131);
        let text = frame_text(&app, 80, 12);
        assert!(text.contains("Root deleted"), "{text}");
        assert!(text.contains("the reply"), "replies remain: {text}");
    }

    #[test]
    fn a_partial_read_says_the_history_is_bounded() {
        let (mut app, _keys) = thread_app(false);
        app.thread.partial = true;
        let text = frame_text(&app, 80, 12);
        assert!(
            text.contains("Partial thread: reply limit reached"),
            "{text}"
        );
    }

    #[test]
    fn a_stale_thread_keeps_its_rows_and_says_they_are_stale() {
        let (mut app, _keys) = thread_app(false);
        app.conn = ConnState::Reconnecting;
        let text = frame_text(&app, 80, 12);
        assert!(text.contains("reconnecting; stale"), "{text}");
        assert!(text.contains("the reply"), "loaded rows stay: {text}");
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
    fn the_list_heading_names_the_filter_and_marks_an_incomplete_answer() {
        let mut app = chat_app(vec![message_row(0, "hello")]);
        assert!(inbox_title(&app).to_string().contains("Inbox: All"));
        assert!(
            inbox_title(&app).to_string().ends_with(" ?"),
            "a list that cannot promise completeness says so: {}",
            inbox_title(&app)
        );

        app.apply(
            crate::session::ChatEvent::Channels(crate::client::Roster {
                items: Vec::new(),
                complete: true,
            }),
            0,
        );
        app.apply(
            crate::session::ChatEvent::ReadState {
                contexts: std::collections::HashMap::new(),
                complete: true,
            },
            0,
        );
        app.filter = crate::app::Filter::Unread;
        assert!(inbox_title(&app).to_string().contains("Inbox: Unread"));
        assert!(
            !inbox_title(&app).to_string().ends_with(" ?"),
            "a complete roster answers for itself: {}",
            inbox_title(&app)
        );
    }

    #[test]
    fn an_empty_view_says_which_kind_of_empty_it_is() {
        let mut app = chat_app(vec![message_row(0, "hello")]);
        app.apply(
            crate::session::ChatEvent::Channels(crate::client::Roster {
                items: Vec::new(),
                complete: true,
            }),
            0,
        );
        // The sidebar is the list surface at 80 columns; the picker's only
        // row is the conversation the cursor stands on.
        app.filter = crate::app::Filter::Unread;
        let text = frame_text(&app, 80, 12);
        assert!(text.contains("Checking..."), "{text}");
        assert!(
            !text.contains("All read"),
            "a list that has not heard from the relay may not claim zero: {text}"
        );

        app.apply(
            crate::session::ChatEvent::ReadState {
                contexts: std::collections::HashMap::new(),
                complete: true,
            },
            0,
        );
        let text = frame_text(&app, 80, 12);
        assert!(text.contains("No conversations"), "{text}");
        assert!(!text.contains("Checking..."), "{text}");
    }

    #[test]
    fn a_picker_row_whose_state_is_unknown_says_so_instead_of_showing_nothing() {
        let mut app = chat_app(vec![message_row(0, "hello")]);
        app.apply(
            crate::session::ChatEvent::ReadState {
                contexts: std::collections::HashMap::new(),
                complete: false,
            },
            0,
        );
        app.handle(crate::keys::Action::TogglePicker, 0);
        let text = frame_text(&app, 40, 10);
        assert!(
            text.contains("1 ?"),
            "an unknown row carries its own signal: {text}"
        );
    }

    #[test]
    fn the_footer_reports_a_read_the_relay_has_not_been_told_about() {
        let mut app = chat_app(vec![message_row(0, "hello")]);
        assert_eq!(
            app.inbox_footer().as_deref(),
            Some("Tracking new messages from this visit"),
            "a conversation with no marker says where its tracking starts"
        );
        app.apply(
            crate::session::ChatEvent::ReadPublished {
                ok: false,
                reason: "relay refused".into(),
            },
            0,
        );
        assert_eq!(
            app.inbox_footer().as_deref(),
            Some("Read here; not synced (relay refused)"),
            "an unsynced read is the more urgent note"
        );
        let text = frame_text(&app, 80, 12);
        assert!(text.contains("Read here; not"), "the note is shown: {text}");
        assert!(
            text.contains("synced"),
            "and wrapped rather than dropped: {text}"
        );
    }

    #[test]
    fn a_dm_header_names_the_people_without_calling_it_a_channel() {
        let mut app = chat_app(vec![message_row(0, "hello")]);
        app.apply(
            crate::session::ChatEvent::Channels(crate::client::Roster {
                items: vec![crate::client::ChannelInfo {
                    id: app.channels[0].id,
                    name: "DM".to_owned(),
                    kind: crate::client::ChannelKind::Dm,
                    participants: vec!["p".to_owned()],
                    archived: false,
                    hidden: false,
                }],
                complete: true,
            }),
            0,
        );
        let text = frame_text(&app, 80, 12);
        assert!(text.contains("alice"), "{text}");
        assert!(
            !text.contains("#alice"),
            "an octothorpe would claim a DM is a channel: {text}"
        );
    }

    #[test]
    fn the_help_overlay_carries_the_blocked_mention_reference() {
        let mut app = chat_app(vec![message_row(0, "hello")]);
        app.mention_block = Some(crate::app::MentionBlock {
            summary: "mention \"@buzzx build\" matches 2 members; replace it with one exact reference (? for identities)".into(),
            details: vec![
                "@buzzx build -> nostr:npub1aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            ],
        });
        app.help = true;
        app.help_scroll = 0;

        // At the smallest size the reference is below the fold, and scrolling
        // reaches it: it is longer than the terminal is wide.
        let mut seen = frame_text(&app, 24, 6);
        for _ in 0..40 {
            app.handle(crate::keys::Action::HelpScroll(1), 0);
            seen.push_str(&frame_text(&app, 24, 6));
        }
        assert!(
            seen.contains("mention"),
            "the block is named: {}",
            &seen[seen.len().saturating_sub(400)..]
        );
        assert!(
            seen.contains("nostr:npub1"),
            "the exact reference is reachable: {}",
            &seen[seen.len().saturating_sub(400)..]
        );
    }

    #[test]
    fn the_help_overlay_scrolls_and_carries_the_whole_selected_label() {
        let mut app = chat_app(vec![message_row(0, "hello")]);
        app.apply(
            crate::session::ChatEvent::Profiles(vec![
                (
                    "p".into(),
                    "A Very Long Participant Name That Does Not Fit".into(),
                ),
                (
                    "q".into(),
                    "Another Participant With A Long Name Too".into(),
                ),
            ]),
            0,
        );
        let mut entry = App::stub_entry(uuid::Uuid::new_v4(), "DM");
        entry.kind = crate::client::ChannelKind::Dm;
        entry.participants = vec!["p".to_owned(), "q".to_owned()];
        let id = entry.id;
        app.channels.push(entry);
        app.stub_roster();
        app.picker = Some(crate::app::Picker { cursor: id, at: 1 });

        let label = app.label(&app.channels[1]).clone();
        assert!(label.contains("Another Participant"), "{label}");
        app.help = true;
        app.help_scroll = 0;
        let first = frame_text(&app, 24, 6);
        assert!(
            !first.contains("Another Participant"),
            "the label is below the fold: {first}"
        );

        // Every row of the help text is reachable by scrolling, at the
        // smallest size, including the label and the read-state limit.
        let mut seen = first.clone();
        for _ in 0..24 {
            app.handle(crate::keys::Action::HelpScroll(1), 0);
            seen.push_str(&frame_text(&app, 24, 6));
        }
        // The label wraps at 24 columns, so the whole of it is reachable as
        // its parts: the head and the tail of the same name.
        assert!(
            seen.contains("selected:") && seen.contains("Another"),
            "scrolling reaches the label: {}",
            &seen[seen.len().saturating_sub(600)..]
        );
        assert!(
            seen.contains("Fit"),
            "and its end: {}",
            &seen[seen.len().saturating_sub(600)..]
        );
        assert!(
            seen.contains("not synced"),
            "the read-state limit stays reachable"
        );
        assert!(seen.contains("unread"), "the marker legend stays reachable");
    }

    #[test]
    fn the_picker_hint_keeps_its_actions_at_the_floor_size() {
        let mut app = chat_app(vec![message_row(0, "hello world")]);
        app.picker = Some(crate::app::Picker {
            cursor: app.channels[0].id,
            at: 0,
        });
        for (columns, rows, expected) in [
            (24, 6, "enter f filter esc"),
            (40, 10, "enter open f filter esc close"),
            (80, 12, "enter open · f filter · esc close · ? help"),
        ] {
            let text = frame_text(&app, columns, rows);
            let line = text
                .lines()
                .find(|line| line.contains("filter"))
                .expect("the picker hint renders");
            // The hint is the block's bottom title: everything before the
            // border's own dashes is what the user reads.
            let hint = line
                .trim_start_matches('└')
                .split('─')
                .next()
                .unwrap_or_default();
            assert_eq!(hint, expected, "the hint at {columns} columns");
        }
    }

    #[test]
    fn a_long_name_gives_up_columns_instead_of_the_signal() {
        // 18 columns is the sidebar's row width at 80x12.
        let plain = conversation_item(Some(5), Marker::None, 0, "build", false, 18).to_string();
        assert_eq!(plain, "5      build");
        let unread =
            conversation_item(Some(5), Marker::Unread, 0, "buzzx-tui-build", true, 18).to_string();
        assert!(unread.contains('●'), "{unread:?} keeps the unread signal");
        assert!(unread.ends_with('…'), "{unread:?} keeps the typing marker");
        assert_eq!(unread.chars().count(), 18, "{unread:?} fills the row");
        let short = conversation_item(Some(1), Marker::Mention, 0, "dm", true, 18).to_string();
        assert_eq!(short, "1 @    dm …");
    }

    #[test]
    fn the_unread_count_survives_a_long_name() {
        let line = conversation_item(Some(5), Marker::Unread, 12, "buzzx-tui-build", false, 18)
            .to_string();
        assert_eq!(line, "5 ● 12 buzzx-tui-b");
        let mentioned =
            conversation_item(Some(2), Marker::Mention, 3, "design-review", true, 18).to_string();
        assert_eq!(mentioned, "2 @ 3  design-re …");
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
        app.channels
            .push(App::stub_entry(uuid::Uuid::new_v4(), "quiet"));
        app.stub_roster();
        app.apply(
            crate::session::ChatEvent::ReadState {
                contexts: std::collections::HashMap::new(),
                complete: true,
            },
            0,
        );
        app.apply(
            crate::session::ChatEvent::Profiles(vec![("agent-a".into(), "Agent A".into())]),
            130,
        );
        app.apply(
            crate::session::ChatEvent::Typing {
                channel: active,
                pubkey: "agent-a".into(),
                at: 130,
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
        assert!(text.contains("1      general …"), "{text}");
        assert!(
            !text.contains("2      quiet …"),
            "a quiet channel stays plain"
        );
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
                at: 130,
            },
            130,
        );
        let draft = "draft-in-composer";
        let nav = frame_text(&app, 24, 6);
        assert!(nav.contains("@agent-a typing…"), "{nav}");
        assert!(
            !nav.contains(draft),
            "the composer is closed, so the typing line takes the row and the \
             composer takes none: {nav}"
        );

        app.mode = Mode::Composer;
        app.composer.set_text(draft);

        let narrow = frame_text(&app, 40, 10);
        assert!(narrow.contains("@agent-a typing…"), "{narrow}");
        assert!(narrow.contains("enter send"), "the composer keeps its hint");
        assert!(narrow.contains(draft), "{narrow}");

        // 24x6 is the floor: header, one timeline row, typing, composer, and
        // status must all fit at once.
        let minimal = frame_text(&app, 24, 6);
        assert!(minimal.contains("@agent-a typing…"), "{minimal}");
        assert!(
            minimal.contains(&format!("> {draft}")),
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
        app.channels.push(App::stub_entry(second, "second"));
        app.stub_roster();
        app.apply(
            crate::session::ChatEvent::ReadState {
                contexts: std::collections::HashMap::new(),
                complete: true,
            },
            0,
        );
        app.apply(
            crate::session::ChatEvent::Typing {
                channel: second,
                pubkey: "agent-b".into(),
                at: 130,
            },
            130,
        );
        app.picker = Some(crate::app::Picker {
            cursor: second,
            at: 1,
        });

        let text = frame_text(&app, 40, 10);
        assert!(text.contains("2      second …"), "{text}");
        assert!(!text.contains("1      general …"), "{text}");
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
        let header = row_lines(&row, 110, 40, true, RootMark::None)[0].to_string();
        assert!(header.starts_with("*alice 10s"), "{header}");
        assert!(header.contains(" <"), "{header}");
        assert!(header.contains("@you"), "{header}");
    }

    fn agent_frame(agent: &str, channel: Option<Uuid>, turn: &str) -> crate::agents::Frame {
        crate::agents::Frame {
            agent: agent.to_owned(),
            kind: crate::agents::Kind::Started,
            channel: channel.map(|id| id.to_string()),
            turn: Some(turn.to_owned()),
            seq: 1,
        }
    }

    /// An open Agents overlay. `observed` is whether the observer feed is live.
    fn agent_app(observed: bool, names: &[(&str, &str)]) -> App {
        let mut app = chat_app(Vec::new());
        app.agents.roster = crate::agents::Load::Loaded(crate::agents::Roster {
            agents: names
                .iter()
                .map(|(name, pubkey)| crate::agents::OwnedAgent {
                    pubkey: (*pubkey).to_owned(),
                    name: (*name).to_owned(),
                })
                .collect(),
            ..crate::agents::Roster::default()
        });
        app.agents.observed = observed;
        app.agents.open = true;
        app
    }

    #[test]
    fn the_agents_list_shows_a_name_and_a_status_word_at_every_size() {
        let app = agent_app(true, &[("Ada", "aa")]);
        for (width, height) in [(24, 6), (40, 10), (79, 12), (80, 12)] {
            let text = frame_text(&app, width, height);
            assert!(text.contains("Ada"), "{width}x{height}: {text}");
            assert!(
                text.contains("No active turn"),
                "the state is named at {width}x{height}: {text}"
            );
            assert!(
                text.contains("esc"),
                "the way out is on screen at {width}x{height}: {text}"
            );
        }
    }

    #[test]
    fn an_unobserved_agent_reads_unknown_rather_than_idle() {
        let app = agent_app(false, &[("Ada", "aa")]);
        let text = frame_text(&app, 80, 12);
        assert!(text.contains("Unknown"), "{text}");
        assert!(
            !text.contains("No active turn"),
            "no live feed is no quiet Agent: {text}"
        );
    }

    #[test]
    fn the_agent_detail_shows_the_evidence_behind_its_status() {
        let mut app = agent_app(true, &[("Ada", "aa")]);
        let id = app.channels[0].id;
        app.agents
            .work
            .apply(&agent_frame("aa", Some(id), "t1"), 127);
        app.agents.cursor.level = crate::agents::Level::Detail;
        let text = frame_text(&app, 80, 12);
        assert!(text.contains("Agent Ada"), "{text}");
        assert!(text.contains("Working"), "{text}");
        assert!(text.contains("last signal"), "{text}");
        assert!(text.contains("3s ago"), "the age of the evidence: {text}");
        assert!(text.contains("general"), "the context names it: {text}");
        assert!(text.contains("esc"), "the way back is on screen: {text}");
    }

    #[test]
    fn a_context_the_user_is_not_in_hides_its_identity() {
        let mut app = agent_app(true, &[("Ada", "aa")]);
        let elsewhere = Uuid::from_u64_pair(0xdead_beef, 0xfeed);
        app.agents
            .work
            .apply(&agent_frame("aa", Some(elsewhere), "t1"), 130);
        app.agents.cursor.level = crate::agents::Level::Detail;
        let text = frame_text(&app, 80, 12);
        assert!(text.contains("Unavailable context"), "{text}");
        assert!(
            !text.contains(&elsewhere.to_string()),
            "no id, no name of a conversation outside the user's channels: {text}"
        );
    }

    #[test]
    fn a_row_gives_up_its_name_before_the_status_it_carries() {
        let working = crate::app::AgentStatus::Working(1);
        let line = agent_row("a-very-long-agent-name", &working, 20)[0].to_string();
        assert!(line.ends_with("Working"), "{line}");
        assert_eq!(line.chars().count(), 20, "{line}");
        // Too little room for both: the name takes a line, the status the next.
        let rows = agent_row("Ada", &working, 10);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].to_string(), "Ada");
        assert_eq!(rows[1].to_string().trim(), "Working");
    }

    #[test]
    fn the_roster_states_keep_their_own_words() {
        assert_eq!(
            roster_state_lines(&crate::agents::Load::Failed("x".into()))[0],
            "Cannot load agents"
        );
        assert_eq!(
            roster_state_lines(&crate::agents::Load::Unavailable("x".into()))[0],
            "Agent overview unavailable for this identity"
        );
        let empty = crate::agents::Load::Loaded(crate::agents::Roster::default());
        assert_eq!(roster_state_lines(&empty), vec!["No agents".to_owned()]);
        let partial = crate::agents::Load::Loaded(crate::agents::Roster {
            unverified: 1,
            ..crate::agents::Roster::default()
        });
        let lines = roster_state_lines(&partial);
        assert!(
            lines.iter().any(|line| line.starts_with("No agents")),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line.starts_with("Ownership unverified")),
            "an unverified record is not an owned Agent: {lines:?}"
        );
    }

    #[test]
    fn the_detail_window_follows_the_selected_context() {
        assert_eq!(scroll_window(3, 10, 0), 0..3);
        assert_eq!(scroll_window(10, 4, 0), 0..4);
        assert_eq!(scroll_window(10, 4, 9), 6..10);
        assert_eq!(scroll_window(10, 0, 5), 0..0);
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
            row_lines(row, 0, 40, compact, RootMark::None)
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
