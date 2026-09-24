//! Rendering only. `ui.rs` reads `App` state and draws; it never mutates it
//! and never touches the network.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

use crate::app::{App, ConnState, Mode};

/// The smallest terminal the interface is designed for.
pub const MIN_WIDTH: u16 = 80;
pub const MIN_HEIGHT: u16 = 12;

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
/// and reactions.
fn row_lines(row: &crate::content::Row, now: u64, width: u16) -> Vec<Line<'static>> {
    let mut header = vec![
        Span::styled(row.author.clone(), author_style()),
        Span::raw(" "),
        Span::raw(age(row.created_at, now)),
    ];
    if row.pending {
        header.push(Span::styled(" ...".to_owned(), pending_style()));
    }
    if row.uncertain {
        header.push(Span::styled(" ?".to_owned(), mention_style()));
    }
    if row.parent_id.is_some() {
        header.push(Span::raw(" (reply)"));
    }
    if row.broadcast {
        header.push(Span::raw(" @channel"));
    }
    if row.mentions_me {
        header.push(Span::styled(" @you".to_owned(), mention_style()));
    }
    if row.edited {
        header.push(Span::raw(" (edited)"));
    }
    let mut lines = vec![Line::from(header)];
    if row.mentions_me {
        lines[0] = Line::from(vec![
            Span::styled("*".to_owned(), mention_style()),
            lines[0].spans.remove(0),
        ]);
    }

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
            .map(|(emoji, count)| {
                if *count > 1 {
                    format!("{emoji} x{count}")
                } else {
                    emoji.clone()
                }
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

/// Draw the whole interface for one frame.
pub fn draw(frame: &mut Frame, app: &App, now: u64) {
    let area = frame.area();
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        let notice = format!(
            "buzzx needs at least {} columns by {} rows; this terminal is {} by {}",
            MIN_WIDTH, MIN_HEIGHT, area.width, area.height
        );
        let block = Block::default().borders(Borders::ALL);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        frame.render_widget(Paragraph::new(notice), inner);
        return;
    }

    let outer = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(area);
    let main = Layout::horizontal([Constraint::Length(22), Constraint::Min(40)]).split(outer[0]);

    draw_channels(frame, app, main[0]);
    draw_timeline(frame, app, main[1], now);
    draw_status(frame, app, outer[1]);
    if app.help {
        draw_help(frame, area);
    }
}

fn draw_channels(frame: &mut Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = app
        .channels
        .iter()
        .enumerate()
        .map(|(index, channel)| {
            let number = (index + 1).min(9);
            ListItem::new(Line::raw(format!("{number} {}", channel.name)))
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

fn draw_timeline(frame: &mut Frame, app: &App, area: Rect, now: u64) {
    let header_height = 1u16;
    let composer_height = 3u16;
    let column = Layout::vertical([
        Constraint::Length(header_height),
        Constraint::Min(1),
        Constraint::Length(composer_height),
    ])
    .split(area);

    let selected_name = app
        .channels
        .get(app.selected)
        .map(|c| c.name.as_str())
        .unwrap_or("no channel");
    let header_text = if app
        .channels
        .get(app.selected)
        .map(|e| e.loading)
        .unwrap_or(false)
    {
        format!("#{selected_name} loading")
    } else {
        format!("#{selected_name}")
    };
    frame.render_widget(Paragraph::new(header_text), column[0]);

    let body_width = column[1].width.saturating_sub(2);
    let items: Vec<ListItem> = app
        .channels
        .get(app.selected)
        .map(|e| e.rows.as_slice())
        .unwrap_or(&[])
        .iter()
        .map(|row| ListItem::new(row_lines(row, now, body_width)))
        .collect();
    let has_rows = !items.is_empty();
    let list = List::new(items)
        .highlight_style(highlight_style())
        .highlight_symbol("> ");
    let mut state = ListState::default();
    if has_rows {
        state.select(Some(app.focus));
    }
    frame.render_stateful_widget(list, column[1], &mut state);

    draw_composer(frame, app, column[2]);
}

fn draw_composer(frame: &mut Frame, app: &App, area: Rect) {
    let hint = if app.composer.edit.is_some() {
        "editing your message - enter to save, esc to cancel".to_owned()
    } else if let Some(reply) = &app.composer.reply {
        format!("reply to {} - enter to send, esc to clear", reply.author)
    } else if app.mode == Mode::Composer {
        "new message - enter to send, alt+enter newline".to_owned()
    } else {
        "i compose - enter reply".to_owned()
    };
    let block = Block::default().borders(Borders::ALL).title(hint);
    let inner = block.inner(area);
    frame.render_widget(block, area);
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

fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
    let conn = match app.conn {
        ConnState::Connecting => "connecting",
        ConnState::Connected => "connected",
        ConnState::Reconnecting => "reconnecting",
    };
    let mode = match app.mode {
        Mode::Navigation => "nav",
        Mode::Composer => "composer",
    };
    let status = format!(
        "status: {conn} {} | {} | mode: {mode} | ?=help",
        app.relay_label, app.status
    );
    frame.render_widget(Paragraph::new(status), area);
}

fn draw_help(frame: &mut Frame, area: Rect) {
    let text = "\
navigation
  j k up down   switch channel     1-9 jump
  g G PgUp PgDn move the focused row
  i Tab         compose new         Enter  reply to focused row
  r             react (press again to remove)
  e             edit your focused row
  d             delete your focused row
  ?             toggle this help    q quit
composer
  Enter send    Alt+Enter newline   Esc clear target, then leave";
    let width = 56u16;
    let height = 14u16;
    let left = area.width.saturating_sub(width) / 2;
    let top = area.height.saturating_sub(height) / 2;
    let popup = Rect {
        x: left,
        y: top,
        width: width.min(area.width),
        height: height.min(area.height),
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
}
