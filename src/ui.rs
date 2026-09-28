//! Rendering only. `ui.rs` reads `App` state and draws; it never mutates it
//! and never touches the network. The layout mode follows the frame size; the
//! timeline is the only base surface, and the conversation list is an overlay
//! at every size.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};

use crate::agents;
use crate::app::{
    AgentStatus, App, Command, ConnState, Context, Filter, Marker, Mode, ReaderOrigin, Sections,
};
use crate::content::{Row, short_pubkey};
use crate::layout::{self, LayoutMode};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use uuid::Uuid;

fn no_color() -> bool {
    static NO_COLOR: std::sync::LazyLock<bool> =
        std::sync::LazyLock::new(|| std::env::var("NO_COLOR").is_ok());
    *NO_COLOR
}

fn author_style() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

fn action_style() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

fn pending_style() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

fn separator_style() -> Style {
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

fn failure_style() -> Style {
    if no_color() {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(Color::LightRed)
            .add_modifier(Modifier::BOLD)
    }
}

fn highlight_style() -> Style {
    Style::default().add_modifier(Modifier::REVERSED)
}

fn target_text(app: &App) -> String {
    if app.composer.edit.is_some() {
        "Edit own message".to_owned()
    } else if let Some(reply) = &app.composer.reply {
        format!("Reply to {}", reply.author)
    } else {
        "New message".to_owned()
    }
}

fn conn_word(state: ConnState) -> &'static str {
    match state {
        ConnState::Connecting => "connecting",
        ConnState::Connected => "connected",
        ConnState::Reconnecting => "reconnecting",
    }
}
fn status_style(status: &str) -> Style {
    let lower = status.to_ascii_lowercase();
    if ["failed", "refused", "error"]
        .iter()
        .any(|word| lower.contains(word))
    {
        failure_style()
    } else if [
        "unknown",
        "uncertain",
        "unconfirmed",
        "partial",
        "stale",
        "reconnecting",
        "not synced",
    ]
    .iter()
    .any(|word| lower.contains(word))
    {
        mention_style()
    } else {
        pending_style()
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

#[derive(Clone, Copy)]
struct RowOptions {
    roomy: bool,
    focus_visible: bool,
    head: RootMark,
}

/// The default row renderer keeps the pure content tests independent from
/// viewport decoration.
#[cfg(test)]
fn row_lines(
    row: &crate::content::Row,
    now: u64,
    width: u16,
    compact: bool,
    mark: RootMark,
) -> Vec<Line<'static>> {
    row_lines_with_options(row, now, width, compact, mark, false, false)
}

/// Render one message. Separators and focus are presentation options layered
/// onto the same event-derived lines; neither creates a selectable row.
fn row_lines_with_options(
    row: &crate::content::Row,
    now: u64,
    width: u16,
    compact: bool,
    mark: RootMark,
    focused: bool,
    compact_rule: bool,
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
    let prefix_width: usize = header
        .iter()
        .map(|span| span.content.as_ref().width())
        .sum();
    let mut suffix: Vec<Span<'static>> = vec![Span::raw(format!(" {}", age(row.created_at, now)))];
    if mark != RootMark::None {
        suffix.push(Span::styled("  [root]".to_owned(), pending_style()));
    }
    if row.pending {
        suffix.push(Span::styled(" ...".to_owned(), pending_style()));
    }
    if row.uncertain {
        suffix.push(Span::styled(" ? Unconfirmed".to_owned(), mention_style()));
    }
    if row.parent_id.is_some() {
        suffix.push(Span::raw(reply));
    }
    if row.broadcast {
        suffix.push(Span::raw(broadcast));
    }
    if row.mentions_me {
        suffix.push(Span::styled(" @you".to_owned(), mention_style()));
    }
    if row.edited {
        suffix.push(Span::raw(edited));
    }
    let suffix_width = |spans: &[Span<'static>]| {
        spans
            .iter()
            .map(|span| span.content.as_ref().width())
            .sum::<usize>()
    };
    let fits = |spans: &[Span<'static>]| prefix_width + 1 + suffix_width(spans) < width as usize;
    if !fits(&suffix) {
        // Age is secondary; semantic state words are not. Drop it before
        // shortening the author, so a long name cannot hide @you or failure.
        suffix.remove(0);
    }
    // Metadata yields in a fixed order when a narrow header cannot carry all
    // of it. Attention and write-state words stay ahead of reply decoration.
    for marker in [edited, broadcast, reply, "  [root]"] {
        if fits(&suffix) {
            break;
        }
        if let Some(index) = suffix
            .iter()
            .position(|span| span.content.as_ref() == marker)
        {
            suffix.remove(index);
        }
    }
    if !fits(&suffix)
        && let Some(index) = suffix
            .iter()
            .position(|span| span.content.as_ref() == " ...")
    {
        suffix.remove(index);
    }
    let suffix_width = suffix_width(&suffix);
    let author_budget = (width as usize)
        .saturating_sub(prefix_width + 1 + suffix_width)
        .max(1);
    header.push(Span::styled(
        clip_with_ellipsis(&row.author, author_budget),
        if focused {
            action_style()
        } else {
            author_style()
        },
    ));
    header.extend(suffix);
    let header_width: usize = header
        .iter()
        .map(|span| span.content.as_ref().width())
        .sum();
    if compact_rule && header_width + 6 <= width as usize {
        header.push(Span::raw("  "));
        header.push(Span::styled(
            "─".repeat(width as usize - header_width - 2),
            separator_style(),
        ));
    }

    let mut lines = vec![Line::from(header)];
    let body_style = if row.pending || row.uncertain {
        pending_style()
    } else {
        Style::default()
    };
    if mark == RootMark::Deleted {
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

/// Greedy word wrap measured in terminal cells, so wide CJK and emoji yield
/// before decoration or a footer can be overwritten.
fn wrap(line: &str, width: u16) -> Vec<String> {
    let width = width.max(8) as usize;
    if line.is_empty() {
        return vec![String::new()];
    }
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in line.split(' ') {
        let word_width = word.width();
        if word_width > width {
            if !current.is_empty() {
                out.push(std::mem::take(&mut current));
            }
            let mut rest = word;
            while rest.width() > width {
                let (chunk, tail) = split_cells(rest, width);
                out.push(chunk.to_owned());
                rest = tail;
            }
            current.push_str(rest);
            continue;
        }
        let candidate_width = current.width() + word_width + usize::from(!current.is_empty());
        if candidate_width > width && !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    out.push(current);
    out
}

fn split_cells(text: &str, max_width: usize) -> (&str, &str) {
    let mut used = 0;
    for (index, ch) in text.char_indices() {
        let width = ch.width().unwrap_or(0);
        if used > 0 && used + width > max_width {
            return (&text[..index], &text[index..]);
        }
        used += width;
    }
    (text, "")
}

fn cursor_window_start(line: &str, cursor: usize, width: usize) -> usize {
    let cursor = cursor.min(line.len());
    let before = &line[..cursor];
    let wanted_start = before
        .width()
        .saturating_sub(width.max(1).saturating_sub(1));
    let mut used = 0;
    for (index, ch) in line.char_indices() {
        let ch_width = ch.width().unwrap_or(0);
        if used + ch_width > wanted_start {
            return index;
        }
        used += ch_width;
    }
    line.len()
}

/// The visible slice of one composer line around the cursor, so a narrow
/// composer keeps the insertion point on screen. The draft itself is
/// unchanged; this is a view of it.
fn cursor_window(line: &str, cursor: usize, width: usize) -> String {
    let cursor = cursor.min(line.len());
    let start = cursor_window_start(line, cursor, width);
    split_cells(&line[start..], width.max(1)).0.to_owned()
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
        // Full-message reading replaces the channel or thread surface.
        _ if app.reader.open => draw_reader(frame, app, now, area),
        _ if app.context.open => draw_context(frame, app, now, area),
        _ if app.search.open => draw_search(frame, app, now, area),
        _ if app.thread.open => draw_thread(frame, app, now, area, mode),
        LayoutMode::Wide | LayoutMode::Narrow => draw_column(frame, app, now, area, mode),
        LayoutMode::Minimal => draw_minimal(frame, app, now, area),
    }
    if app.agents.open {
        draw_agents(frame, app, now, area);
    }
    if app.switcher.is_some() {
        draw_switcher(frame, app, now, area);
    }
    if app.palette.is_some() {
        draw_palette(frame, app, area);
    }
    if app.community_picker.is_some() {
        draw_community_picker(frame, app, area);
    }
    if app.help {
        draw_help(frame, app, area, mode);
    }
}

fn reader_label(app: &App, channel: Uuid) -> String {
    match app.channels.iter().find(|entry| entry.id == channel) {
        Some(entry) if entry.is_dm() => app.label(entry),
        Some(entry) => format!("#{}", app.label(entry)),
        None => channel.to_string(),
    }
}

fn draw_reader(frame: &mut Frame, app: &App, now: u64, area: Rect) {
    let compact = area.width < 80;
    let channel = app.reader.channel();
    let thread = matches!(app.reader.origin, Some(ReaderOrigin::Thread { .. }));
    let title = match channel {
        Some(channel) if compact => format!("Message {}", reader_label(app, channel)),
        Some(channel) if thread => format!("Message / Thread {}", reader_label(app, channel)),
        Some(channel) => format!("Message / {}", reader_label(app, channel)),
        None => "Message".to_owned(),
    };
    let title = format!(
        "{}  {}",
        clip_with_ellipsis(&title, area.width.saturating_sub(12) as usize),
        if compact { "Esc:back" } else { "Esc: back" }
    );
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(2),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(area);
    frame.render_widget(Paragraph::new(title), rows[0]);

    let metadata = app
        .reader
        .row
        .as_ref()
        .map(|row| {
            let edited = if row.edited { " · edited" } else { "" };
            format!("{} · {}{}", row.author, age(row.created_at, now), edited)
        })
        .unwrap_or_else(|| "Message deleted".to_owned());
    frame.render_widget(
        Paragraph::new(clip_with_ellipsis(&metadata, rows[1].width as usize)),
        rows[1],
    );

    let body_lines = app
        .reader
        .row
        .as_ref()
        .map(|row| reader_lines(row, rows[2].width))
        .unwrap_or_else(|| vec!["Message deleted".to_owned()]);
    let total = body_lines.len();
    let max_scroll = total.saturating_sub(rows[2].height as usize);
    let start = app.reader.scroll.min(max_scroll);
    let visible = body_lines
        .iter()
        .skip(start)
        .take(rows[2].height as usize)
        .cloned()
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(visible.join("\n")), rows[2]);

    let actions = if compact {
        "Enter reply · j/k scroll · Esc/v back · ?"
    } else {
        "j/k scroll · PgUp/PgDn · Enter reply · Esc/v back · ? help"
    };
    frame.render_widget(
        Paragraph::new(clip_with_ellipsis(actions, rows[3].width as usize)),
        rows[3],
    );
    let state = if let Some(notice) = &app.reader.notice {
        notice.clone()
    } else if app.reader.deleted {
        "Message deleted".to_owned()
    } else {
        let end = (start + rows[2].height as usize).min(total);
        format!(
            "{} · lines {}-{} of {}",
            conn_word(app.conn),
            start + 1,
            end,
            total
        )
    };
    frame.render_widget(
        Paragraph::new(Line::styled(
            clip_with_ellipsis(&state, rows[4].width as usize),
            status_style(&state),
        )),
        rows[4],
    );
}

fn draw_search(frame: &mut Frame, app: &App, now: u64, area: Rect) {
    let compact = area.width < 80;
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(area);
    let title = if compact {
        "Search".to_owned()
    } else {
        format!(
            "Search · {} · {}",
            app.search_scope_label(app.search.applied_scope),
            app.search.applied_time.label()
        )
    };
    frame.render_widget(Paragraph::new(title), rows[0]);
    let line = if app.search.scope_pick.is_some() {
        "choose conversation".to_owned()
    } else if let Some(pick) = &app.search.author_pick {
        if pick.pubkey_mode {
            format!("pubkey> {}_", pick.pubkey_entry)
        } else if compact {
            "authors · incomplete".to_owned()
        } else {
            "author picker · known profiles · the list may be incomplete".to_owned()
        }
    } else if app.search.editing {
        format!("search> {}_", app.search.query)
    } else {
        let mut line = format!("search: {}", app.search.applied_query);
        if let Some(author) = &app.search.applied_author {
            line.push_str(&format!(" · author {}", app.author_label(author)));
        }
        line
    };
    frame.render_widget(
        Paragraph::new(clip_with_ellipsis(&line, rows[1].width as usize)),
        rows[1],
    );
    if let Some(pick) = &app.search.author_pick {
        draw_author_pick(frame, app, pick, rows[2]);
    } else if let Some(pick) = &app.search.scope_pick {
        draw_scope_pick(frame, app, pick, rows[2]);
    } else if app.search.form {
        draw_filter_form(frame, app, rows[2], compact);
    } else {
        // In the all-accessible scope each result names its conversation;
        // the label is presentation, identity stays the event id.
        let results: Vec<crate::content::Row> =
            if app.search.applied_scope == crate::app::SearchScope::All {
                app.search
                    .results
                    .iter()
                    .enumerate()
                    .map(|(index, row)| {
                        let mut row = row.clone();
                        if let Some(channel) = app.search.channels.get(index) {
                            row.body = format!("[{}] {}", app.label_for(*channel), row.body);
                        }
                        row
                    })
                    .collect()
            } else {
                app.search.results.clone()
            };
        draw_rows(
            frame,
            &results,
            app.search.focus,
            rows[2],
            now,
            compact,
            RowOptions {
                roomy: area.width >= 40 && area.height >= 16,
                focus_visible: !app.search.editing,
                head: RootMark::None,
            },
        );
    }
    let footer = if app.search.scope_pick.is_some() {
        "j/k move · Enter choose · Esc back"
    } else if app.search.author_pick.is_some() {
        "j/k move · Enter select · p exact pubkey · Esc back"
    } else if app.search.form {
        "Enter apply · Esc cancel"
    } else if app.search.loading {
        "searching… · Esc back"
    } else if app.search.failed.is_some() {
        "search failed · Enter retry · / edit · f filters · Esc back"
    } else {
        "j/k move · Enter open context · / edit · f filters · Esc back"
    };
    frame.render_widget(
        Paragraph::new(clip_with_ellipsis(footer, rows[3].width as usize)),
        rows[3],
    );
    let status = search_status(app);
    frame.render_widget(
        Paragraph::new(Line::styled(
            clip_with_ellipsis(&status, rows[4].width as usize),
            status_style(&status),
        )),
        rows[4],
    );
}

/// Loading, no returned matches, a failed read, a bounded page, and the
/// previous successful query after a failure are all distinct states. When
/// visibility filtering hid returned hits, the visible list is labeled
/// bounded rather than presented as the whole search space.
fn search_status(app: &App) -> String {
    let hidden = if app.search.hidden > 0 {
        format!(" · bounded: {} hidden by visibility", app.search.hidden)
    } else {
        String::new()
    };
    if let Some(reason) = &app.search.failed {
        let mut status = format!("search failed: {reason}");
        if let Some(previous) = &app.search.previous {
            status.push_str(&format!(" · results from previous query {previous}"));
        }
        return status;
    }
    if app.search.loading {
        let mut status = "searching…".to_owned();
        if let Some(previous) = &app.search.previous {
            status.push_str(&format!(" · showing previous query {previous}"));
        }
        return status;
    }
    if app.search.results.is_empty() {
        if app.search.applied_query.is_empty() && app.search.applied_author.is_none() {
            // Nothing has been submitted yet: invite, do not search.
            return match app.status.is_empty() {
                true => "type a keyword, Enter submit".to_owned(),
                false => app.status.clone(),
            };
        }
        return format!("no returned matches{hidden}");
    }
    if app.search.bounded {
        return format!("Top 50; narrow filters{hidden}");
    }
    format!("{} results returned{hidden}", app.search.results.len())
}

/// The filter form replaces the results while it is open. The letters work
/// from any row, so every value stays reachable at the smallest width.
fn draw_filter_form(frame: &mut Frame, app: &App, body: Rect, compact: bool) {
    let controls = [
        ("scope", app.search_scope_label(app.search.scope)),
        (
            "author",
            app.search
                .author
                .as_deref()
                .map(|author| app.author_label(author))
                .unwrap_or_else(|| "any author".to_owned()),
        ),
        ("time", app.search.time.label().to_owned()),
    ];
    let hints = ["  s cycle · o choose", "  a pick · h clear", "  t cycle"];
    let lines: Vec<Line> = controls
        .iter()
        .enumerate()
        .map(|(index, (label, value))| {
            let marker = if app.search.form_row == index {
                "> "
            } else {
                "  "
            };
            let hint = if compact { "" } else { hints[index] };
            Line::from(format!("{marker}{label:6} {value}{hint}"))
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), body);
}

fn draw_author_pick(frame: &mut Frame, app: &App, pick: &crate::app::AuthorPick, body: Rect) {
    let lines: Vec<Line> = pick
        .candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            let marker = if pick.focus == index { "> " } else { "  " };
            Line::from(format!("{marker}{}", app.author_label(&candidate.pubkey)))
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), body);
}

fn draw_scope_pick(frame: &mut Frame, app: &App, pick: &crate::app::ScopePick, body: Rect) {
    let lines: Vec<Line> = pick
        .options
        .iter()
        .enumerate()
        .map(|(index, id)| {
            let marker = if pick.focus == index { "> " } else { "  " };
            Line::from(format!("{marker}{}", app.label_for(*id)))
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), body);
}
fn draw_context(frame: &mut Frame, app: &App, now: u64, area: Rect) {
    let compact = area.width < 80;
    let composing = app.mode == Mode::Composer;
    let roomy = area.width >= 40 && area.height >= 16;
    let input_rows = u16::from(composing) * if roomy { 2 } else { 1 };
    let target_rows = u16::from(composing);
    let column = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(target_rows),
        Constraint::Length(input_rows),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(area);
    let label = reader_label(app, app.context.channel);
    let title = if compact {
        format!("Context {label}")
    } else {
        format!("Context / {label} · {}", app.context.target)
    };
    frame.render_widget(Paragraph::new(title), column[0]);
    draw_rows(
        frame,
        &app.context.rows,
        app.context.focus,
        column[1],
        now,
        compact,
        RowOptions {
            roomy,
            focus_visible: !composing,
            head: RootMark::None,
        },
    );
    if composing {
        draw_composer_target(frame, app, column[2]);
        draw_composer(frame, app, column[3]);
    }
    let keys = if composing {
        "Enter: send · Esc: cancel compose"
    } else if compact {
        "j/k move · t thread · v read · [/] more · Esc back"
    } else {
        "j/k move · Enter reply · t thread · v read · [/] older/newer · Esc back"
    };
    frame.render_widget(
        Paragraph::new(clip_with_ellipsis(keys, column[4].width as usize)),
        column[4],
    );
    let status = if let Some(notice) = &app.context.notice {
        notice.clone()
    } else if let Some(reason) = &app.context.failed {
        format!("context failed: {reason}")
    } else if app.context.loading && app.context.rows.is_empty() {
        "loading context…".to_owned()
    } else if app.context.older_loading || app.context.newer_loading {
        "loading more context…".to_owned()
    } else if !app.context.before_complete || !app.context.after_complete {
        "partial context · [ older · ] newer".to_owned()
    } else {
        conn_word(app.conn).to_owned()
    };
    frame.render_widget(
        Paragraph::new(Line::styled(
            clip_with_ellipsis(&status, column[5].width as usize),
            status_style(&status),
        )),
        column[5],
    );
}

/// Split a message into source-preserving display lines. Unlike timeline
/// wrapping, this keeps indentation, repeated spaces, blank lines, and tabs.
fn reader_lines(row: &Row, width: u16) -> Vec<String> {
    let width = usize::from(width.max(1));
    let mut lines = row
        .body
        .split('\n')
        .flat_map(|line| wrap_reader_line(line, width))
        .collect::<Vec<_>>();
    if let Some(attachment) = &row.attachment {
        lines.push(format!("[file] {attachment}"));
    }
    if !row.reactions.is_empty() {
        lines.push(
            row.reactions
                .iter()
                .map(|(emoji, count)| format!("{emoji} x{count}"))
                .collect::<Vec<_>>()
                .join(" "),
        );
    }
    lines
}

fn wrap_reader_line(line: &str, width: usize) -> Vec<String> {
    let mut expanded = String::new();
    let mut cells = 0;
    for grapheme in line.graphemes(true) {
        if grapheme == "\t" {
            let spaces = 4 - cells % 4;
            expanded.push_str(&" ".repeat(spaces));
            cells += spaces;
        } else {
            expanded.push_str(grapheme);
            cells += grapheme.width();
        }
    }
    if expanded.is_empty() {
        return vec![String::new()];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut used = 0;
    for grapheme in expanded.graphemes(true) {
        let grapheme_width = grapheme.width();
        if used > 0 && used + grapheme_width > width {
            lines.push(std::mem::take(&mut current));
            used = 0;
        }
        current.push_str(grapheme);
        used += grapheme_width;
    }
    lines.push(current);
    lines
}

/// One column: the conversation on top, its timeline, the composer, and the
/// hint and status lines at the bottom. The conversation list is never drawn
/// beside it: `c` opens the switcher over this column at every width, so the
/// timeline keeps the whole width in every layout.
///
/// Wide and narrow differ in density alone: the wide header leaves the
/// connection state to the status line, and its timeline uses the full rows.
fn draw_column(frame: &mut Frame, app: &App, now: u64, area: Rect, mode: LayoutMode) {
    let wide = mode == LayoutMode::Wide;
    let composing = app.mode == Mode::Composer;
    let roomy = area.width >= 40 && area.height >= 16;
    let typing = typing_text(app, now, !wide);
    let input_rows = u16::from(composing) * if roomy { 2 } else { 1 };
    let target_rows = u16::from(composing);
    let gap_rows = u16::from(composing && roomy);
    let column = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(u16::from(!typing.is_empty())),
        Constraint::Length(target_rows),
        Constraint::Length(gap_rows),
        Constraint::Length(input_rows),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(area);
    draw_header(frame, app, column[0], !wide);
    draw_timeline(frame, app, column[1], now, false, roomy);
    draw_typing(frame, &typing, column[2]);
    if composing {
        draw_composer_target(frame, app, column[3]);
        draw_composer(frame, app, column[5]);
    }
    draw_channel_keys(frame, area.width, column[6], composing);
    draw_status(frame, app, column[7], wide);
}

/// The focused thread: one full-screen timeline at every size, with the
/// conversation named in the header and the way back next to it.
///
/// Nothing channel-scoped is drawn here. Typing is per channel, so it cannot
/// say who is replying to this thread, and it is left out.
fn draw_thread(frame: &mut Frame, app: &App, now: u64, area: Rect, mode: LayoutMode) {
    let compact = mode != LayoutMode::Wide;
    let composing = app.mode == Mode::Composer;
    let roomy = area.width >= 40 && area.height >= 16;
    let input_rows = u16::from(composing) * if roomy { 2 } else { 1 };
    let target_rows = u16::from(composing);
    let gap_rows = u16::from(composing && roomy);
    let column = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(target_rows),
        Constraint::Length(gap_rows),
        Constraint::Length(input_rows),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(area);
    draw_thread_header(frame, app, column[0], compact);
    draw_thread_timeline(frame, app, column[1], now, compact, roomy);
    if composing {
        draw_composer_target(frame, app, column[2]);
        draw_composer(frame, app, column[4]);
    }
    draw_thread_keys(frame, app, column[5], compact);
    draw_thread_status(frame, app, column[6], compact);
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
fn draw_thread_timeline(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    now: u64,
    compact: bool,
    roomy: bool,
) {
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
        RowOptions {
            roomy,
            focus_visible: app.mode != Mode::Composer,
            head,
        },
    );
}

/// The composer target is an explicit destination, not a decoration attached
/// to whichever row happens to be nearest the input.
fn draw_composer_target(frame: &mut Frame, app: &App, area: Rect) {
    if area.is_empty() {
        return;
    }
    let line = Line::from(vec![
        Span::styled("│ ", separator_style()),
        Span::styled(target_text(app), action_style()),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn draw_thread_keys(frame: &mut Frame, app: &App, area: Rect, compact: bool) {
    let keys = match (app.mode == Mode::Composer, compact) {
        (false, false) => "j/k: move  Enter: reply  i: root  G: latest  ?: help",
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
    let status = thread_status(app, compact);
    frame.render_widget(
        Paragraph::new(Line::styled(status.clone(), status_style(&status))),
        area,
    );
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
    if app.thread.observed_new > 0 {
        // What this client saw since the view left the tail, never a claim
        // about the thread's total reply count.
        let count = app.thread.observed_new;
        return if compact {
            format!("{count} new · G latest")
        } else if count == 1 {
            "1 new reply · G latest".to_owned()
        } else {
            format!("{count} new replies · G latest")
        };
    }
    if app.thread.loaded() && app.thread.rows.len() <= 1 {
        return "No replies yet".to_owned();
    }
    conn_word(app.conn).to_owned()
}

fn draw_minimal(frame: &mut Frame, app: &App, now: u64, area: Rect) {
    let composing = app.mode == Mode::Composer;
    let typing = if !composing && area.height >= 7 {
        typing_text(app, now, true)
    } else {
        String::new()
    };
    let column = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(if composing { 1 } else { 3 }),
        Constraint::Length(u16::from(!typing.is_empty())),
        Constraint::Length(u16::from(composing)),
        Constraint::Length(u16::from(composing)),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(area);
    draw_header(frame, app, column[0], true);
    draw_timeline(frame, app, column[1], now, true, false);
    draw_typing(frame, &typing, column[2]);
    if composing {
        draw_composer_target(frame, app, column[3]);
        draw_composer_line(frame, app, column[4]);
    }
    draw_channel_keys(frame, area.width, column[5], composing);
    draw_status(frame, app, column[6], false);
}

fn draw_channel_keys(frame: &mut Frame, width: u16, area: Rect, composing: bool) {
    let keys = if composing {
        "Enter:send Esc:nav"
    } else if width >= 80 {
        "j/k move · c switch · Enter reply · ? help"
    } else if width >= 40 {
        "j/k move · c switch · Enter reply"
    } else {
        "j/k · c · Enter · ?"
    };
    frame.render_widget(Paragraph::new(keys), area);
}

/// The conversation on the left and the Inbox summary on the right. The
/// compact modes add the connection state here, because their status line
/// carries the relay's answer.
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
    let mut right: Vec<String> = Vec::new();
    if with_conn {
        right.push(conn_word(app.conn).to_owned());
    }
    right.push(app.inbox_summary());
    let right = right.join(" · ");
    let right_width = right.width() as u16;
    // With more than one saved community the header says which one is
    // active; with one it would only repeat the profile name. The
    // conversation keeps its name either way: below this the summary is
    // dropped rather than cutting the conversation in half.
    let context = if app.communities.len() > 1 {
        format!("{} / {}", app.community_name, channel)
    } else {
        channel
    };
    if area.width < 36 || right_width + 12 > area.width {
        frame.render_widget(
            Paragraph::new(Line::styled(
                clip_with_ellipsis(&context, area.width as usize),
                author_style(),
            )),
            area,
        );
        return;
    }
    let left_width = area.width - right_width - 2;
    let columns = Layout::horizontal([
        Constraint::Length(left_width),
        Constraint::Length(right_width + 2),
    ])
    .split(area);
    frame.render_widget(
        Paragraph::new(Line::styled(
            clip_with_ellipsis(&context, left_width as usize),
            author_style(),
        )),
        columns[0],
    );
    frame.render_widget(
        Paragraph::new(Line::styled(right, pending_style())).alignment(Alignment::Right),
        columns[1],
    );
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

/// The switcher's rows. Sections are in a fixed order, and a view with nothing
/// in it says what its emptiness means instead of showing headings: a typed
/// name filter that matched nothing says so, and the filter's own emptiness
/// keeps the four answers `empty_view` distinguishes.
fn switcher_rows(app: &App, view: &Sections) -> Vec<ListRow> {
    let mut rows = Vec::new();
    if view.channels.is_empty() && view.dms.is_empty() {
        if app.switcher_query.trim().is_empty() {
            rows.push(ListRow::Empty(app.empty_view()));
        } else {
            rows.push(ListRow::Empty("No match"));
        }
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
/// switcher's cursor land on the conversation and not on a heading.
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

/// The timeline, drawn as a line window instead of a widget list. A widget
/// list drops any item taller than its area, and in a chat one long message
/// is exactly that; this keeps the focused row's own lines on screen.
fn draw_timeline(frame: &mut Frame, app: &App, area: Rect, now: u64, compact: bool, roomy: bool) {
    let rows = app
        .channels
        .get(app.selected)
        .map(|e| e.rows.as_slice())
        .unwrap_or(&[]);
    draw_rows(
        frame,
        rows,
        app.focus,
        area,
        now,
        compact,
        RowOptions {
            roomy,
            focus_visible: app.mode != Mode::Composer,
            head: RootMark::None,
        },
    );
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
    options: RowOptions,
) {
    if area.is_empty() {
        return;
    }
    let body_width = area.width.saturating_sub(2);
    if rows.is_empty() {
        return;
    }

    let focus = focus.min(rows.len() - 1);
    let compact_rule = area.width >= 40 && !options.roomy;
    // Flatten the rows, and remember where each row starts, so the window can
    // be placed in lines rather than in whole rows.
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut starts: Vec<usize> = Vec::with_capacity(rows.len() + 1);
    for (index, row) in rows.iter().enumerate() {
        starts.push(lines.len());
        let mark = if index == 0 {
            options.head
        } else {
            RootMark::None
        };
        lines.extend(row_lines_with_options(
            row,
            now,
            body_width,
            compact,
            mark,
            options.focus_visible && index == focus,
            compact_rule,
        ));
        if options.roomy && index + 1 < rows.len() {
            lines.push(Line::styled(
                "─".repeat(body_width as usize),
                separator_style(),
            ));
        }
    }
    starts.push(lines.len());

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
            let symbol = if options.focus_visible && row == focus && at == starts[focus] {
                "> "
            } else {
                "  "
            };
            let mut spans = vec![Span::raw(symbol)];
            spans.extend(line.spans);
            Line::from(spans)
        })
        .collect();
    frame.render_widget(Paragraph::new(visible), area);
}

fn composer_prefix(app: &App) -> &'static str {
    if app.composer.edit.is_some() {
        "e> "
    } else if app.composer.reply.is_some() {
        "r> "
    } else {
        "> "
    }
}

fn cursor_offset(line: &str, cursor: usize, width: usize) -> usize {
    let cursor = cursor.min(line.len());
    let start = cursor_window_start(line, cursor, width);
    line[start..cursor].width()
}

fn draw_composer(frame: &mut Frame, app: &App, area: Rect) {
    if area.is_empty() {
        return;
    }
    let prefix = composer_prefix(app);
    let cursor_line = app.composer.cursor.0;
    let cursor_col = app.composer.cursor.1;
    let visible = area.height as usize;
    let skip = cursor_line.saturating_sub(visible.saturating_sub(1));
    let mut lines = Vec::with_capacity(visible.max(1));
    for (index, line) in app
        .composer
        .lines
        .iter()
        .skip(skip)
        .take(visible.max(1))
        .enumerate()
    {
        let actual = skip + index;
        let marker = if index == 0 { prefix } else { "  " };
        let width = area.width.saturating_sub(marker.width() as u16) as usize;
        let text = if actual == cursor_line {
            cursor_window(line, cursor_col, width)
        } else {
            line.clone()
        };
        lines.push(Line::raw(format!("{marker}{text}")));
    }
    if lines.is_empty() {
        lines.push(Line::raw(prefix));
    }
    frame.render_widget(Paragraph::new(lines), area);

    let marker_width = if cursor_line == 0 {
        prefix.width()
    } else {
        "  ".width()
    };
    let width = area.width.saturating_sub(marker_width as u16) as usize;
    let cursor_y = area.y + cursor_line.saturating_sub(skip) as u16;
    let cursor_x = area.x
        + marker_width as u16
        + cursor_offset(
            app.composer
                .lines
                .get(cursor_line)
                .map(String::as_str)
                .unwrap_or_default(),
            cursor_col,
            width,
        ) as u16;
    frame.set_cursor_position((cursor_x, cursor_y));
}

/// The minimal composer: one line, with the target it is aimed at.
fn draw_composer_line(frame: &mut Frame, app: &App, area: Rect) {
    if area.is_empty() {
        return;
    }
    let prompt = composer_prefix(app);
    let (line, col) = app.composer.cursor;
    let text = app.composer.lines.get(line).cloned().unwrap_or_default();
    let width = area.width.saturating_sub(prompt.width() as u16) as usize;
    frame.render_widget(
        Paragraph::new(format!("{prompt}{}", cursor_window(&text, col, width))),
        area,
    );
    frame.set_cursor_position((
        area.x + prompt.width() as u16 + cursor_offset(&text, col, width) as u16,
        area.y,
    ));
}

fn status_detail(app: &App) -> String {
    let connection = conn_word(app.conn);
    if app.status == connection
        || app.status.starts_with(&format!("{connection} "))
        || app.status.starts_with(&format!("{connection}:"))
    {
        app.status.clone()
    } else {
        format!("{connection} | {}", app.status)
    }
}

fn draw_status(frame: &mut Frame, app: &App, area: Rect, wide: bool) {
    let mode = match app.mode {
        Mode::Navigation => "nav",
        Mode::Composer if wide => "composer",
        Mode::Composer => "compose",
    };
    let detail = status_detail(app);
    let status = if wide && area.width >= 100 {
        format!("status: {detail} | mode: {mode} | ?=help")
    } else {
        let prefix = format!("{mode} · ");
        let remaining = area.width.saturating_sub(prefix.width() as u16) as usize;
        format!("{prefix}{}", clip_with_ellipsis(&detail, remaining))
    };
    frame.render_widget(
        Paragraph::new(Line::styled(status.clone(), status_style(&status))),
        area,
    );
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

/// The conversation switcher: every accessible, known conversation, the Inbox
/// filter as tabs, and a local name query. It takes the whole terminal at
/// every layout: it is the only conversation list the client draws, and the
/// timeline behind it keeps its width. Opening it, previewing it, and closing
/// it never advance a read marker.
fn draw_switcher(frame: &mut Frame, app: &App, now: u64, area: Rect) {
    let width = list_row_width(area);
    let view = app.switcher_view();
    let rows = switcher_rows(app, &view);
    let items = list_items(app, &rows, width, now);
    let inner = area.width.saturating_sub(2);
    // Each hint fits the block's inner width, so the whole action stays
    // readable instead of being cut at the border.
    let hint = if inner >= 58 {
        "j/k move · type to filter · enter open · esc back · ? help"
    } else if inner >= 46 {
        "j/k move · type filter · enter open · esc back"
    } else if inner >= 35 {
        "type filter · enter open · esc back"
    } else if inner >= 22 {
        "enter open · esc back"
    } else {
        "esc back"
    };
    let mark = if app.inbox_incomplete() { " ?" } else { "" };
    let title = if app.switcher_editing || !app.switcher_query.is_empty() {
        Line::raw(clip_with_ellipsis(
            &format!(
                "Switch conversation{mark} · name: {}{}",
                app.switcher_query,
                if app.switcher_editing { "_" } else { "" }
            ),
            inner as usize,
        ))
    } else {
        Line::raw(format!("Switch conversation{mark}"))
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_bottom(Line::raw(hint));
    let body = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    let parts = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(body);
    draw_switcher_tabs(frame, app, parts[0]);
    let list = List::new(items)
        .highlight_style(highlight_style())
        .highlight_symbol("> ");
    let mut state = ListState::default();
    state.select(item_index(&rows, app.switcher.as_ref().map(|s| s.cursor)));
    frame.render_stateful_widget(list, parts[1], &mut state);
}

/// The filter tabs. The active one is reversed, so the selected filter reads
/// without color.
fn draw_switcher_tabs(frame: &mut Frame, app: &App, area: Rect) {
    let mut spans = Vec::new();
    for (at, filter) in Filter::CYCLE.iter().enumerate() {
        if at > 0 {
            spans.push(Span::raw("  "));
        }
        let style = if *filter == app.filter {
            highlight_style()
        } else {
            pending_style()
        };
        spans.push(Span::styled(filter.name().to_owned(), style));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The command palette: the actions the timeline already has, with the key
/// that runs each one. It takes the whole terminal, and it holds no state of
/// its own beyond the cursor.
fn draw_palette(frame: &mut Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = Command::ALL
        .iter()
        .map(|command| {
            ListItem::new(Line::from(vec![
                Span::raw(format!("{:<20}", command.name())),
                Span::styled(command.key().to_owned(), pending_style()),
            ]))
        })
        .collect();
    let hint = if area.width >= 34 {
        "j/k move · enter run · esc back"
    } else {
        "enter run  esc back"
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title("Commands")
        .title_bottom(Line::raw(hint));
    let list = List::new(items)
        .block(block)
        .highlight_style(highlight_style())
        .highlight_symbol("> ");
    let mut state = ListState::default();
    state.select(app.palette);
    frame.render_widget(Clear, area);
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_community_picker(frame: &mut Frame, app: &App, area: Rect) {
    let items = app
        .communities
        .iter()
        .map(|community| ListItem::new(format!("{} · {}", community.name, community.relay_url)))
        .collect::<Vec<_>>();
    let title = format!("Community: {}", app.community_name);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_bottom(Line::raw("enter switch · esc close"));
    let list = List::new(items)
        .block(block)
        .highlight_style(highlight_style())
        .highlight_symbol("> ");
    let mut state = ListState::default();
    state.select(app.community_picker);
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

fn detail_field(label: &str, value: &str, width: usize, style: Style) -> Vec<Line<'static>> {
    let prefix = format!("{label} ");
    let value_width = width.saturating_sub(prefix.width()).max(1);
    wrap(value, value_width as u16)
        .into_iter()
        .enumerate()
        .map(|(index, part)| {
            let lead = if index == 0 {
                prefix.clone()
            } else {
                " ".repeat(prefix.width())
            };
            Line::from(vec![
                Span::styled(lead, pending_style()),
                Span::styled(part, style),
            ])
        })
        .collect()
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
    body.extend(detail_field("name", &agent.name, width, Style::default()));
    body.extend(detail_field(
        "key",
        &short_pubkey(&agent.pubkey),
        width,
        Style::default(),
    ));
    body.extend(detail_field(
        "status",
        &agent_status_word(&status),
        width,
        agent_status_style(&status),
    ));
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

fn agent_row(name: &str, status: &AgentStatus, width: usize) -> Vec<Line<'static>> {
    let text = agent_status_word(status);
    let style = agent_status_style(status);
    let budget = width.saturating_sub(text.width() + 1);
    if budget >= AGENT_NAME_MIN {
        let name = clip(name, budget);
        let pad = budget.saturating_sub(name.width()) + 1;
        return vec![Line::from(vec![
            Span::raw(format!("{name}{}", " ".repeat(pad))),
            Span::styled(text, style),
        ])];
    }
    let status_width = width.saturating_sub(2).max(1);
    let mut lines = vec![Line::raw(clip(name, width))];
    lines.extend(
        wrap(&text, status_width as u16)
            .into_iter()
            .map(|line| Line::from(vec![Span::styled(format!("  {line}"), style)])),
    );
    lines
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
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let width = ch.width().unwrap_or(0);
        if used + width > max {
            break;
        }
        out.push(ch);
        used += width;
    }
    out
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
    let mut text = if app.reader.open {
        if mode == LayoutMode::Wide {
            READER_WIDE_HELP.to_owned()
        } else {
            READER_COMPACT_HELP.to_owned()
        }
    } else if app.community_picker.is_some() {
        COMMUNITY_HELP.to_owned()
    } else if app.palette.is_some() {
        PALETTE_HELP.to_owned()
    } else if app.switcher.is_some() {
        if mode == LayoutMode::Wide {
            SWITCHER_WIDE_HELP.to_owned()
        } else {
            SWITCHER_COMPACT_HELP.to_owned()
        }
    } else {
        match (app.thread.open, mode) {
            (true, LayoutMode::Wide) => THREAD_WIDE_HELP.to_owned(),
            (true, _) => THREAD_COMPACT_HELP.to_owned(),
            (false, _) if app.search.open => SEARCH_HELP.to_owned(),
            (false, _) if app.context.open => CONTEXT_HELP.to_owned(),
            (false, LayoutMode::Wide) => WIDE_HELP.to_owned(),
            (false, _) => COMPACT_HELP.to_owned(),
        }
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
  j k up down    move the focused row   1-9 jump
  c              switch conversation   Esc close
  C              community picker      Enter switch
  Ctrl+P         commands
  a              Agents: owned roster, working now
  f              Inbox filter
  g G PgUp PgDn  move the focused row
  v              read full message        Enter  reply
  i              compose new
  r e d          react / edit / delete
  ?              help                  q quit
  Enter send     Alt+Enter newline
  Esc leaves the composer, keeps the text
switcher
  j k move  f Tab filter  type to filter  enter open  esc back
commands
  j k move  enter run  esc back
agents
  j k select  enter detail  enter open channel
  esc back    ? help        a close
community
  j k select  enter switch  esc back
  ? help      q quit
signals
  ● unread   @ unread mention   ? unknown   … typing";

const COMPACT_HELP: &str = "\
i compose  Enter send
j k move row  c switch  C communities
Ctrl+P commands
a agents  f filter
Enter reply  v read message  r react
e edit  d delete
1-9 jump  g G start/end
PgUp/PgDn move ten rows
? help  j k scroll  q quit
Esc/v back  Alt+Enter nl
● unread  @ mention  ? unknown";

const SWITCHER_WIDE_HELP: &str = "\
switch conversation
  j k up down    move the cursor      enter open
  type           filter by name; the first character starts
                 the query
  /              edit the query       esc back
  backspace      delete one character
  f Tab          Inbox filter (All, Unread, For you)
  ?              help                 q quit
  the list holds the accessible conversations this session
  already knows. opening or previewing it never marks
  anything read, and live messages update signals in place
  without moving the cursor.";

const SWITCHER_COMPACT_HELP: &str = "\
switch: j k move  enter open  esc back
type name filter  / edit  f filter
? help  q quit";

const COMMUNITY_HELP: &str = "\
switch community
  j k up down    move the cursor      enter switch
  esc back       close
  ?              help                 q quit
  the list holds the identity's saved profiles. switching
  changes the relay the session reads and writes; the draft
  and any pending write come along.";

const PALETTE_HELP: &str = "\
commands
  j k up down    move the cursor      enter run
  esc back       ? help               q quit
  the palette lists actions the timeline already has: switch
  conversation, search messages, My agents, help, quit.
  it owns no state and duplicates no command.";

const READER_WIDE_HELP: &str = "\
reader
  j k up down    scroll one displayed line
  PgUp PgDn      scroll a page
  g/Home G/End   start/end
  Enter          reply to this message
  Esc/v          return to the origin
  ?              close help                  q quit";

const READER_COMPACT_HELP: &str = "\
reader: j/k scroll  PgUp/PgDn page
g/Home start  G/End end
Enter reply  Esc/v back
? close help  q quit";

const THREAD_WIDE_HELP: &str = "\
thread
  j k up down    move focused row    g G PgUp PgDn ends
  v              read full message
  t              retry failed read
  r e d          react / edit / delete on focused row
  Esc            back to the channel
  ?              help                  q quit
composer
  Enter send     Alt+Enter newline
  Esc leaves composing in one press and keeps its target";

const THREAD_COMPACT_HELP: &str = "\
thread: j k move  v read  g G ends  PgUp/PgDn
r/e/d react/edit/delete  Esc back
? help  q quit
composer: Esc leaves; Enter sends";

const SEARCH_HELP: &str = "\
search
  / edit query    Enter submit or open context
  f filter form   j k move results
  Enter retry after a failure     Esc back to the origin
  ? help          q quit
editing
  printable characters are query text
  Enter submits   Esc restores the applied query
filter form
  j k move        h l adjust the focused control
  s scope         t time range     a pick author
  o choose a conversation
  Enter applies the form and submits
  Esc cancels and restores the applied filters
author picker
  j k move        Enter select     p exact pubkey
  the known-profile list may be incomplete; duplicate
  names stay distinguishable by their short keys
scope and time
  current conversation, all accessible, or one chosen
  conversation; all time, 7 days, 30 days
  filters apply only on submit";

const CONTEXT_HELP: &str = "\
context
  j k move        g G ends         PgUp/PgDn page
  v read message  t thread         Enter reply
  [ older         ] newer          Esc back to results";

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
            if word.width() > width {
                if !current.is_empty() {
                    lines.push(std::mem::take(&mut current));
                }
                let mut rest = word;
                while rest.width() > width {
                    let (chunk, tail) = split_cells(rest, width);
                    lines.push(chunk.to_owned());
                    rest = tail;
                }
                current.push_str(rest);
                continue;
            }
            let candidate_width = current.width() + word.width() + usize::from(!current.is_empty());
            if candidate_width > width && !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(word);
        }
        lines.push(current);
    }
    lines
}
fn help_context(app: &App) -> String {
    if app.reader.open {
        return "Reader".to_owned();
    }
    if app.thread.open {
        return "Thread".to_owned();
    }
    if app.palette.is_some() {
        return "Commands".to_owned();
    }
    if app.switcher.is_some() {
        return format!("Switch conversation / {}", app.filter.name());
    }
    if app.agents.open
        && matches!(app.agents.cursor.level, agents::Level::Detail)
        && let Some(agent) = app.selected_agent()
    {
        return format!("My agents / {}", agent.name);
    }
    if app.agents.open {
        return "My agents".to_owned();
    }
    app.channels
        .get(app.selected)
        .map(|entry| {
            let label = app.label(entry);
            if entry.is_dm() {
                label
            } else {
                format!("#{label}")
            }
        })
        .unwrap_or_else(|| "conversation".to_owned())
}

fn draw_help(frame: &mut Frame, app: &App, area: Rect, mode: LayoutMode) {
    frame.render_widget(Clear, area);
    let width = area.width as usize;
    let fixed = area.height.min(3) as usize;
    let content_rows = area.height as usize - fixed;
    let lines = help_lines(&help_text(app, mode), width);
    let scroll = (app.help_scroll as usize).min(lines.len().saturating_sub(content_rows));
    let mut visible = Vec::with_capacity(area.height as usize);
    visible.push(Line::raw(format!(
        "Help / {}",
        clip_with_ellipsis(&help_context(app), width.saturating_sub(7)),
    )));
    visible.extend(
        lines
            .into_iter()
            .skip(scroll)
            .take(content_rows)
            .map(Line::raw),
    );
    visible.push(Line::raw(clip_with_ellipsis(
        "Esc return · j/k scroll",
        width,
    )));
    visible.push(Line::styled(
        clip_with_ellipsis(&format!("state: {}", app.status), width),
        status_style(&app.status),
    ));
    frame.render_widget(Paragraph::new(visible), area);
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
            assert!(chunk.width() <= 22, "{chunk:?} is wider than 22");
        }
        assert_eq!(wrap(url, 22).concat(), url, "only the wrapping changed");
    }

    #[test]
    fn wrapping_and_clipping_use_terminal_cells_for_wide_text() {
        let text = "甲🙂乙🙂丙";
        let chunks = wrap(text, 8);
        assert!(chunks.iter().all(|chunk| chunk.width() <= 8), "{chunks:?}");
        assert_eq!(chunks.concat(), text);
        assert_eq!(clip(text, 6), "甲🙂乙");
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

    #[test]
    fn timeline_density_uses_a_rule_without_making_it_a_focusable_row() {
        let rows = vec![message_row(0, "first"), message_row(1, "second")];
        let app = chat_app(rows);
        let compact = frame_text(&app, 80, 12);
        assert!(compact.lines().any(|line| line.contains("─")), "{compact}");
        assert!(
            !compact.contains("┌"),
            "the timeline does not draw message boxes: {compact}"
        );

        let roomy = frame_text(&app, 80, 20);
        let separator = roomy
            .lines()
            .find(|line| line.contains("────────"))
            .expect("roomy messages have a separator");
        assert!(separator.starts_with("  ─"), "{separator:?}");
        let matrix_wide = frame_text(&app, 120, 30);
        assert!(
            matrix_wide.lines().any(|line| line.contains("─")),
            "the 120x30 roomy layout keeps the separator: {matrix_wide}"
        );
    }

    #[test]
    fn composing_keeps_roomy_density_and_moves_focus_to_the_composer() {
        let mut app = chat_app(vec![message_row(0, "first"), message_row(1, "second")]);
        app.mode = Mode::Composer;
        app.composer.set_text("draft");
        let text = frame_text(&app, 120, 19);
        assert!(text.lines().any(|line| line.contains("─")), "{text}");
        assert!(
            !text.lines().any(|line| line.contains("> alice")),
            "the timeline focus marker must yield to the composer: {text}"
        );
    }

    #[test]
    fn composing_at_the_floor_names_the_target_without_an_input_box() {
        let mut app = chat_app(vec![message_row(0, "hello")]);
        app.mode = Mode::Composer;
        app.composer.set_text("draft");
        let text = frame_text(&app, 24, 6);
        assert!(text.contains("│ New message"), "{text}");
        assert!(text.contains("> draft"), "{text}");
        assert!(
            !text.contains("┌"),
            "the composer is not a full box: {text}"
        );
    }

    #[test]
    fn help_keeps_context_actions_and_state_at_the_screen_edges() {
        let mut app = chat_app(vec![message_row(0, "hello")]);
        app.help = true;
        let text = frame_text(&app, 24, 6);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].starts_with("Help /"), "{text}");
        assert!(lines[4].contains("Esc return"), "{text}");
        assert!(lines[5].starts_with("state:"), "{text}");
    }

    #[test]
    fn wide_status_keeps_connection_state_with_a_transient_notice() {
        let mut app = chat_app(vec![message_row(0, "hello")]);
        app.status = "read warning".to_owned();
        let text = frame_text(&app, 120, 19);
        assert!(text.contains("status: connected | read warning"), "{text}");
        assert!(text.contains("?=help"), "{text}");
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
    fn the_thread_frame_at_seventy_nine_columns_stays_one_column() {
        let (app, _keys) = thread_app(false);
        let text = frame_text(&app, 79, 12);
        assert!(text.contains("Thread #general"), "the short label: {text}");
        assert!(text.contains("Esc:back"), "the way back survives: {text}");
        assert!(text.contains("Enter:reply"), "the short keys: {text}");
        assert!(text.contains("the reply"), "{text}");
        assert!(
            !text.contains("j/k: move"),
            "the wide key row is not used here: {text}"
        );
    }

    #[test]
    fn the_thread_frame_at_forty_columns_keeps_the_rows_and_the_target() {
        let (mut app, _keys) = thread_app(false);
        let reading = frame_text(&app, 40, 10);
        assert!(reading.contains("Thread #general"), "{reading}");
        assert!(reading.contains("Esc:back"), "{reading}");
        assert!(reading.contains("[root]"), "{reading}");
        assert!(reading.contains("the reply"), "{reading}");
        app.handle(crate::keys::Action::ThreadReplyFocused, 131);
        let composing = frame_text(&app, 40, 10);
        assert!(composing.contains("Reply to "), "{composing}");
        assert!(composing.contains("Enter:send"), "{composing}");
    }

    #[test]
    fn every_thread_status_fits_the_narrowest_frames() {
        for (width, height) in [(40u16, 10u16), (24u16, 6u16)] {
            let (mut app, _keys) = thread_app(false);
            app.thread.partial = true;
            let text = frame_text(&app, width, height);
            assert!(
                text.contains("Partial; limit reached"),
                "{width}x{height} clips the partial status: {text}"
            );
            app.thread.partial = false;
            app.thread.observed_new = 12;
            let text = frame_text(&app, width, height);
            assert!(
                text.contains("12 new · G latest"),
                "{width}x{height} clips the observed-new status: {text}"
            );
            app.thread.observed_new = 0;
            app.conn = ConnState::Reconnecting;
            let text = frame_text(&app, width, height);
            assert!(
                text.contains("reconnecting; stale"),
                "{width}x{height} clips the stale status: {text}"
            );
        }
    }

    #[test]
    fn a_wide_body_wraps_inside_the_thread_frame() {
        let (mut app, _keys) = thread_app(false);
        let root_row = message_row(0, "the root");
        let mut wide = message_row(1, "链接 https://example.test/非常长的路径?q=1 🚀 结束");
        wide.root_id = Some(root_row.event_id.clone());
        wide.parent_id = Some(root_row.event_id.clone());
        wide.author = "字宽".to_owned();
        app.thread.rows = vec![root_row, wide];
        // The focus on the last row is the follow: no flag to keep in step.
        app.thread.focus = 1;
        assert!(app.thread.following());
        for (width, height) in [(80u16, 12u16), (40, 10), (24, 6)] {
            let text = frame_text(&app, width, height);
            // A wide glyph owns two cells, so the buffer carries a blank
            // second cell: compare without the cell padding.
            let dense: String = text.chars().filter(|ch| !ch.is_whitespace()).collect();
            assert!(
                dense.contains("链接"),
                "{width}x{height} keeps the wide body: {text}"
            );
            assert!(
                text.contains("Esc"),
                "{width}x{height} keeps the way back: {text}"
            );
            for line in text.lines() {
                assert!(
                    line.chars().count() <= width as usize,
                    "{width}x{height} overflows a row: {line:?}"
                );
            }
        }
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

    #[test]
    fn observed_new_replies_are_named_with_the_way_to_the_latest() {
        let (mut app, _keys) = thread_app(false);
        app.thread.observed_new = 3;
        let text = frame_text(&app, 80, 12);
        assert!(text.contains("3 new replies · G latest"), "{text}");
        let compact = frame_text(&app, 40, 10);
        assert!(compact.contains("3 new · G latest"), "{compact}");
        app.thread.observed_new = 1;
        let single = frame_text(&app, 80, 12);
        assert!(single.contains("1 new reply · G latest"), "{single}");
    }

    #[test]
    fn reader_preserves_blank_lines_indentation_tabs_and_wide_text() {
        let row = message_row(
            0,
            "  first\tline\n\n链接 https://example.test/very-long-path",
        );
        let lines = reader_lines(&row, 12);
        assert_eq!(lines[0], "  first line");
        assert_eq!(lines[1], "");
        assert!(
            lines.iter().any(|line| line.contains("链接")),
            "CJK text remains in the reader: {lines:?}"
        );
        assert!(
            lines.join("").contains("https://"),
            "URLs remain literal rather than word-normalized: {lines:?}"
        );
    }

    #[test]
    fn reader_uses_fixed_metadata_and_state_rows_at_the_floor_size() {
        let mut app = chat_app(vec![message_row(0, "line one\nline two\nline three")]);
        app.handle(crate::keys::Action::OpenReader, 130);
        let text = frame_text(&app, 24, 6);
        assert!(text.contains("Message"), "{text}");
        assert!(text.contains("alice"), "{text}");
        assert!(text.contains("Enter reply"), "{text}");
        assert!(text.contains("connected"), "{text}");
        assert!(text.contains("line one"), "{text}");
        assert!(text.contains("line two"), "{text}");
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

    /// The roster and read state that make every Inbox answer complete: the
    /// conversation is listed, and its marker sits at the end of its history.
    fn complete_state(app: &mut App) {
        let id = app.channels[0].id;
        app.apply(
            crate::session::ChatEvent::Channels(crate::client::Roster {
                items: vec![crate::client::ChannelInfo {
                    id,
                    name: "general".to_owned(),
                    kind: crate::client::ChannelKind::Channel,
                    participants: Vec::new(),
                    archived: false,
                    hidden: false,
                }],
                complete: true,
            }),
            0,
        );
        app.apply(
            crate::session::ChatEvent::ReadState {
                contexts: std::collections::HashMap::from([(id.to_string(), 100)]),
                complete: true,
            },
            0,
        );
    }

    #[test]
    fn the_switcher_states_its_filter_and_marks_an_incomplete_answer() {
        let mut app = chat_app(vec![message_row(0, "hello")]);
        app.handle(crate::keys::Action::ToggleSwitcher, 0);
        let text = frame_text(&app, 80, 12);
        assert!(text.contains("Switch conversation ?"), "{text}");
        assert!(text.contains("For you"), "the filter tabs: {text}");

        complete_state(&mut app);
        app.filter = crate::app::Filter::Unread;
        let text = frame_text(&app, 80, 12);
        assert!(text.contains("Switch conversation"), "{text}");
        assert!(
            !text.contains("Switch conversation ?"),
            "a complete roster answers for itself: {text}"
        );
        assert!(text.contains("Unread"), "the filter tabs: {text}");
    }

    #[test]
    fn an_empty_view_says_which_kind_of_empty_it_is() {
        let mut app = chat_app(vec![message_row(0, "hello")]);
        app.filter = crate::app::Filter::Unread;
        assert_eq!(app.empty_view(), "Checking...");
        let text = frame_text(&app, 80, 12);
        assert!(
            text.contains("Inbox ?"),
            "an unknown answer says so: {text}"
        );

        complete_state(&mut app);
        assert_eq!(app.empty_view(), "All read");
        let text = frame_text(&app, 80, 12);
        assert!(
            text.contains("Inbox read"),
            "a complete answer is not a question: {text}"
        );
        assert!(!text.contains("Inbox ?"), "{text}");
    }

    #[test]
    fn a_switcher_row_whose_state_is_unknown_says_so_instead_of_showing_nothing() {
        let mut app = chat_app(vec![message_row(0, "hello")]);
        app.apply(
            crate::session::ChatEvent::ReadState {
                contexts: std::collections::HashMap::new(),
                complete: false,
            },
            0,
        );
        app.handle(crate::keys::Action::ToggleSwitcher, 0);
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
        app.handle(crate::keys::Action::ToggleSwitcher, 0);
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
    fn smoke_search_and_context_views_render() {
        let mut app = chat_app(vec![message_row(0, "hello")]);
        app.handle(crate::keys::Action::OpenSearch, 0);
        assert!(app.search.open);
        app.handle(crate::keys::Action::SearchInput('b'), 0);
        app.handle(crate::keys::Action::SearchInput('o'), 0);
        app.handle(crate::keys::Action::SearchSubmit, 0);
        let mut tokens = Vec::new();
        for command in app.take_outbox() {
            if let crate::session::SessionCommand::Search { token, .. } = command {
                tokens.push(token);
            }
        }
        assert_eq!(tokens.len(), 1, "one search command leaves the outbox");
        app.apply(
            crate::session::ChatEvent::Search {
                request: tokens[0],
                events: Vec::new(),
            },
            0,
        );
        let text = frame_text(&app, 80, 12);
        assert!(text.contains("Search · general · all time"), "{text}");
        assert!(text.contains("search: bo"), "{text}");
        app.search.results.push(message_row(0, "hit one"));
        app.search.channels.push(app.channels[0].id);
        let text = frame_text(&app, 40, 10);
        assert!(text.contains("hit one"), "{text}");
        app.handle(crate::keys::Action::SearchSubmit, 0);
        assert!(app.context.open);
        app.context.rows = vec![message_row(1, "around it")];
        let text = frame_text(&app, 80, 12);
        assert!(text.contains("Context / #general"), "{text}");
        assert!(text.contains("around it"), "{text}");
    }

    /// The token of the only outstanding search command.
    fn take_search_token(app: &mut App) -> u64 {
        app.take_outbox()
            .into_iter()
            .find_map(|command| match command {
                crate::session::SessionCommand::Search { token, .. } => Some(token),
                _ => None,
            })
            .expect("a search command")
    }

    fn one_hit_event(id: uuid::Uuid, body: &str, at: u64) -> nostr::Event {
        nostr::EventBuilder::new(nostr::Kind::Custom(9), body)
            .tags(vec![nostr::Tag::parse(["h", &id.to_string()]).unwrap()])
            .custom_created_at(nostr::Timestamp::from(at))
            .sign_with_keys(&nostr::Keys::generate())
            .unwrap()
    }

    #[test]
    fn the_filter_form_replaces_results_and_shows_every_control() {
        let mut app = chat_app(vec![message_row(0, "hello")]);
        app.handle(crate::keys::Action::OpenSearch, 0);
        // Esc leaves the empty query editing; the results surface owns f now.
        app.handle(crate::keys::Action::Dismiss, 0);
        app.handle(crate::keys::Action::SearchFilter, 0);
        assert!(app.search.form);
        let text = frame_text(&app, 80, 12);
        assert!(text.contains("scope"), "{text}");
        assert!(
            text.contains("general"),
            "the current scope is named: {text}"
        );
        assert!(text.contains("any author"), "{text}");
        assert!(text.contains("all time"), "{text}");
        assert!(text.contains("Enter apply"), "{text}");

        // At the floor size the form still names its controls and values.
        let text = frame_text(&app, 24, 6);
        assert!(text.contains("scope"), "{text}");
        assert!(text.contains("general"), "{text}");

        // The form replaces the results while it is open.
        app.search.results.push(message_row(0, "a visible hit"));
        app.search.channels.push(app.channels[0].id);
        let text = frame_text(&app, 80, 12);
        assert!(!text.contains("a visible hit"), "{text}");
    }

    #[test]
    fn the_author_switcher_says_its_list_may_be_incomplete() {
        let mut app = chat_app(vec![message_row(0, "hello")]);
        app.handle(crate::keys::Action::OpenSearch, 0);
        app.handle(crate::keys::Action::Dismiss, 0);
        app.handle(crate::keys::Action::SearchFilter, 0);
        app.handle(crate::keys::Action::SearchInput('a'), 0);
        let text = frame_text(&app, 80, 12);
        assert!(text.contains("incomplete"), "{text}");
        assert!(text.contains("pubkey"), "{text}");
    }

    #[test]
    fn a_failed_retry_names_the_previous_query_and_keeps_its_results() {
        let mut app = chat_app(vec![message_row(0, "hello")]);
        app.handle(crate::keys::Action::OpenSearch, 0);
        app.handle(crate::keys::Action::SearchInput('b'), 0);
        app.handle(crate::keys::Action::SearchInput('o'), 0);
        app.handle(crate::keys::Action::SearchSubmit, 0);
        let token = take_search_token(&mut app);
        let id = app.channels[0].id;
        app.apply(
            crate::session::ChatEvent::Search {
                request: token,
                events: vec![one_hit_event(id, "the kept hit", 100)],
            },
            0,
        );
        assert_eq!(app.search.results.len(), 1);

        app.handle(crate::keys::Action::SearchEdit, 0);
        app.handle(crate::keys::Action::SearchSubmit, 0);
        let retry = take_search_token(&mut app);
        app.apply(
            crate::session::ChatEvent::SearchFailed {
                request: retry,
                reason: "relay 500".into(),
            },
            0,
        );
        let text = frame_text(&app, 80, 12);
        assert!(
            text.contains("the kept hit"),
            "the previous results stay visible: {text}"
        );
        assert!(text.contains("previous query"), "{text}");
        assert!(text.contains("relay 500"), "{text}");
    }

    #[test]
    fn result_counts_say_returned_and_name_the_50_bound() {
        let mut app = chat_app(vec![message_row(0, "hello")]);
        app.handle(crate::keys::Action::OpenSearch, 0);
        app.handle(crate::keys::Action::SearchInput('b'), 0);
        app.handle(crate::keys::Action::SearchSubmit, 0);
        let token = take_search_token(&mut app);
        let id = app.channels[0].id;
        let events: Vec<nostr::Event> = (0..50)
            .map(|n| one_hit_event(id, &format!("hit {n}"), 100 + n))
            .collect();
        app.apply(
            crate::session::ChatEvent::Search {
                request: token,
                events,
            },
            0,
        );
        let text = frame_text(&app, 80, 12);
        assert!(text.contains("Top 50; narrow filters"), "{text}");

        // Hidden hits make the visible list bounded, never exhaustive.
        app.search.hidden = 3;
        let text = frame_text(&app, 80, 12);
        assert!(text.contains("bounded"), "{text}");
        assert!(text.contains("3 hidden by visibility"), "{text}");

        // A shorter page counts what came back without claiming totality.
        let mut app = chat_app(vec![message_row(0, "hello")]);
        app.handle(crate::keys::Action::OpenSearch, 0);
        app.handle(crate::keys::Action::SearchInput('b'), 0);
        app.handle(crate::keys::Action::SearchSubmit, 0);
        let token = take_search_token(&mut app);
        let id = app.channels[0].id;
        app.apply(
            crate::session::ChatEvent::Search {
                request: token,
                events: vec![one_hit_event(id, "only one", 100)],
            },
            0,
        );
        let text = frame_text(&app, 80, 12);
        assert!(text.contains("1 results returned"), "{text}");
        assert!(!text.contains("Top 50"), "{text}");
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
        app.switcher = Some(crate::app::Switcher { cursor: id, at: 1 });

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
    fn the_switcher_hint_keeps_its_actions_at_the_floor_size() {
        let mut app = chat_app(vec![message_row(0, "hello world")]);
        app.switcher = Some(crate::app::Switcher {
            cursor: app.channels[0].id,
            at: 0,
        });
        for (columns, rows, expected) in [
            (24, 6, "enter open · esc back"),
            (40, 10, "type filter · enter open · esc back"),
            (
                80,
                12,
                "j/k move · type to filter · enter open · esc back · ? help",
            ),
        ] {
            let text = frame_text(&app, columns, rows);
            let line = text
                .lines()
                .find(|line| line.contains("enter open"))
                .expect("the switcher hint renders");
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
        // 18 columns is a narrow row, one short name and its signal.
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
        assert!(!text.contains("i compose"), "reading has no empty editor");

        // The list is the switcher over the timeline: the rows are the same
        // ones, and the typing signal belongs to the conversation it is in.
        app.handle(crate::keys::Action::ToggleSwitcher, 0);
        let list = frame_text(&app, 80, 12);
        let active_row = list
            .lines()
            .find(|line| line.contains("general"))
            .expect("the active conversation is listed");
        assert!(active_row.contains('…'), "{active_row:?}");
        let quiet_row = list
            .lines()
            .find(|line| line.contains("quiet"))
            .expect("the quiet conversation is listed");
        assert!(
            !quiet_row.contains('…'),
            "a quiet channel stays plain: {quiet_row:?}"
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
        assert!(
            !nav.contains("@agent-a typing…"),
            "the minimum reading view omits optional typing to preserve content: {nav}"
        );

        app.mode = Mode::Composer;
        app.composer.set_text(draft);

        let narrow = frame_text(&app, 40, 10);
        assert!(narrow.contains("@agent-a typing…"), "{narrow}");
        assert!(
            narrow.contains("New message"),
            "the composer names its target"
        );
        assert!(narrow.contains(draft), "{narrow}");

        let minimal = frame_text(&app, 24, 6);
        // 24x6 composing is exactly header, context, target, input, keys,
        // and state; optional channel typing is omitted.
        assert!(!minimal.contains("@agent-a typing…"), "{minimal}");
        assert!(minimal.contains("New message"), "{minimal}");
        assert!(
            minimal.contains(&format!("> {draft}")),
            "the composer prompt survives: {minimal}"
        );
        assert!(
            minimal.contains("> "),
            "the focused message header survives: {minimal}"
        );
    }

    #[test]
    fn the_switcher_marks_the_channels_someone_is_composing_in() {
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
        app.switcher = Some(crate::app::Switcher {
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

    #[test]
    fn a_narrow_mentioned_header_reserves_the_semantic_label() {
        let mut row = message_row(0, "hi");
        row.author = "Alexandra_Responsible_For_Customer_Support_International".to_owned();
        row.mentions_me = true;
        let header = row_lines(&row, 110, 22, true, RootMark::None)[0].to_string();
        assert!(header.width() <= 22, "{header:?}");
        assert!(header.contains("@you"), "{header:?}");
    }

    #[test]
    fn narrow_headers_bound_optional_metadata_before_state_words() {
        let mut row = message_row(0, "hi");
        row.author = "Alexandra_Responsible_For_Customer_Support_International".to_owned();
        row.mentions_me = true;
        row.pending = true;
        row.uncertain = true;
        row.parent_id = Some("root".to_owned());
        row.broadcast = true;
        row.edited = true;
        let header = row_lines(&row, 110, 38, false, RootMark::Root)[0].to_string();
        assert!(header.width() <= 38, "{header:?}");
        assert!(header.contains("Unconfirmed"), "{header:?}");
        assert!(header.contains("@you"), "{header:?}");
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
    fn narrow_agent_views_keep_the_complete_quiet_state() {
        let app = agent_app(true, &[("Review Agent", "aa")]);
        let list = frame_text(&app, 24, 6);
        assert!(list.contains("No active turn"), "{list}");
        assert!(list.contains("observed"), "{list}");

        let mut detail_app = app;
        detail_app.agents.cursor.level = crate::agents::Level::Detail;
        let detail = frame_text(&detail_app, 24, 6);
        assert!(detail.contains("No active turn"), "{detail}");
        assert!(detail.contains("observed"), "{detail}");
    }

    #[test]
    fn agent_detail_help_keeps_the_selected_agent_in_context() {
        let mut app = agent_app(true, &[("Review Agent", "aa")]);
        app.agents.cursor.level = crate::agents::Level::Detail;
        app.help = true;
        let text = frame_text(&app, 40, 10);
        assert!(text.contains("Help / My agents / Review Agent"), "{text}");
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
