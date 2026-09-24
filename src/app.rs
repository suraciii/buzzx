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
    /// Removing the identity's own reaction. Its WriteOk must not re-record
    /// the reaction as active.
    ReactRemove {
        row_id: String,
        emoji: String,
        reaction_id: String,
    },
}

/// One identity composing in one channel. `since` is the event time of the
/// last indicator, so a message can be compared against the claim it ends;
/// `expires_at` is this machine's clock, because believing a claim is local.
#[derive(Debug, Clone, Copy)]
struct TypingEntry {
    since: u64,
    expires_at: u64,
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
    /// The channel picker's cursor while it is open. Compact layouts open it
    /// with `c`; the picker is the only channel list they show.
    pub picker: Option<usize>,
    pub conn: ConnState,
    pub status: String,
    pub composer: Composer,
    pub quit: bool,
    pub exit_code: i32,
    profiles: HashMap<String, String>,
    /// Live typing indicators, per channel: pubkey -> the second the entry
    /// expires. Ephemeral state, rebuilt from the current connection only.
    typing: HashMap<Uuid, HashMap<String, TypingEntry>>,
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
            picker: None,
            conn: ConnState::Connecting,
            status: "connecting".to_owned(),
            composer: Composer::new(),
            quit: false,
            exit_code: 0,
            profiles: HashMap::new(),
            typing: HashMap::new(),
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

    /// Drop typing entries whose 8-second TTL has passed. Reads filter on the
    /// same deadline, so a missed tick delays the cleanup, never the expiry.
    pub fn expire_typing(&mut self, now: u64) {
        self.typing.retain(|_, entries| {
            entries.retain(|_, entry| entry.expires_at > now);
            !entries.is_empty()
        });
    }

    /// The identities composing in one channel right now, by display name.
    /// Sorted and deduplicated so the line does not reshuffle between frames.
    pub fn typing_names(&self, channel: Uuid, now: u64) -> Vec<String> {
        let Some(entries) = self.typing.get(&channel) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .iter()
            .filter(|(_, entry)| entry.expires_at > now)
            .map(|(pubkey, _)| author_name(&self.profiles, &self.me, pubkey))
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// Whether anyone is composing in one channel. The channel list asks this
    /// for every entry it draws.
    pub fn is_typing(&self, channel: Uuid, now: u64) -> bool {
        self.typing
            .get(&channel)
            .is_some_and(|entries| entries.values().any(|entry| entry.expires_at > now))
    }

    /// One message from this author ends the signal it announced. Only a
    /// message that is not older than the claim counts: a timeline replays up
    /// to 30 seconds of history on a new subscription, and an author who sent
    /// something before starting to compose is still composing.
    fn end_typing(&mut self, channel: Uuid, pubkey: &str, at: u64) {
        if let Some(entries) = self.typing.get_mut(&channel)
            && entries.get(pubkey).is_some_and(|entry| at >= entry.since)
        {
            entries.remove(pubkey);
        }
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
                // Typing state lives on the connection that carried it: the
                // session is over, so every entry is stale.
                self.typing.clear();
                self.status = format!("reconnecting: {reason}");
            }
            ChatEvent::Channels(channels) => {
                self.merge_channels(channels);
                // Typing state follows the roster: a channel that is no longer
                // a member channel keeps no indicator, or it would show one if
                // the same id came back inside the TTL.
                let ids: Vec<Uuid> = self.channels.iter().map(|c| c.id).collect();
                self.typing.retain(|channel, _| ids.contains(channel));
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
            ChatEvent::Typing {
                channel,
                pubkey,
                at,
            } => {
                // The identity's own typing is never shown: it already knows.
                // Another client of the same identity (the desktop app) is a
                // member too, and its indicators land here as well.
                if pubkey != self.me {
                    // A name beats a shortened key, so an author first seen
                    // typing gets resolved like one first seen posting. Only a
                    // new entry asks: a refresh of a live one asks nothing.
                    let fresh = self
                        .typing
                        .entry(channel)
                        .or_default()
                        .insert(
                            pubkey.clone(),
                            TypingEntry {
                                since: at,
                                expires_at: now.saturating_add(content::TYPING_TTL_SECS),
                            },
                        )
                        .is_none();
                    if fresh && !self.profiles.contains_key(&pubkey) {
                        self.outbox.push(SessionCommand::LoadProfiles(vec![pubkey]));
                    }
                }
            }
            ChatEvent::TypingClosed { channel, reason } => {
                self.typing.remove(&channel);
                // A channel that is gone has already said why it is quiet;
                // this status is for a feed that was refused while the channel
                // itself is still open.
                if self.channels.iter().any(|c| c.id == channel) {
                    self.status = format!("typing feed closed: {reason}");
                }
            }
            ChatEvent::Overlay(event) => self.apply_overlay(event),
            ChatEvent::ChannelGone { channel, reason } => {
                self.channels.retain(|c| c.id != channel);
                self.typing.remove(&channel);
                if self.selected >= self.channels.len() {
                    self.selected = self.channels.len().saturating_sub(1);
                }
                self.set_focus(self.focus);
                self.clamp_picker();
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
        self.clamp_picker();
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
        let fetched: Vec<(String, u64)> = rows
            .iter()
            .filter(|r| !r.pending)
            .map(|r| (r.pubkey.clone(), r.created_at))
            .collect();
        entry.rows = rows;
        entry.loading = false;
        // History can land after the indicator it belongs to - the channel is
        // opened over HTTP while the live feed is already running - so the
        // fetched rows end indicators on the same rule as live ones.
        for (pubkey, at) in fetched {
            self.end_typing(channel, &pubkey, at);
        }
        if self.selected_entry().map(|e| e.id) == Some(channel) {
            self.set_focus(usize::MAX);
        }
        self.request_profiles();
        let _ = now;
    }

    fn apply_timeline(&mut self, channel: Uuid, event: nostr::Event, _now: u64) {
        let id = event.id.to_hex();
        let me = self.me.clone();
        let profiles = self.profiles.clone();
        let author_key = event.pubkey.to_hex();
        let author = author_name(&profiles, &me, &author_key);
        let known_author = profiles.contains_key(&author_key) || author_key == me;
        let selected = self.selected_entry().map(|e| e.id) == Some(channel);
        let was_at_bottom = selected && self.at_bottom();
        let Some(entry) = self.entry_mut(&channel) else {
            return;
        };
        if entry.seen.contains(&id) || entry.rows.iter().any(|r| r.event_id == id) {
            return;
        }
        entry.seen.insert(id);
        let mut row = content::row_from_event(&event, &me);
        row.author = author;
        entry.rows.push(row);
        if was_at_bottom {
            self.focus = entry.rows.len() - 1;
        }
        // The message itself is the end of that author's indicator: typing is
        // a pre-message signal, so it never outlives the message it announced.
        // A replay of an older message leaves a live claim standing.
        self.end_typing(channel, &author_key, event.created_at.as_secs());
        if !known_author {
            self.outbox
                .push(SessionCommand::LoadProfiles(vec![author_key]));
        }
    }

    fn apply_overlay(&mut self, event: nostr::Event) {
        let id = event.id.to_hex();
        if !self.seen_aux.insert(id.clone()) {
            return;
        }
        // A kind 5 aimed at a known reaction event id removes that reaction
        // before the generic overlay path sees it as a row deletion.
        if event.kind.as_u16() == 5
            && let Some(target) = event.tags.iter().find_map(|t| {
                let parts = t.as_slice();
                (parts.first().map(String::as_str) == Some("e"))
                    .then(|| parts.get(1).cloned())
                    .flatten()
            })
            && let Some((row_id, emoji)) = self.reaction_of.get(&target).cloned()
        {
            self.apply_to_row(&row_id, |row| {
                content::apply_overlay(
                    row,
                    &content::Overlay::Reaction {
                        emoji: format!("-{emoji}"),
                    },
                );
            });
            self.reaction_of.remove(&target);
            self.my_reaction.remove(&(row_id, emoji));
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

    /// Drop the bookkeeping for one reaction so a later toggle cannot remove
    /// an already-removed reaction twice.
    fn purge_reaction(&mut self, reaction_id: &str) {
        if let Some((row_id, emoji)) = self.reaction_of.remove(reaction_id) {
            self.my_reaction.remove(&(row_id, emoji));
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
            Action::ToggleHelp => {
                self.help = !self.help;
                if self.help {
                    // The help draws over the picker, so it replaces it.
                    self.picker = None;
                }
            }
            Action::Dismiss => {
                if self.help {
                    self.help = false;
                } else {
                    self.picker = None;
                }
            }
            Action::NextChannel => self.switch_channel(self.selected.saturating_add(1)),
            Action::PrevChannel => self.switch_channel(self.selected.saturating_sub(1)),
            Action::NextRow => self.set_focus(self.focus.saturating_add(1)),
            Action::PrevRow => self.set_focus(self.focus.saturating_sub(1)),
            Action::Channel(n) => {
                self.picker = None;
                self.switch_channel(n.saturating_sub(1));
            }
            Action::TogglePicker => self.toggle_picker(),
            Action::PickerNext => self.move_picker(1),
            Action::PickerPrev => self.move_picker(-1),
            Action::PickerConfirm => {
                if let Some(index) = self.picker.take() {
                    self.switch_channel(index);
                }
            }
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
                if target
                    .as_ref()
                    .map(|(id, _, pending)| *pending || !is_event_id(id))
                    == Some(true)
                {
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

    /// `c`: the picker opens on the selected channel, and closes when it is
    /// already open. An empty channel list has nothing to pick.
    fn toggle_picker(&mut self) {
        self.picker = match self.picker {
            Some(_) => None,
            None if !self.channels.is_empty() => {
                // Only one overlay is on screen at a time.
                self.help = false;
                Some(self.selected)
            }
            None => None,
        };
    }

    fn move_picker(&mut self, step: isize) {
        let last = self.channels.len().saturating_sub(1);
        if let Some(index) = self.picker {
            self.picker = Some(index.saturating_add_signed(step).min(last));
        }
    }

    /// The membership can change under the picker; its cursor never points
    /// past the list.
    fn clamp_picker(&mut self) {
        if let Some(index) = self.picker {
            self.picker = Some(index.min(self.channels.len().saturating_sub(1)));
        }
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
                PendingOp::ReactRemove {
                    row_id: row_id.clone(),
                    emoji: emoji.clone(),
                    reaction_id: reaction_id.clone(),
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
                self.status = if row.pending || !is_event_id(&row.event_id) {
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
            if let Some((_, id, pending)) = own {
                self.status = if pending || !is_event_id(&id) {
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
            Some(PendingOp::ReactRemove { reaction_id, .. }) => {
                self.seen_aux_insert(&event_id);
                self.purge_reaction(&reaction_id);
                self.status = "reaction removed".to_owned();
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
            Some(PendingOp::ReactRemove {
                row_id,
                emoji,
                reaction_id,
            }) => {
                // The removal was refused: the counter comes back, and so
                // does the toggle entry.
                if let Some(row) = self.row_mut(&row_id) {
                    content::apply_overlay(
                        row,
                        &content::Overlay::Reaction {
                            emoji: emoji.clone(),
                        },
                    );
                }
                self.my_reaction.insert((row_id, emoji), reaction_id);
                self.status = format!("reaction removal failed: {reason}");
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
            Some(PendingOp::React { .. }) | Some(PendingOp::ReactRemove { .. }) => {
                self.status = format!("reaction uncertain, not retried: {reason}");
            }
            None => {}
        }
    }
}

/// A real event address: 64 hex characters, not a local pending marker.
fn is_event_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit())
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

    fn channel_info(id: u32) -> ChannelInfo {
        ChannelInfo {
            id: Uuid::from_u64_pair(id as u64, 0),
            name: format!("c{id}"),
        }
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
    fn a_typing_indicator_names_its_channel_and_expires_after_its_ttl() {
        let mut app = app();
        let (one, two) = (channel(1), channel(2));
        app.channels = vec![one.clone(), two.clone()];
        app.apply(
            ChatEvent::Profiles(vec![("agent-a".into(), "Agent A".into())]),
            0,
        );

        app.apply(
            ChatEvent::Typing {
                channel: one.id,
                pubkey: "agent-a".into(),
                at: 100,
            },
            100,
        );
        assert_eq!(app.typing_names(one.id, 100), vec!["Agent A".to_owned()]);
        assert!(app.is_typing(one.id, 100));
        assert!(!app.is_typing(two.id, 100), "another channel stays quiet");

        // The publisher refreshes every 3 seconds and the deadline moves with
        // each one: 8 seconds after the last, the entry is gone.
        app.apply(
            ChatEvent::Typing {
                channel: one.id,
                pubkey: "agent-a".into(),
                at: 100,
            },
            103,
        );
        assert!(app.is_typing(one.id, 110), "a refreshed entry stays live");
        assert!(!app.is_typing(one.id, 111), "three refreshes, then expiry");

        app.expire_typing(111);
        assert!(app.typing_names(one.id, 111).is_empty());
        assert!(app.typing_names(channel(9).id, 0).is_empty());
    }

    #[test]
    fn a_message_ends_its_authors_indicator() {
        let mut app = app();
        let id = channel(1).id;
        app.channels = vec![channel(1)];
        let agent = keys();
        let agent_key = agent.public_key().to_hex();
        app.apply(
            ChatEvent::Profiles(vec![(agent_key.clone(), "Agent A".into())]),
            0,
        );
        app.apply(
            ChatEvent::Typing {
                channel: id,
                pubkey: agent_key,
                at: 100,
            },
            100,
        );
        assert!(app.is_typing(id, 100));

        let event = message_event(&agent, id, "the answer", 101);
        app.apply(ChatEvent::Timeline { channel: id, event }, 101);
        assert!(
            !app.is_typing(id, 101),
            "the message replaced the indicator"
        );
    }

    #[test]
    fn the_identities_own_typing_is_never_shown() {
        let mut app = app();
        let id = channel(1).id;
        app.channels = vec![channel(1)];
        // Another client of the same identity is still the identity.
        let me = app.me.clone();
        app.apply(
            ChatEvent::Typing {
                channel: id,
                pubkey: me,
                at: 100,
            },
            100,
        );
        assert!(!app.is_typing(id, 100));
        assert!(app.typing_names(id, 100).is_empty());
    }

    #[test]
    fn a_closed_channel_and_a_disconnect_drop_their_indicators() {
        let mut app = app();
        let (one, two) = (channel(1), channel(2));
        app.channels = vec![one.clone(), two.clone()];
        for (channel_id, pubkey) in [(one.id, "agent-a"), (two.id, "agent-b")] {
            app.apply(
                ChatEvent::Typing {
                    channel: channel_id,
                    pubkey: pubkey.into(),
                    at: 100,
                },
                100,
            );
        }

        app.apply(
            ChatEvent::ChannelGone {
                channel: one.id,
                reason: "restricted".into(),
            },
            101,
        );
        assert!(!app.is_typing(one.id, 101), "a closed channel keeps none");
        assert!(
            app.is_typing(two.id, 101),
            "the other channel keeps its own"
        );

        app.apply(ChatEvent::Disconnected("socket closed".into()), 102);
        assert!(!app.is_typing(two.id, 102), "nothing survives the session");
    }

    #[test]
    fn an_unknown_typist_shows_a_short_key_until_the_profile_lands() {
        let mut app = app();
        let id = channel(1).id;
        app.channels = vec![channel(1)];
        let agent = keys();
        let key = agent.public_key().to_hex();
        app.apply(
            ChatEvent::Typing {
                channel: id,
                pubkey: key.clone(),
                at: 100,
            },
            100,
        );
        let short = app.typing_names(id, 100);
        assert_eq!(short.len(), 1);
        assert_ne!(short[0], key, "a key is shortened, never shown whole");

        app.apply(ChatEvent::Profiles(vec![(key, "Agent A".into())]), 101);
        assert_eq!(
            app.typing_names(id, 101),
            vec!["Agent A".to_owned()],
            "the resolved name replaces the key without another indicator"
        );
    }

    #[test]
    fn a_message_ends_the_indicator_only_when_it_is_not_older() {
        let mut app = app();
        let id = channel(1).id;
        app.channels = vec![channel(1)];
        let agent = keys();
        let key = agent.public_key().to_hex();
        app.apply(
            ChatEvent::Profiles(vec![(key.clone(), "Agent A".into())]),
            0,
        );
        let claim = |at| ChatEvent::Typing {
            channel: id,
            pubkey: key.clone(),
            at,
        };

        // A message the author sent before starting to compose is not the end
        // of anything: the timeline replays 30 seconds of history, and the
        // author is still composing.
        app.apply(claim(100), 100);
        let old = message_event(&agent, id, "before", 99);
        app.apply(
            ChatEvent::Timeline {
                channel: id,
                event: old,
            },
            101,
        );
        assert!(app.is_typing(id, 101), "an older message changes nothing");

        // The message that answers the claim does end it.
        let answer = message_event(&agent, id, "the answer", 102);
        app.apply(
            ChatEvent::Timeline {
                channel: id,
                event: answer,
            },
            102,
        );
        assert!(!app.is_typing(id, 102));
    }

    #[test]
    fn history_that_lands_after_an_indicator_ends_it() {
        let mut app = app();
        let id = channel(1).id;
        app.channels = vec![channel(1)];
        let agent = keys();
        let key = agent.public_key().to_hex();
        app.apply(
            ChatEvent::Profiles(vec![(key.clone(), "Agent A".into())]),
            0,
        );
        // The channel is opened over HTTP while the live feed is already
        // running: the indicator can land first, its author's message second.
        app.apply(
            ChatEvent::Typing {
                channel: id,
                pubkey: key.clone(),
                at: 100,
            },
            100,
        );
        app.apply(
            ChatEvent::History {
                channel: id,
                events: vec![message_event(&agent, id, "the answer", 101)],
            },
            101,
        );
        assert!(
            !app.is_typing(id, 101),
            "the fetched message is the end of the signal"
        );
    }

    #[test]
    fn a_channel_that_leaves_the_roster_keeps_no_indicator() {
        let mut app = app();
        let (one, two) = (channel(1), channel(2));
        app.channels = vec![one.clone(), two.clone()];
        for id in [one.id, two.id] {
            app.apply(
                ChatEvent::Typing {
                    channel: id,
                    pubkey: "agent-a".into(),
                    at: 100,
                },
                100,
            );
        }
        app.apply(ChatEvent::Channels(vec![channel_info(1)]), 101);
        assert!(app.is_typing(one.id, 101), "a member channel keeps its own");
        assert!(
            !app.is_typing(two.id, 101),
            "a channel the roster dropped keeps nothing, even if it returns"
        );
    }

    #[test]
    fn a_refused_typing_feed_clears_its_channel_and_explains_itself() {
        let mut app = app();
        let (one, two) = (channel(1), channel(2));
        app.channels = vec![one.clone(), two.clone()];
        app.apply(
            ChatEvent::Typing {
                channel: one.id,
                pubkey: "agent-a".into(),
                at: 100,
            },
            100,
        );
        app.apply(
            ChatEvent::TypingClosed {
                channel: one.id,
                reason: "restricted: not a member".into(),
            },
            101,
        );
        assert!(!app.is_typing(one.id, 101));
        assert!(
            app.status.contains("restricted: not a member"),
            "the open channel says why nobody is typing: {}",
            app.status
        );

        // A closed channel already said why it is quiet; the typing feed on
        // top of it is not a second message.
        app.apply(
            ChatEvent::ChannelGone {
                channel: two.id,
                reason: "restricted".into(),
            },
            102,
        );
        let after_gone = app.status.clone();
        app.apply(
            ChatEvent::TypingClosed {
                channel: two.id,
                reason: "restricted".into(),
            },
            102,
        );
        assert_eq!(app.status, after_gone);
    }

    #[test]
    fn a_pending_send_replaces_its_row_on_ok_and_restores_the_draft_on_failure() {
        let mut app = app();
        app.channels = vec![channel(1)];
        app.handle(Action::ComposeNew, 0);
        app.composer.set_text("hello");
        let parent = format!("{:0<64}", "bb");
        app.composer.reply = Some(ReplyTarget {
            event_id: parent.clone(),
            author: "bob".into(),
        });
        app.channels[0].rows = vec![row(&parent, 1, "bob")];
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
                assert_eq!(thread.as_ref().unwrap(), &(parent.clone(), parent.clone()));
                assert!(local.starts_with("pending:"));
                let pending = app.channels[0].rows.last().unwrap();
                assert!(pending.pending);
                assert_eq!(pending.parent_id.as_deref(), Some(parent.as_str()));

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
        let root = format!("{:0<64}", "11");
        let child = format!("{:0<64}", "22");
        app.channels[0].rows = vec![
            Row {
                event_id: root.clone(),
                root_id: None,
                ..row(&root, 1, "bob")
            },
            Row {
                event_id: child.clone(),
                root_id: Some(root.clone()),
                ..row(&child, 2, "amy")
            },
        ];
        app.focus = 1;
        app.handle(Action::ComposeReply, 0);
        assert_eq!(app.composer.reply.as_ref().unwrap().event_id, child);
        app.composer.set_text("nested answer");
        app.handle(Action::ComposerSend, 0);
        match &outbox_drained(&mut app)[..] {
            [SessionCommand::Send { thread, .. }] => {
                assert_eq!(thread.as_ref().unwrap(), &(root, child));
            }
            other => panic!("expected one send, got {other:?}"),
        }
    }

    #[test]
    fn escape_clears_the_reply_target_before_leaving_the_composer() {
        let mut app = app();
        let target = format!("{:0<64}", "bb");
        app.channels = vec![channel(1)];
        app.channels[0].rows = vec![row(&target, 1, "bob")];
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
        let other = format!("{:0<64}", "bb");
        app.channels = vec![channel(1)];
        app.channels[0].rows = vec![row(&other, 1, "someone-else")];
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
        app.channels[0].rows = vec![row(&format!("{:0<64}", "e1"), 1, &me)];
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
        assert_eq!(target, &format!("{:0<64}", "e1"));
        assert_eq!(content, "corrected");

        app.apply(
            ChatEvent::WriteFailed {
                local: local.clone(),
                reason: "refused".into(),
            },
            0,
        );
        assert_eq!(
            app.channels[0].rows[0].body,
            format!("body {:0<64}", "e1"),
            "the old body returns"
        );
    }

    #[test]
    fn a_delete_removes_the_row_and_failure_restores_it() {
        let mut app = app();
        let me = app.me.clone();
        app.channels = vec![channel(1)];
        let mine = format!("{:0<64}", "dd");
        app.channels[0].rows = vec![row("keep", 1, "p"), row(&mine, 2, &me)];
        app.focus = 1;
        app.handle(Action::DeleteRow, 0);
        assert_eq!(app.channels[0].rows.len(), 1, "the row disappears at once");
        let commands = outbox_drained(&mut app);
        let SessionCommand::Delete { local, target, .. } = &commands[0] else {
            panic!()
        };
        assert_eq!(target, &mine);

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
    fn the_picker_opens_on_the_selection_and_confirm_switches_the_channel() {
        let mut app = app();
        let ids: Vec<Uuid> = (1..=3).map(|n| channel(n).id).collect();
        app.channels = vec![channel(1), channel(2), channel(3)];
        app.selected = 1;
        app.handle(Action::TogglePicker, 0);
        assert_eq!(app.picker, Some(1), "the picker opens on the selection");
        app.handle(Action::PickerNext, 0);
        app.handle(Action::PickerNext, 0);
        app.handle(Action::PickerNext, 0);
        assert_eq!(app.picker, Some(2), "the cursor clamps at the last channel");
        app.handle(Action::PickerConfirm, 0);
        assert_eq!(app.picker, None, "confirming closes the picker");
        assert_eq!(app.channels[app.selected].id, ids[2]);
        assert!(
            matches!(
                outbox_drained(&mut app)[..],
                [SessionCommand::OpenChannel(_)]
            ),
            "confirming opens the picked channel"
        );
    }

    #[test]
    fn the_overlays_replace_each_other_and_escape_closes_whichever_is_open() {
        let mut app = app();
        app.channels = vec![channel(1)];
        app.handle(Action::ToggleHelp, 0);
        assert!(app.help);
        // Opening the picker puts the help away: they never share the screen.
        app.handle(Action::TogglePicker, 0);
        assert_eq!(app.picker, Some(0));
        assert!(!app.help);
        app.handle(Action::Dismiss, 0);
        assert_eq!(app.picker, None);
        app.handle(Action::ToggleHelp, 0);
        app.handle(Action::Dismiss, 0);
        assert!(!app.help);
    }

    #[test]
    fn a_picker_without_channels_stays_closed() {
        let mut app = app();
        app.handle(Action::TogglePicker, 0);
        assert_eq!(app.picker, None);
    }

    #[test]
    fn a_channel_list_that_shrinks_pulls_the_picker_cursor_back() {
        let mut app = app();
        app.channels = vec![channel(1), channel(2), channel(3)];
        app.picker = Some(2);
        app.apply(
            ChatEvent::ChannelGone {
                channel: channel(3).id,
                reason: "restricted".into(),
            },
            0,
        );
        assert_eq!(app.picker, Some(1));
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

    #[test]
    fn a_reaction_removal_does_not_resurrect_itself_on_ok() {
        let mut app = app();
        app.channels = vec![channel(1)];
        let row_id = format!("{:0<64}", "a1");
        app.channels[0].rows = vec![row(&row_id, 1, &app.me.clone())];
        app.focus = 0;

        app.handle(Action::React, 0);
        let SessionCommand::React { local, .. } = &app.take_outbox()[0] else {
            panic!()
        };
        app.apply(
            ChatEvent::WriteOk {
                local: local.clone(),
                event_id: "b2".repeat(32),
            },
            0,
        );

        app.handle(Action::React, 0);
        match &app.take_outbox()[0] {
            SessionCommand::React {
                local,
                remove: Some(_),
                ..
            } => {
                // The removal succeeding must not re-record the reaction as
                // active: a third press adds again instead of removing twice.
                app.apply(
                    ChatEvent::WriteOk {
                        local: local.clone(),
                        event_id: "c3".repeat(32),
                    },
                    0,
                );
            }
            other => panic!("expected a removal, got {other:?}"),
        }
        assert!(
            !app.my_reaction
                .contains_key(&(row_id.clone(), content::DEFAULT_REACTION.to_owned()))
        );
        app.handle(Action::React, 0);
        match &app.take_outbox()[0] {
            SessionCommand::React { remove: None, .. } => {}
            other => panic!("a third press must add again, got {other:?}"),
        }
    }

    #[test]
    fn a_remote_removal_purges_the_toggle_entry() {
        let mut app = app();
        app.channels = vec![channel(1)];
        let row_id = format!("{:0<64}", "a1");
        let reaction_id = format!("{:0<64}", "e9");
        app.channels[0].rows = vec![row(&row_id, 1, "p")];
        app.reaction_of.insert(
            reaction_id.clone(),
            (row_id.clone(), content::DEFAULT_REACTION.to_owned()),
        );
        app.my_reaction.insert(
            (row_id.clone(), content::DEFAULT_REACTION.to_owned()),
            reaction_id.clone(),
        );

        let removal = delete_event(&reaction_id);
        app.apply(ChatEvent::Overlay(removal), 0);
        assert!(!app.reaction_of.contains_key(&reaction_id));
        assert!(
            !app.my_reaction
                .contains_key(&(row_id, content::DEFAULT_REACTION.to_owned()))
        );
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
