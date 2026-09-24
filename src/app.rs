//! The TUI state machine: channels, rows, composer, focus, and the pending
//! lifecycle. No I/O happens here. Time arrives as an argument; commands
//! leave through the outbox for the session.

use std::collections::{HashMap, HashSet};

use nostr::Keys;
use uuid::Uuid;

use crate::content::{self, Row};
use crate::keys::{Action, PAGE_ROWS};
use crate::session::{ChannelInfo, ChatEvent, SessionCommand};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Navigation,
    Composer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnState {
    Connecting,
    Connected,
    Reconnecting,
}

/// A reply the composer is aimed at.
#[derive(Debug, Clone, PartialEq)]
pub struct ReplyTarget {
    pub event_id: String,
    pub author: String,
}

/// An edit the composer is aimed at.
#[derive(Debug, Clone, PartialEq)]
pub struct EditTarget {
    pub event_id: String,
    pub channel: Uuid,
}

/// The composer buffer: lines, a cursor, and at most one target.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Composer {
    pub lines: Vec<String>,
    /// (line, byte-column within the line).
    pub cursor: (usize, usize),
    pub reply: Option<ReplyTarget>,
    pub edit: Option<EditTarget>,
}

impl Composer {
    pub fn new() -> Self {
        Self {
            lines: vec![String::new()],
            cursor: (0, 0),
            reply: None,
            edit: None,
        }
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn set_text(&mut self, text: &str) {
        self.lines = if text.is_empty() {
            vec![String::new()]
        } else {
            text.split('\n').map(str::to_owned).collect()
        };
        self.cursor = (
            self.lines.len() - 1,
            self.lines.last().map(String::len).unwrap_or(0),
        );
    }

    pub fn input(&mut self, c: char) {
        let (line, col) = self.cursor;
        self.lines[line].insert(col, c);
        self.cursor = (line, col + c.len_utf8());
    }

    pub fn backspace(&mut self) {
        let (line, col) = self.cursor;
        if col > 0 {
            let byte = self.lines[line][..col]
                .char_indices()
                .rev()
                .map(|(i, _)| i)
                .next()
                .unwrap_or(0);
            self.lines[line].replace_range(byte..col, "");
            self.cursor = (line, byte);
        } else if line > 0 {
            let merged = self.lines.remove(line);
            let end = self.lines[line - 1].len();
            self.lines[line - 1].push_str(&merged);
            self.cursor = (line - 1, end);
        }
    }

    pub fn newline(&mut self) {
        let (line, col) = self.cursor;
        let rest = self.lines[line].split_off(col);
        self.lines.insert(line + 1, rest);
        self.cursor = (line + 1, 0);
    }

    pub fn left(&mut self) {
        let (line, col) = self.cursor;
        if col > 0 {
            let byte = self.lines[line][..col]
                .char_indices()
                .rev()
                .map(|(i, _)| i)
                .next()
                .unwrap_or(0);
            self.cursor = (line, byte);
        } else if line > 0 {
            let end = self.lines[line - 1].len();
            self.cursor = (line - 1, end);
        }
    }

    pub fn right(&mut self) {
        let (line, col) = self.cursor;
        if col < self.lines[line].len() {
            let offset = self.lines[line][col..]
                .chars()
                .next()
                .map(char::len_utf8)
                .unwrap_or(1);
            self.cursor = (line, col + offset);
        } else if line + 1 < self.lines.len() {
            self.cursor = (line + 1, 0);
        }
    }

    pub fn up(&mut self) {
        if self.cursor.0 > 0 {
            let line = self.cursor.0 - 1;
            self.cursor = (line, self.lines[line].len().min(self.cursor.1));
        }
    }

    pub fn down(&mut self) {
        if self.cursor.0 + 1 < self.lines.len() {
            let line = self.cursor.0 + 1;
            self.cursor = (line, self.lines[line].len().min(self.cursor.1));
        }
    }

    pub fn home(&mut self) {
        self.cursor.1 = 0;
    }

    pub fn end(&mut self) {
        self.cursor.1 = self.lines[self.cursor.0].len();
    }

    pub fn clear(&mut self) {
        self.lines = vec![String::new()];
        self.cursor = (0, 0);
        self.reply = None;
        self.edit = None;
    }
}

/// One channel with its loaded rows.
#[derive(Debug, Clone)]
pub struct ChannelEntry {
    pub id: Uuid,
    pub name: String,
    pub rows: Vec<Row>,
    pub seen: HashSet<String>,
    pub loading: bool,
}

/// What a local pending id stands for, so a write result can be undone or
/// completed.
#[derive(Debug, Clone)]
enum PendingOp {
    Send {
        channel: Uuid,
        draft: String,
        reply: Option<ReplyTarget>,
    },
    Edit {
        row_id: String,
        old_body: String,
    },
    Delete {
        channel: Uuid,
        row: Row,
    },
    React {
        row_id: String,
        emoji: String,
    },
}

pub struct App {
    pub me: String,
    pub relay_label: String,
    pub channels: Vec<ChannelEntry>,
    pub selected: usize,
    /// The focused row of the selected channel. It is also the scroll
    /// position: the viewport keeps the focused row visible.
    pub focus: usize,
    pub mode: Mode,
    pub help: bool,
    pub conn: ConnState,
    pub status: String,
    pub composer: Composer,
    pub quit: bool,
    pub exit_code: i32,
    profiles: HashMap<String, String>,
    seen_aux: HashSet<String>,
    reaction_of: HashMap<String, (String, String)>,
    my_reaction: HashMap<(String, String), String>,
    pending: HashMap<String, PendingOp>,
    opened_once: bool,
    outbox: Vec<SessionCommand>,
}

impl App {
    pub fn new(keys: &Keys, relay_label: &str) -> Self {
        Self {
            me: keys.public_key().to_hex(),
            relay_label: relay_label.to_owned(),
            channels: Vec::new(),
            selected: 0,
            focus: 0,
            mode: Mode::Navigation,
            help: false,
            conn: ConnState::Connecting,
            status: "connecting".to_owned(),
            composer: Composer::new(),
            quit: false,
            exit_code: 0,
            profiles: HashMap::new(),
            seen_aux: HashSet::new(),
            reaction_of: HashMap::new(),
            my_reaction: HashMap::new(),
            pending: HashMap::new(),
            opened_once: false,
            outbox: Vec::new(),
        }
    }

    pub fn take_outbox(&mut self) -> Vec<SessionCommand> {
        std::mem::take(&mut self.outbox)
    }

    fn entry_mut(&mut self, id: &Uuid) -> Option<&mut ChannelEntry> {
        self.channels.iter_mut().find(|c| &c.id == id)
    }

    fn selected_entry(&self) -> Option<&ChannelEntry> {
        self.channels.get(self.selected)
    }

    fn selected_rows(&self) -> &[Row] {
        self.selected_entry()
            .map(|e| e.rows.as_slice())
            .unwrap_or(&[])
    }

    fn focused_row(&self) -> Option<&Row> {
        self.selected_rows().get(self.focus)
    }

    fn at_bottom(&self) -> bool {
        let len = self.selected_rows().len();
        len == 0 || self.focus == len - 1
    }

    fn set_focus(&mut self, index: usize) {
        let len = self.selected_rows().len();
        self.focus = len.saturating_sub(1).min(index);
    }

    fn refresh_authors(&mut self) {
        let profiles = self.profiles.clone();
        let me = self.me.clone();
        for entry in &mut self.channels {
            for row in &mut entry.rows {
                row.author = author_name(&profiles, &me, &row.pubkey);
            }
        }
    }

    fn missing_profiles(&self) -> Vec<String> {
        let me = self.me.clone();
        self.channels
            .iter()
            .flat_map(|e| e.rows.iter())
            .map(|r| r.pubkey.clone())
            .filter(|pubkey| pubkey != &me && !self.profiles.contains_key(pubkey))
            .collect::<HashSet<String>>()
            .into_iter()
            .collect()
    }

    fn request_profiles(&mut self) {
        let missing = self.missing_profiles();
        if !missing.is_empty() {
            self.outbox.push(SessionCommand::LoadProfiles(missing));
        }
    }

    /// Apply one transport event. `now` is only used to order rows.
    pub fn apply(&mut self, event: ChatEvent, now: u64) {
        match event {
            ChatEvent::Connected => {
                self.conn = ConnState::Connected;
                self.status = format!("connected {}", self.relay_label);
                if !self.opened_once {
                    self.outbox.push(SessionCommand::LoadChannels);
                } else if let Some(entry) = self.selected_entry() {
                    // A reconnect is a full resubscribe: refresh membership
                    // and replace the selected channel's history so the gap
                    // while offline heals.
                    let id = entry.id;
                    self.outbox.push(SessionCommand::LoadChannels);
                    self.outbox.push(SessionCommand::OpenChannel(id));
                }
            }
            ChatEvent::Disconnected(reason) => {
                self.conn = ConnState::Reconnecting;
                self.status = format!("reconnecting: {reason}");
            }
            ChatEvent::Channels(channels) => {
                self.merge_channels(channels);
                if !self.opened_once && !self.channels.is_empty() {
                    self.opened_once = true;
                    self.selected = 0;
                    self.outbox
                        .push(SessionCommand::OpenChannel(self.channels[0].id));
                }
            }
            ChatEvent::History { channel, events } => {
                self.load_history(channel, events, now);
            }
            ChatEvent::Profiles(resolved) => {
                for (pubkey, name) in resolved {
                    self.profiles.insert(pubkey, name);
                }
                self.refresh_authors();
            }
            ChatEvent::Timeline { channel, event } => {
                self.apply_timeline(channel, event, now);
            }
            ChatEvent::Overlay(event) => self.apply_overlay(event),
            ChatEvent::ChannelGone { channel, reason } => {
                self.channels.retain(|c| c.id != channel);
                if self.selected >= self.channels.len() {
                    self.selected = self.channels.len().saturating_sub(1);
                }
                self.set_focus(self.focus);
                self.status = format!("channel closed: {reason}");
                if let Some(entry) = self.channels.get(self.selected) {
                    let id = entry.id;
                    self.outbox.push(SessionCommand::OpenChannel(id));
                }
            }
            ChatEvent::Status(message) => self.status = message,
            ChatEvent::WriteOk { local, event_id } => self.complete_write(local, event_id),
            ChatEvent::WriteFailed { local, reason } => self.fail_write(local, reason),
            ChatEvent::WriteUncertain { local, reason } => self.uncertain_write(local, reason),
        }
    }

    fn merge_channels(&mut self, channels: Vec<ChannelInfo>) {
        let mut merged: Vec<ChannelEntry> = Vec::with_capacity(channels.len());
        for info in channels {
            let existing = self.channels.iter().find(|c| c.id == info.id);
            let entry = match existing {
                Some(old) => ChannelEntry {
                    id: old.id,
                    name: info.name,
                    rows: old.rows.clone(),
                    seen: old.seen.clone(),
                    loading: old.loading,
                },
                None => ChannelEntry {
                    id: info.id,
                    name: info.name,
                    rows: Vec::new(),
                    seen: HashSet::new(),
                    loading: true,
                },
            };
            merged.push(entry);
        }
        self.channels = merged;
        if self.selected >= self.channels.len() {
            self.selected = self.channels.len().saturating_sub(1);
            self.set_focus(self.focus);
        }
    }

    fn load_history(&mut self, channel: Uuid, events: Vec<nostr::Event>, now: u64) {
        let me = self.me.clone();
        let profiles = self.profiles.clone();
        let Some(entry) = self.entry_mut(&channel) else {
            return;
        };
        let pending: Vec<Row> = entry.rows.iter().filter(|r| r.pending).cloned().collect();
        let mut rows: Vec<Row> = events
            .iter()
            .map(|e| content::row_from_event(e, &me))
            .collect();
        // The relay returns events in a stable order; the client keeps it,
        // sorted only by timestamp for the oldest-first view.
        rows.sort_by_key(|r| r.created_at);
        for row in &mut rows {
            row.author = author_name(&profiles, &me, &row.pubkey);
        }
        rows.extend(pending);
        entry.seen = rows
            .iter()
            .filter(|r| !r.pending)
            .map(|r| r.event_id.clone())
            .collect();
        entry.rows = rows;
        entry.loading = false;
        if self.selected_entry().map(|e| e.id) == Some(channel) {
            self.set_focus(usize::MAX);
        }
        self.request_profiles();
        let _ = now;
    }

    fn apply_timeline(&mut self, channel: Uuid, event: nostr::Event, _now: u64) {
        let id = event.id.to_hex();
        let me = self.me.clone();
        let known_author =
            self.profiles.contains_key(&event.pubkey.to_hex()) || event.pubkey.to_hex() == me;
        let selected = self.selected_entry().map(|e| e.id) == Some(channel);
        let was_at_bottom = selected && self.at_bottom();
        let Some(entry) = self.entry_mut(&channel) else {
            return;
        };
        if entry.seen.contains(&id) || entry.rows.iter().any(|r| r.event_id == id) {
            return;
        }
        entry.seen.insert(id);
        let row = content::row_from_event(&event, &me);
        entry.rows.push(row);
        if was_at_bottom {
            self.focus = entry.rows.len() - 1;
        }
        if !known_author {
            let author = event.pubkey.to_hex();
            self.outbox.push(SessionCommand::LoadProfiles(vec![author]));
        }
    }

    fn apply_overlay(&mut self, event: nostr::Event) {
        let id = event.id.to_hex();
        if !self.seen_aux.insert(id.clone()) {
            return;
        }
        let reaction_of = self.reaction_of.clone();
        let classified =
            content::overlay_of(&event, &|target: &str| reaction_of.get(target).cloned());
        let Some((target, overlay)) = classified else {
            return;
        };
        match overlay {
            content::Overlay::Reaction { emoji } if emoji.starts_with('-') => {
                let removed = emoji.trim_start_matches('-').to_owned();
                self.apply_to_row(&target, |row| {
                    content::apply_overlay(
                        row,
                        &content::Overlay::Reaction {
                            emoji: format!("-{removed}"),
                        },
                    );
                });
            }
            content::Overlay::Reaction { emoji } => {
                self.reaction_of
                    .insert(id.clone(), (target.clone(), emoji.clone()));
                self.apply_to_row(&target, |row| {
                    content::apply_overlay(
                        row,
                        &content::Overlay::Reaction {
                            emoji: emoji.clone(),
                        },
                    );
                });
            }
            content::Overlay::Edit { body } => {
                self.apply_to_row(&target, |row| {
                    content::apply_overlay(row, &content::Overlay::Edit { body: body.clone() })
                });
            }
            content::Overlay::Delete => {
                self.remove_row(&target);
            }
        }
    }

    fn apply_to_row(&mut self, target: &str, mut apply: impl FnMut(&mut Row)) {
        for entry in &mut self.channels {
            if let Some(row) = entry.rows.iter_mut().find(|r| r.event_id == target) {
                apply(row);
                return;
            }
        }
    }

    fn remove_row(&mut self, target: &str) {
        for index in 0..self.channels.len() {
            if let Some(position) = self.channels[index]
                .rows
                .iter()
                .position(|r| r.event_id == target)
            {
                self.channels[index].rows.remove(position);
                if self.selected == index {
                    self.set_focus(self.focus.min(self.channels[index].rows.len()));
                }
                return;
            }
        }
    }

    /// Handle one key action. `now` stamps optimistic rows.
    pub fn handle(&mut self, action: Action, now: u64) {
        match self.mode {
            Mode::Navigation => self.handle_navigation(action, now),
            Mode::Composer => self.handle_composer(action, now),
        }
    }

    fn handle_navigation(&mut self, action: Action, now: u64) {
        match action {
            Action::Quit => {
                self.quit = true;
            }
            Action::ToggleHelp => self.help = !self.help,
            Action::NextChannel => self.switch_channel(self.selected.saturating_add(1)),
            Action::PrevChannel => self.switch_channel(self.selected.saturating_sub(1)),
            Action::Channel(n) => self.switch_channel(n.saturating_sub(1)),
            Action::Top => self.set_focus(0),
            Action::Bottom => self.set_focus(usize::MAX),
            Action::PageUp => {
                let next = self.focus.saturating_sub(PAGE_ROWS);
                self.set_focus(next);
            }
            Action::PageDown => self.set_focus(self.focus + PAGE_ROWS),
            Action::ComposeNew => {
                self.composer.reply = None;
                self.composer.edit = None;
                self.mode = Mode::Composer;
            }
            Action::ComposeReply => {
                let target = self
                    .focused_row()
                    .map(|row| (row.event_id.clone(), row.author.clone(), row.pending));
                if target.as_ref().map(|(_, _, pending)| *pending) == Some(true) {
                    self.status = "the focused message is still sending".to_owned();
                    return;
                }
                let target = target.map(|(id, author, _)| (id, author));
                self.composer.edit = None;
                match target {
                    Some((event_id, author)) => {
                        self.composer.reply = Some(ReplyTarget { event_id, author });
                    }
                    None => self.composer.reply = None,
                }
                self.mode = Mode::Composer;
            }
            Action::React => self.react_focused(),
            Action::EditRow => self.edit_focused(),
            Action::DeleteRow => self.delete_focused(),
            Action::Ignored => {}
            _ => {}
        }
        let _ = now;
    }

    fn handle_composer(&mut self, action: Action, now: u64) {
        match action {
            Action::Quit => self.quit = true,
            Action::ComposerInput(c) => self.composer.input(c),
            Action::ComposerBackspace => self.composer.backspace(),
            Action::ComposerNewline => self.composer.newline(),
            Action::ComposerCursorUp => self.composer.up(),
            Action::ComposerCursorDown => self.composer.down(),
            Action::ComposerCursorLeft => self.composer.left(),
            Action::ComposerCursorRight => self.composer.right(),
            Action::ComposerHome => self.composer.home(),
            Action::ComposerEnd => self.composer.end(),
            Action::ComposerEscape => {
                // First Esc clears the target; the second leaves the
                // composer with the text kept.
                if self.composer.reply.take().is_none() && self.composer.edit.take().is_none() {
                    self.mode = Mode::Navigation;
                }
            }
            Action::ComposerSend => self.send_composer(now),
            Action::Ignored => {}
            _ => {}
        }
    }

    fn switch_channel(&mut self, index: usize) {
        if self.channels.is_empty() || index >= self.channels.len() || index == self.selected {
            return;
        }
        self.selected = index;
        self.set_focus(usize::MAX);
        let id = self.channels[index].id;
        self.status = format!("opened #{}", self.channels[index].name);
        self.channels[index].loading = true;
        self.outbox.push(SessionCommand::OpenChannel(id));
    }

    fn react_focused(&mut self) {
        let Some(row) = self.focused_row() else {
            return;
        };
        let row_id = row.event_id.clone();
        let emoji = content::DEFAULT_REACTION.to_owned();
        let toggle_key = (row_id.clone(), emoji.clone());
        if let Some(reaction_id) = self.my_reaction.get(&toggle_key).cloned() {
            // Pressing r again removes the identity's own reaction.
            let local = format!("pending:{}", Uuid::new_v4());
            self.pending.insert(
                local.clone(),
                PendingOp::React {
                    row_id: row_id.clone(),
                    emoji: emoji.clone(),
                },
            );
            content::apply_overlay(
                self.row_mut(&row_id).expect("the focused row exists"),
                &content::Overlay::Reaction {
                    emoji: format!("-{emoji}"),
                },
            );
            self.my_reaction.remove(&toggle_key);
            self.outbox.push(SessionCommand::React {
                target: row_id,
                emoji,
                remove: Some(reaction_id),
                local,
            });
            return;
        }
        let local = format!("pending:{}", Uuid::new_v4());
        self.pending.insert(
            local.clone(),
            PendingOp::React {
                row_id: row_id.clone(),
                emoji: emoji.clone(),
            },
        );
        if let Some(row) = self.row_mut(&row_id) {
            content::apply_overlay(
                row,
                &content::Overlay::Reaction {
                    emoji: emoji.clone(),
                },
            );
        }
        self.outbox.push(SessionCommand::React {
            target: row_id,
            emoji,
            remove: None,
            local,
        });
    }

    fn row_mut(&mut self, id: &str) -> Option<&mut Row> {
        self.channels
            .iter_mut()
            .flat_map(|e| e.rows.iter_mut())
            .find(|r| r.event_id == id)
    }

    fn edit_focused(&mut self) {
        let Some(row) = self.focused_row().filter(|row| row.pubkey == self.me) else {
            if let Some(row) = self.focused_row() {
                self.status = if row.pending {
                    "the focused message is still sending".to_owned()
                } else {
                    "only your own messages can be edited".to_owned()
                };
            }
            return;
        };
        let Some(channel_id) = self.selected_entry().map(|e| e.id) else {
            return;
        };
        let body = row.body.clone();
        let id = row.event_id.clone();
        self.composer.reply = None;
        self.composer.set_text(&body);
        self.composer.edit = Some(EditTarget {
            event_id: id,
            channel: channel_id,
        });
        self.mode = Mode::Composer;
    }

    fn delete_focused(&mut self) {
        let own = self
            .focused_row()
            .map(|row| (row.pubkey == self.me, row.event_id.clone(), row.pending));
        let Some((true, id, false)) = own else {
            if let Some((_, _, pending)) = own {
                self.status = if pending {
                    "the focused message is still sending".to_owned()
                } else {
                    "only your own messages can be deleted".to_owned()
                };
            }
            return;
        };
        let Some(channel_id) = self.selected_entry().map(|e| e.id) else {
            return;
        };
        let local = format!("pending:{}", Uuid::new_v4());
        let removed = self
            .channels
            .get_mut(self.selected)
            .and_then(|e| e.rows.iter().position(|r| r.event_id == id))
            .map(|position| self.channels[self.selected].rows.remove(position));
        let Some(removed) = removed else { return };
        self.set_focus(self.focus);
        self.pending.insert(
            local.clone(),
            PendingOp::Delete {
                channel: channel_id,
                row: removed,
            },
        );
        self.outbox.push(SessionCommand::Delete {
            channel: channel_id,
            target: id,
            local,
        });
    }

    fn send_composer(&mut self, now: u64) {
        let text = self.composer.text();
        if text.trim().is_empty() {
            self.status = "nothing to send".to_owned();
            return;
        }
        let Some(channel) = self.selected_entry() else {
            return;
        };
        let channel_id = channel.id;
        let local = format!("pending:{}", Uuid::new_v4());
        if let Some(edit) = self.composer.edit.clone() {
            let row_id = edit.event_id.clone();
            let old_body = self
                .row_mut(&row_id)
                .map(|row| std::mem::replace(&mut row.body, text.clone()))
                .unwrap_or_default();
            self.pending.insert(
                local.clone(),
                PendingOp::Edit {
                    row_id: row_id.clone(),
                    old_body,
                },
            );
            self.outbox.push(SessionCommand::Edit {
                channel: channel_id,
                target: row_id,
                content: text,
                local,
            });
            self.composer.clear();
            self.mode = Mode::Navigation;
            return;
        }

        // The pending row mirrors the final tag shape so overlays behave.
        let thread = self.composer.reply.as_ref().and_then(|reply| {
            let target = self
                .channels
                .iter()
                .flat_map(|e| e.rows.iter())
                .find(|r| r.event_id == reply.event_id)?;
            let root = target
                .root_id
                .clone()
                .unwrap_or_else(|| target.event_id.clone());
            Some((root, target.event_id.clone()))
        });
        let mentions_me = false;
        let row = content::pending_row(&self.me, &local, &text, &thread, mentions_me, now);
        let draft = text.clone();
        let reply = self.composer.reply.clone();
        self.pending.insert(
            local.clone(),
            PendingOp::Send {
                channel: channel_id,
                draft: draft.clone(),
                reply,
            },
        );
        // Follow the new row only when the user was at the bottom before it
        // appeared; checking after the push always reads as not at bottom.
        let was_at_bottom =
            self.selected_entry().map(|e| e.id) == Some(channel_id) && self.at_bottom();
        if let Some(entry) = self.entry_mut(&channel_id) {
            entry.rows.push(row);
            if was_at_bottom {
                self.set_focus(usize::MAX);
            }
        }
        self.outbox.push(SessionCommand::Send {
            channel: channel_id,
            content: draft.clone(),
            thread,
            local,
        });
        self.composer.clear();
        self.mode = Mode::Navigation;
    }

    fn complete_write(&mut self, local: String, event_id: String) {
        match self.pending.remove(&local) {
            Some(PendingOp::Send { channel, .. }) => {
                self.seen_aux_insert(&event_id);
                if let Some(entry) = self.entry_mut(&channel) {
                    if entry.rows.iter().any(|r| r.event_id == event_id) {
                        // The live echo already landed; drop the duplicate.
                        entry.rows.retain(|r| r.event_id != local);
                    } else if let Some(row) = entry.rows.iter_mut().find(|r| r.event_id == local) {
                        row.event_id = event_id.clone();
                        row.pending = false;
                    }
                    entry.seen.insert(event_id);
                }
                self.set_focus(self.focus);
                self.status = "sent".to_owned();
            }
            Some(PendingOp::Edit { row_id, .. }) => {
                self.seen_aux_insert(&event_id);
                if let Some(row) = self.row_mut(&row_id) {
                    row.uncertain = false;
                }
                self.status = "edited".to_owned();
            }
            Some(PendingOp::Delete { .. }) => {
                self.seen_aux_insert(&event_id);
                self.status = "deleted".to_owned();
            }
            Some(PendingOp::React { row_id, emoji }) => {
                self.seen_aux_insert(&event_id);
                self.reaction_of
                    .insert(event_id.clone(), (row_id.clone(), emoji.clone()));
                self.my_reaction.insert((row_id, emoji), event_id);
                self.status = "reacted".to_owned();
            }
            None => {}
        }
        self.set_focus(self.focus);
    }

    fn seen_aux_insert(&mut self, event_id: &str) {
        self.seen_aux.insert(event_id.to_owned());
    }

    fn fail_write(&mut self, local: String, reason: String) {
        match self.pending.remove(&local) {
            Some(PendingOp::Send {
                channel,
                draft,
                reply,
            }) => {
                if let Some(entry) = self.entry_mut(&channel) {
                    entry.rows.retain(|r| r.event_id != local);
                }
                self.set_focus(self.focus);
                self.composer.set_text(&draft);
                self.composer.reply = reply;
                self.mode = Mode::Composer;
                self.status = format!("send failed: {reason}");
            }
            Some(PendingOp::Edit {
                row_id, old_body, ..
            }) => {
                if let Some(row) = self.row_mut(&row_id) {
                    row.body = old_body;
                    row.uncertain = false;
                }
                self.status = format!("edit failed: {reason}");
            }
            Some(PendingOp::Delete { channel, row }) => {
                if let Some(entry) = self.entry_mut(&channel) {
                    entry.rows.push(row);
                    entry.rows.sort_by_key(|r| r.created_at);
                }
                self.set_focus(self.focus);
                self.status = format!("delete failed: {reason}");
            }
            Some(PendingOp::React { row_id, emoji }) => {
                if let Some(row) = self.row_mut(&row_id) {
                    content::apply_overlay(
                        row,
                        &content::Overlay::Reaction {
                            emoji: format!("-{emoji}"),
                        },
                    );
                }
                self.status = format!("reaction failed: {reason}");
            }
            None => {}
        }
    }

    fn uncertain_write(&mut self, local: String, reason: String) {
        match self.pending.remove(&local) {
            Some(PendingOp::Send { channel, .. }) => {
                if let Some(row) = self
                    .entry_mut(&channel)
                    .and_then(|entry| entry.rows.iter_mut().find(|r| r.event_id == local))
                {
                    row.pending = false;
                    row.uncertain = true;
                }
                self.status = format!("send uncertain, not retried: {reason}");
            }
            Some(PendingOp::Edit { row_id, .. }) => {
                if let Some(row) = self.row_mut(&row_id) {
                    row.uncertain = true;
                }
                self.status = format!("edit uncertain, not retried: {reason}");
            }
            Some(PendingOp::Delete { .. }) => {
                self.status = format!("delete uncertain, not retried: {reason}");
            }
            Some(PendingOp::React { .. }) => {
                self.status = format!("reaction uncertain, not retried: {reason}");
            }
            None => {}
        }
    }
}

/// Display name for an author: the profile name when known, `you` for the
/// identity, a shortened key otherwise.
fn author_name(profiles: &HashMap<String, String>, me: &str, pubkey: &str) -> String {
    if pubkey == me {
        return "you".to_owned();
    }
    profiles
        .get(pubkey)
        .cloned()
        .unwrap_or_else(|| content::short_pubkey(pubkey))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::{EventBuilder, Kind, Timestamp};

    fn keys() -> Keys {
        Keys::generate()
    }

    fn app() -> App {
        App::new(&keys(), "http://relay.test")
    }

    fn channel(id: u32) -> ChannelEntry {
        ChannelEntry {
            id: Uuid::from_u64_pair(id as u64, 0),
            name: format!("c{id}"),
            rows: Vec::new(),
            seen: HashSet::new(),
            loading: false,
        }
    }

    fn row(id: &str, created_at: u64, pubkey: &str) -> Row {
        Row {
            event_id: id.to_owned(),
            pubkey: pubkey.to_owned(),
            author: pubkey.to_owned(),
            created_at,
            body: format!("body {id}"),
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

    fn message_event(keys: &Keys, channel_id: Uuid, body: &str, at: u64) -> nostr::Event {
        EventBuilder::new(Kind::Custom(9), body)
            .tags(vec![
                nostr::Tag::parse(["h", &channel_id.to_string()]).unwrap(),
            ])
            .custom_created_at(Timestamp::from(at))
            .sign_with_keys(keys)
            .unwrap()
    }

    fn history_event(_app: &App, channel_id: Uuid, body: &str, at: u64) -> nostr::Event {
        message_event(&keys(), channel_id, body, at)
    }

    fn outbox_drained(app: &mut App) -> Vec<SessionCommand> {
        app.take_outbox()
    }

    #[test]
    fn a_connected_session_loads_channels_and_opens_the_first() {
        let mut app = app();
        let id = channel(1).id;
        app.apply(ChatEvent::Connected, 0);
        assert!(matches!(
            outbox_drained(&mut app)[..],
            [SessionCommand::LoadChannels]
        ));
        app.apply(
            ChatEvent::Channels(vec![ChannelInfo {
                id,
                name: "general".into(),
            }]),
            0,
        );
        match &outbox_drained(&mut app)[..] {
            [SessionCommand::OpenChannel(opened)] => assert_eq!(*opened, id),
            other => panic!("expected one open, got {other:?}"),
        }
        assert!(app.channels[0].loading);
    }

    #[test]
    fn history_loads_oldest_first_and_focuses_the_newest_row() {
        let mut app = app();
        let entry = channel(1);
        let id = entry.id;
        app.channels = vec![ChannelEntry {
            loading: true,
            ..entry
        }];
        let events = vec![
            history_event(&app, id, "new", 30),
            history_event(&app, id, "old", 10),
            history_event(&app, id, "mid", 20),
        ];
        app.apply(
            ChatEvent::History {
                channel: id,
                events,
            },
            0,
        );
        let bodies: Vec<&str> = app.channels[0]
            .rows
            .iter()
            .map(|r| r.body.as_str())
            .collect();
        assert_eq!(bodies, vec!["old", "mid", "new"]);
        assert_eq!(app.focus, 2, "focus is the newest loaded row");
        assert!(!app.channels[0].loading);
    }

    #[test]
    fn a_live_row_follows_only_when_the_user_is_at_the_bottom() {
        let mut app = app();
        let id = channel(1).id;
        app.channels = vec![channel(1)];
        app.channels[0].rows = vec![row("a", 1, "p1"), row("b", 2, "p1"), row("c", 3, "p1")];
        app.focus = 2;

        let event = history_event(&app, id, "live", 4);
        app.apply(ChatEvent::Timeline { channel: id, event }, 4);
        assert_eq!(app.channels[0].rows.len(), 4);
        assert_eq!(app.focus, 3, "at the bottom, new rows are followed");

        // Reading history: the user scrolled up.
        app.handle(Action::Top, 0);
        assert_eq!(app.focus, 0);
        let event = history_event(&app, id, "live2", 5);
        app.apply(ChatEvent::Timeline { channel: id, event }, 5);
        assert_eq!(app.focus, 0, "reading history is not interrupted");
        assert_eq!(app.channels[0].rows.len(), 5);
    }

    #[test]
    fn the_same_event_id_never_renders_twice() {
        let mut app = app();
        let id = channel(1).id;
        app.channels = vec![channel(1)];
        let event = history_event(&app, id, "once", 1);
        app.apply(
            ChatEvent::Timeline {
                channel: id,
                event: event.clone(),
            },
            1,
        );
        app.apply(ChatEvent::Timeline { channel: id, event }, 1);
        assert_eq!(app.channels[0].rows.len(), 1);
    }

    #[test]
    fn a_pending_send_replaces_its_row_on_ok_and_restores_the_draft_on_failure() {
        let mut app = app();
        app.channels = vec![channel(1)];
        app.handle(Action::ComposeNew, 0);
        app.composer.set_text("hello");
        app.composer.reply = Some(ReplyTarget {
            event_id: "b".into(),
            author: "bob".into(),
        });
        app.channels[0].rows = vec![row("b", 1, "bob")];
        app.focus = 0;

        app.handle(Action::ComposerSend, 100);
        let commands = outbox_drained(&mut app);
        match &commands[..] {
            [
                SessionCommand::Send {
                    local,
                    content,
                    thread,
                    ..
                },
            ] => {
                assert_eq!(content, "hello");
                assert_eq!(thread.as_ref().unwrap(), &("b".to_owned(), "b".to_owned()));
                assert!(local.starts_with("pending:"));
                let pending = app.channels[0].rows.last().unwrap();
                assert!(pending.pending);
                assert_eq!(pending.parent_id.as_deref(), Some("b"));

                app.apply(
                    ChatEvent::WriteOk {
                        local: local.clone(),
                        event_id: "real1".into(),
                    },
                    0,
                );
                let row = app.channels[0].rows.last().unwrap();
                assert_eq!(row.event_id, "real1");
                assert!(!row.pending);
            }
            other => panic!("expected one send, got {other:?}"),
        }
    }

    #[test]
    fn a_failed_send_restores_the_draft_and_explains_itself() {
        let mut app = app();
        app.channels = vec![channel(1)];
        app.handle(Action::ComposeNew, 0);
        app.composer.set_text("will fail");
        app.handle(Action::ComposerSend, 100);
        let commands = outbox_drained(&mut app);
        let SessionCommand::Send { local, .. } = &commands[0] else {
            panic!()
        };

        app.apply(
            ChatEvent::WriteFailed {
                local: local.clone(),
                reason: "not a member".into(),
            },
            0,
        );
        assert!(
            app.channels[0].rows.iter().all(|r| !r.pending),
            "the pending row is gone"
        );
        assert_eq!(app.composer.text(), "will fail");
        assert!(app.status.contains("not a member"));
        assert_eq!(app.mode, Mode::Composer);
    }

    #[test]
    fn an_uncertain_send_is_marked_and_never_resent() {
        let mut app = app();
        app.channels = vec![channel(1)];
        app.handle(Action::ComposeNew, 0);
        app.composer.set_text("maybe");
        app.handle(Action::ComposerSend, 100);
        let commands = outbox_drained(&mut app);
        let SessionCommand::Send { local, .. } = &commands[0] else {
            panic!()
        };

        app.apply(
            ChatEvent::WriteUncertain {
                local: local.clone(),
                reason: "timeout".into(),
            },
            0,
        );
        let row = app.channels[0].rows.last().unwrap();
        assert!(row.uncertain);
        assert!(!row.pending);
        assert!(
            outbox_drained(&mut app).is_empty(),
            "an uncertain write is not retried"
        );
    }

    #[test]
    fn a_reply_targets_a_nested_thread_with_root_and_parent() {
        let mut app = app();
        app.channels = vec![channel(1)];
        app.channels[0].rows = vec![
            Row {
                event_id: "root1".into(),
                root_id: None,
                ..row("root1", 1, "bob")
            },
            Row {
                event_id: "child".into(),
                root_id: Some("root1".into()),
                ..row("child", 2, "amy")
            },
        ];
        app.focus = 1;
        app.handle(Action::ComposeReply, 0);
        assert_eq!(app.composer.reply.as_ref().unwrap().event_id, "child");
        app.composer.set_text("nested answer");
        app.handle(Action::ComposerSend, 0);
        match &outbox_drained(&mut app)[..] {
            [SessionCommand::Send { thread, .. }] => {
                assert_eq!(
                    thread.as_ref().unwrap(),
                    &("root1".to_owned(), "child".to_owned())
                );
            }
            other => panic!("expected one send, got {other:?}"),
        }
    }

    #[test]
    fn escape_clears_the_reply_target_before_leaving_the_composer() {
        let mut app = app();
        app.channels = vec![channel(1)];
        app.channels[0].rows = vec![row("b", 1, "bob")];
        app.focus = 0;
        app.handle(Action::ComposeReply, 0);
        app.handle(Action::ComposerEscape, 0);
        assert_eq!(
            app.mode,
            Mode::Composer,
            "the first Esc clears the target only"
        );
        assert!(app.composer.reply.is_none());
        app.handle(Action::ComposerEscape, 0);
        assert_eq!(app.mode, Mode::Navigation);
    }

    #[test]
    fn edit_and_delete_refuse_rows_the_identity_did_not_write() {
        let mut app = app();
        app.channels = vec![channel(1)];
        app.channels[0].rows = vec![row("b", 1, "someone-else")];
        app.focus = 0;
        app.handle(Action::EditRow, 0);
        assert_eq!(app.mode, Mode::Navigation);
        assert!(app.status.contains("own"));
        app.handle(Action::DeleteRow, 0);
        assert_eq!(app.channels[0].rows.len(), 1);
    }

    #[test]
    fn an_edit_replaces_the_body_and_failure_restores_it() {
        let mut app = app();
        let me = app.me.clone();
        app.channels = vec![channel(1)];
        app.channels[0].rows = vec![row("m1", 1, &me)];
        app.focus = 0;
        app.handle(Action::EditRow, 0);
        app.composer.set_text("corrected");
        app.handle(Action::ComposerSend, 0);
        assert_eq!(app.channels[0].rows[0].body, "corrected");

        let commands = outbox_drained(&mut app);
        let SessionCommand::Edit {
            local,
            target,
            content,
            ..
        } = &commands[0]
        else {
            panic!()
        };
        assert_eq!(target, "m1");
        assert_eq!(content, "corrected");

        app.apply(
            ChatEvent::WriteFailed {
                local: local.clone(),
                reason: "refused".into(),
            },
            0,
        );
        assert_eq!(
            app.channels[0].rows[0].body, "body m1",
            "the old body returns"
        );
    }

    #[test]
    fn a_delete_removes_the_row_and_failure_restores_it() {
        let mut app = app();
        let me = app.me.clone();
        app.channels = vec![channel(1)];
        app.channels[0].rows = vec![row("keep", 1, "p"), row("mine", 2, &me)];
        app.focus = 1;
        app.handle(Action::DeleteRow, 0);
        assert_eq!(app.channels[0].rows.len(), 1, "the row disappears at once");
        let commands = outbox_drained(&mut app);
        let SessionCommand::Delete { local, target, .. } = &commands[0] else {
            panic!()
        };
        assert_eq!(target, "mine");

        app.apply(
            ChatEvent::WriteFailed {
                local: local.clone(),
                reason: "refused".into(),
            },
            0,
        );
        assert_eq!(
            app.channels[0].rows.len(),
            2,
            "a refused delete restores the row"
        );
    }

    #[test]
    fn a_reaction_counts_and_a_second_press_removes_it() {
        let mut app = app();
        app.channels = vec![channel(1)];
        app.channels[0].rows = vec![row("m", 1, "p")];
        app.focus = 0;
        app.handle(Action::React, 0);
        assert_eq!(app.channels[0].rows[0].reactions.len(), 1);
        let commands = outbox_drained(&mut app);
        let SessionCommand::React {
            local,
            target,
            remove,
            ..
        } = &commands[0]
        else {
            panic!()
        };
        assert_eq!(target, "m");
        assert!(remove.is_none());

        app.apply(
            ChatEvent::WriteOk {
                local: local.clone(),
                event_id: "react1".into(),
            },
            0,
        );
        assert!(
            app.my_reaction
                .contains_key(&("m".to_owned(), content::DEFAULT_REACTION.to_owned()))
        );

        app.handle(Action::React, 0);
        assert!(
            app.channels[0].rows[0].reactions.is_empty(),
            "the toggle decrements"
        );
        match &outbox_drained(&mut app)[..] {
            [
                SessionCommand::React {
                    remove: Some(id), ..
                },
            ] => assert_eq!(id, "react1"),
            other => panic!("expected a removal, got {other:?}"),
        }
    }

    #[test]
    fn a_remote_reaction_overlay_accumulates_and_its_removal_decrements() {
        let mut app = app();
        app.channels = vec![channel(1)];
        let row_id = format!("{:0<64}", "a1");
        let reaction_id = format!("{:0<64}", "e9");
        app.channels[0].rows = vec![row(&row_id, 1, "p")];

        let reaction = reaction_event(&row_id, content::DEFAULT_REACTION, &reaction_id);
        app.apply(ChatEvent::Overlay(reaction), 0);
        assert_eq!(app.channels[0].rows[0].reactions[0].1, 1);

        // Replaying the same overlay must not double count.
        let reaction = reaction_event(&row_id, content::DEFAULT_REACTION, &reaction_id);
        app.apply(ChatEvent::Overlay(reaction), 0);
        assert_eq!(app.channels[0].rows[0].reactions[0].1, 1);

        // A kind 5 aimed at the reaction event id decrements it.
        let removal = delete_event(&reaction_id);
        app.apply(ChatEvent::Overlay(removal), 0);
        assert!(app.channels[0].rows[0].reactions.is_empty());
    }

    #[test]
    fn a_remote_edit_overlay_replaces_the_body() {
        let mut app = app();
        app.channels = vec![channel(1)];
        app.channels[0].rows = vec![row("a1", 1, "p")];
        let edit = edit_event("a1", "rewritten");
        app.apply(ChatEvent::Overlay(edit), 0);
        assert_eq!(app.channels[0].rows[0].body, "rewritten");
        assert!(app.channels[0].rows[0].edited);
    }

    #[test]
    fn a_remote_delete_overlay_removes_the_row() {
        let mut app = app();
        app.channels = vec![channel(1)];
        app.channels[0].rows = vec![row("a1", 1, "p")];
        let delete = delete_event("a1");
        app.apply(ChatEvent::Overlay(delete), 0);
        assert!(app.channels[0].rows.is_empty());
    }

    #[test]
    fn an_overlay_for_an_unloaded_id_is_dropped() {
        let mut app = app();
        app.channels = vec![channel(1)];
        app.channels[0].rows = vec![row("a1", 1, "p")];
        let edit = edit_event("b2", "nope");
        app.apply(ChatEvent::Overlay(edit), 0);
        assert_eq!(app.channels[0].rows[0].body, "body a1");
    }

    #[test]
    fn page_keys_move_focus_in_blocks_and_clamp() {
        let mut app = app();
        app.channels = vec![channel(1)];
        app.channels[0].rows = (0..25).map(|i| row(&format!("r{i}"), i, "p")).collect();
        app.set_focus(usize::MAX);
        assert_eq!(app.focus, 24);
        app.handle(Action::PageUp, 0);
        assert_eq!(app.focus, 14);
        app.handle(Action::Top, 0);
        assert_eq!(app.focus, 0);
        app.handle(Action::PageDown, 0);
        assert_eq!(app.focus, 10);
    }

    #[test]
    fn switching_channels_focuses_the_newest_row_and_requests_history() {
        let mut app = app();
        let first = channel(1);
        let second = channel(2);
        let second_id = second.id;
        app.channels = vec![
            ChannelEntry {
                rows: vec![row("a", 1, "p")],
                ..first
            },
            ChannelEntry {
                rows: vec![row("x", 1, "p"), row("y", 2, "p")],
                ..second
            },
        ];
        app.handle(Action::NextChannel, 0);
        assert_eq!(app.selected, 1);
        assert_eq!(app.focus, 1);
        match &outbox_drained(&mut app)[..] {
            [SessionCommand::OpenChannel(id)] => assert_eq!(*id, second_id),
            other => panic!("expected one open, got {other:?}"),
        }
    }

    #[test]
    fn digits_jump_to_channels_and_clamp_to_the_last() {
        let mut app = app();
        app.channels = vec![channel(1), channel(2), channel(3)];
        app.handle(Action::Channel(3), 0);
        assert_eq!(app.selected, 2);
        app.handle(Action::Channel(9), 0);
        assert_eq!(app.selected, 2, "out-of-range digits do nothing");
    }

    #[test]
    fn a_restricted_channel_leaves_the_list_and_moves_the_selection() {
        let mut app = app();
        let first = channel(1);
        let second = channel(2);
        let gone = first.id;
        app.channels = vec![first, second];
        app.selected = 0;
        app.apply(
            ChatEvent::ChannelGone {
                channel: gone,
                reason: "restricted: membership lost".into(),
            },
            0,
        );
        assert_eq!(app.channels.len(), 1);
        assert_eq!(app.selected, 0);
        assert!(matches!(
            outbox_drained(&mut app)[..],
            [SessionCommand::OpenChannel(_)]
        ));
    }

    #[test]
    fn profiles_resolve_authors_and_you_renders_for_the_identity() {
        let mut app = app();
        app.channels = vec![channel(1)];
        app.channels[0].rows = vec![row("m", 1, "cafe0000")];
        app.apply(
            ChatEvent::Profiles(vec![("cafe0000".to_owned(), "Cafe Owner".to_owned())]),
            0,
        );
        assert_eq!(app.channels[0].rows[0].author, "Cafe Owner");
    }

    #[test]
    fn unknown_authors_are_requested_once() {
        let mut app = app();
        app.channels = vec![channel(1)];
        app.channels[0].rows = vec![row("m", 1, "aa"), row("n", 2, "aa"), row("o", 3, "bb")];
        app.request_profiles();
        match &outbox_drained(&mut app)[..] {
            [SessionCommand::LoadProfiles(pubkeys)] => {
                assert_eq!(pubkeys.len(), 2, "deduplicated");
                assert!(pubkeys.contains(&"aa".to_owned()));
                assert!(pubkeys.contains(&"bb".to_owned()));
            }
            other => panic!("expected one load, got {other:?}"),
        }
    }

    #[test]
    fn the_composer_edits_text_across_lines() {
        let mut composer = Composer::new();
        composer.set_text("one\ntwo");
        assert_eq!(composer.cursor, (1, 3));
        composer.input('!');
        assert_eq!(composer.text(), "one\ntwo!");
        composer.newline();
        composer.input('n');
        assert_eq!(composer.text(), "one\ntwo!\nn");
        composer.backspace();
        composer.backspace();
        assert_eq!(composer.text(), "one\ntwo!", "backspace merges lines");
        composer.set_text("helo");
        composer.left();
        composer.left();
        composer.input('l');
        assert_eq!(composer.text(), "hello");
    }

    fn reaction_event(target: &str, emoji: &str, id: &str) -> nostr::Event {
        let keys = keys();
        let mut event = EventBuilder::new(Kind::Custom(7), emoji)
            .tags(vec![nostr::Tag::parse(["e", target]).unwrap()])
            .custom_created_at(Timestamp::from(1))
            .sign_with_keys(&keys)
            .unwrap();
        set_id(&mut event, id);
        event
    }

    fn edit_event(target: &str, body: &str) -> nostr::Event {
        let keys = keys();
        let mut event = EventBuilder::new(Kind::Custom(40003), body)
            .tags(vec![nostr::Tag::parse(["e", target]).unwrap()])
            .custom_created_at(Timestamp::from(1))
            .sign_with_keys(&keys)
            .unwrap();
        set_id(&mut event, "e3");
        event
    }

    fn delete_event(target: &str) -> nostr::Event {
        let keys = keys();
        let mut event = EventBuilder::new(Kind::Custom(5), "")
            .tags(vec![nostr::Tag::parse(["e", target]).unwrap()])
            .custom_created_at(Timestamp::from(1))
            .sign_with_keys(&keys)
            .unwrap();
        set_id(&mut event, "d5");
        event
    }

    /// The id a real relay would assign; tests need stable, distinct ids
    /// made only of hex digits.
    fn set_id(event: &mut nostr::Event, prefix: &str) {
        let full = format!("{prefix:0<64}");
        event.id = nostr::EventId::from_hex(&full).unwrap();
    }

    #[test]
    fn history_arriving_twice_keeps_pending_rows() {
        let mut app = app();
        let id = channel(1).id;
        app.channels = vec![channel(1)];
        let events = vec![history_event(&app, id, "old", 10)];
        app.apply(
            ChatEvent::History {
                channel: id,
                events,
            },
            0,
        );
        app.handle(Action::ComposeNew, 0);
        app.composer.set_text("in flight");
        app.handle(Action::ComposerSend, 20);
        let events = vec![history_event(&app, id, "old", 10)];
        app.apply(
            ChatEvent::History {
                channel: id,
                events,
            },
            0,
        );
        let bodies: Vec<&str> = app.channels[0]
            .rows
            .iter()
            .map(|r| r.body.as_str())
            .collect();
        assert_eq!(
            bodies,
            vec!["old", "in flight"],
            "a reload keeps optimistic rows"
        );
    }
}
