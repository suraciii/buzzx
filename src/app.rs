//! The TUI state machine: channels, rows, composer, focus, and the pending
//! lifecycle. No I/O happens here. Time arrives as an argument; commands
//! leave through the outbox for the session.

use std::collections::{HashMap, HashSet};

use nostr::Keys;
use uuid::Uuid;

use crate::agents;
use crate::client::{CatchUp, ChannelInfo, ChannelKind, Roster};
use crate::content::{self, Row};
use crate::keys::{self, Action, PAGE_ROWS};
use crate::session::{ChatEvent, SessionCommand};

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
    /// What the relay says this row is. It chooses the section the list shows
    /// it under.
    pub kind: ChannelKind,
    /// A DM's other members, in metadata order: the label a DM that carries no
    /// name of its own is shown under.
    pub participants: Vec<String>,
    pub rows: Vec<Row>,
    pub seen: HashSet<String>,
    /// Events delivered by the live feed but not yet covered by a history
    /// answer. They survive the HTTP/live race without preserving stale rows.
    live_ids: HashSet<String>,
    pub loading: bool,
    /// The draft this conversation kept when it was left, with the reply or
    /// edit target it belongs to. A draft must never be sent into another
    /// conversation by mistake.
    draft: Composer,
    /// Read frontier, unread candidates, and how much of either is known.
    read: ReadTrack,
}

/// One unread candidate: a message this identity has not been shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Candidate {
    at: u64,
    /// A `p` tag in the message names this identity.
    mention: bool,
}

/// Why a conversation's unread state may be missing rather than empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Coverage {
    /// Every query this conversation depends on answered, and its marker state
    /// is known.
    #[default]
    Complete,
    /// The marker may be known, but the catch-up query is still outstanding.
    Pending,
    /// The marker lookup or a slot's decode failed. Absence of a marker is
    /// then unknown, not read, and no seed may stand in for it.
    UnknownMarker,
    /// A query this conversation depends on failed or was truncated: unread
    /// may be missing rather than absent.
    Failed,
}

impl Coverage {
    /// Whether the Inbox can call this conversation read, rather than unknown.
    pub fn known(self) -> bool {
        self == Coverage::Complete
    }
}

/// What this identity has read in one conversation.
#[derive(Debug, Clone, Default)]
struct ReadTrack {
    /// The second read up to. A message at or after it is unread until it is
    /// presented: markers have second resolution, so the frontier's own second
    /// cannot vouch for messages this terminal never showed.
    frontier: u64,
    /// False while the frontier is a local seed. A seed tracks new messages and
    /// is never uploaded as proof of reading.
    marked: bool,
    /// The ids presented in the frontier's own second.
    presented: HashSet<String>,
    /// Whether the marker state for this conversation is known.
    known: bool,
    coverage: Coverage,
    /// Unread candidates by event id.
    unread: HashMap<String, Candidate>,
}

impl ReadTrack {
    /// Whether one message is unread here.
    fn unread_at(&self, id: &str, at: u64) -> bool {
        at >= self.frontier && !self.presented.contains(id)
    }

    /// Whether this conversation shows an unread signal at all.
    fn has_unread(&self) -> bool {
        !self.unread.is_empty()
    }

    /// Whether one of the unread candidates names this identity.
    fn mentioned(&self) -> bool {
        self.unread.values().any(|candidate| candidate.mention)
    }
}

/// The Inbox filter. All, Unread and For you are views of one list, not
/// separate stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Filter {
    /// Every eligible conversation.
    All,
    /// Conversations with known unread messages.
    Unread,
    /// Channels with unread direct mentions, and DMs with unread messages.
    ForYou,
}

impl Filter {
    /// The filter cycle `f` and Tab walk.
    pub const CYCLE: [Filter; 3] = [Filter::All, Filter::Unread, Filter::ForYou];

    pub fn name(self) -> &'static str {
        match self {
            Filter::All => "All",
            Filter::Unread => "Unread",
            Filter::ForYou => "For you",
        }
    }

    fn next(self) -> Filter {
        let at = Filter::CYCLE.iter().position(|f| *f == self).unwrap_or(0);
        Filter::CYCLE[(at + 1) % Filter::CYCLE.len()]
    }
}

/// The picker's own state: a cursor tied to a conversation, so a filter change
/// or a removal cannot move it onto a different conversation.
#[derive(Debug, Clone, PartialEq)]
pub struct Picker {
    /// The conversation under the cursor.
    pub cursor: Uuid,
    /// The row it stood on in the view, so a conversation that disappears
    /// hands the row to its neighbor instead of moving the cursor elsewhere.
    pub at: usize,
}

/// A filter's order: sections decide where a row is shown, and first-seen order
/// decides where it sits within one. New qualifying conversations append to
/// their section, so a new message never reorders the rows already there.
#[derive(Debug, Clone, Default)]
pub struct Sections {
    pub channels: Vec<Uuid>,
    pub dms: Vec<Uuid>,
}

impl Sections {
    fn all(&self) -> impl Iterator<Item = Uuid> + '_ {
        self.channels.iter().chain(self.dms.iter()).copied()
    }

    fn contains(&self, id: Uuid) -> bool {
        self.channels.contains(&id) || self.dms.contains(&id)
    }

    /// Append one conversation to the section it belongs to.
    fn push(&mut self, kind: ChannelKind, id: Uuid) {
        if kind == ChannelKind::Dm {
            self.dms.push(id);
        } else {
            self.channels.push(id);
        }
    }
}

/// The empty view, for a filter that has no order of its own yet.
static EMPTY_SECTIONS: Sections = Sections {
    channels: Vec::new(),
    dms: Vec::new(),
};

/// The signal one conversation's row carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Marker {
    /// Known unread messages, none of them a direct mention.
    Unread,
    /// An unread message that directly mentions this identity.
    Mention,
    /// Unread state unknown: the row says so instead of claiming zero.
    Unknown,
    /// Focused, and kept in a filtered view it no longer matches.
    Read,
    /// Nothing unread here.
    None,
}

impl Marker {
    /// The glyph, or the word a retained row carries.
    pub fn text(self) -> &'static str {
        match self {
            Marker::Unread => "●",
            Marker::Mention => "@",
            Marker::Unknown => "?",
            Marker::Read => "Read",
            Marker::None => "",
        }
    }
}

/// The names the relay gives a DM when the identity named nothing of its own.
/// The list shows the people in it instead.
fn generic_dm_name(name: &str) -> bool {
    name == "DM" || name == "Direct Messages" || name.starts_with("Group DM (")
}

/// How many participants a DM label names before it counts the rest.
const DM_LABEL_LIMIT: usize = 3;
/// How many conversations keep a numeric shortcut.
const SHORTCUTS: usize = 9;

impl ChannelEntry {
    /// Whether this conversation is shown under the DMs heading. A row the
    /// relay has not described is not claimed to be one.
    pub fn is_dm(&self) -> bool {
        self.kind == ChannelKind::Dm
    }
}

impl Filter {
    /// Whether one conversation belongs in this view. An unknown conversation
    /// is never counted as unread: the view says it is unknown instead.
    fn matches(self, entry: &ChannelEntry) -> bool {
        match self {
            Filter::All => true,
            Filter::Unread => entry.read.has_unread(),
            // A channel qualifies on an unread direct mention only: an ordinary
            // channel message is not personal attention. A DM qualifies on any
            // unread message, and leaves the view once it has been read - the
            // view answers "what needs reading", not "what is a DM".
            Filter::ForYou => (entry.is_dm() && entry.read.has_unread()) || entry.read.mentioned(),
        }
    }
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

/// What one Agent's row says. The four states stay separate on purpose: only
/// one of them is evidence that work is running, and only one of them is
/// evidence that it is not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentStatus {
    /// This many turns are observed running.
    Working(usize),
    /// Only channel typing is observed, in this channel.
    Typing(Uuid),
    /// The feed is live and no turn is observed. Never a claim of idleness.
    NoTurn,
    /// Nothing is observed at all: no connection, no feed, no fresh evidence.
    Unknown,
}

/// One observed working context of the selected Agent. Only a context the user
/// can already open names its channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Context {
    Channel {
        id: Uuid,
        name: String,
    },
    /// Observed work in a conversation this identity is not in. The name, the
    /// id and the content stay hidden; the fact of the work does not.
    Unavailable,
    /// The frame carried no conversation: a scheduled turn, not a reply.
    Unknown,
}

/// The Agents overlay: the owned roster, the observed work behind it, and the
/// cursor. The state outlives the overlay, so reopening it is instant.
#[derive(Debug, Default)]
pub struct AgentView {
    pub roster: agents::Load,
    pub work: agents::Work,
    pub cursor: agents::View,
    /// Whether the observer feed is live on this connection. False means
    /// `Unknown`: with no observation there is no absence to report.
    pub observed: bool,
    /// Whether the overlay is on screen.
    pub open: bool,
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
    /// How far the help text is scrolled: at the minimum size the text is
    /// taller than the screen, and the whole of it must stay reachable.
    pub help_scroll: u16,
    /// The Inbox filter in effect. The sidebar and the picker both show it.
    pub filter: Filter,
    /// The order each filtered view shows conversations in.
    views: HashMap<Filter, Sections>,
    /// The conversations the numeric shortcuts point at, in the order they
    /// were first assigned. A slot is never reused: a number keeps its
    /// conversation for the session, whatever a filter hides.
    shortcuts: Vec<Uuid>,
    /// The channel picker's state while it is open. Compact layouts open it
    /// with `c`; the picker is the only channel list they show.
    pub picker: Option<Picker>,
    /// Whether the relay described every roster row. False means the list is a
    /// floor, not the whole answer.
    pub roster_complete: bool,
    /// Whether the last marker lookup answered in full. False makes every
    /// frontier unknown: absence of a marker is not absence.
    marker_complete: bool,
    /// Whether the marker lookup has answered at all this session.
    marker_read: bool,
    /// Why the last read publish failed, until one succeeds. A publish still
    /// in flight does not clear it: the relay does not know either way yet.
    read_failed: Option<String>,
    /// Conversations waiting for a marker answer before their catch-up can be
    /// asked for.
    catch_up_pending: Vec<Uuid>,
    /// Inbox feeds that closed before their roster entry existed, or while
    /// their current entry was loaded. A reconnect clears these stale
    /// connection failures.
    inbox_failed: HashSet<Uuid>,
    /// Conversations whose history request failed. A later catch-up or seed
    /// cannot make the selected timeline complete; a successful History event
    /// clears the failure.
    history_failed: HashSet<Uuid>,
    pub conn: ConnState,
    pub status: String,
    pub composer: Composer,
    /// The owned Agents and what the observer feed says about their work.
    pub agents: AgentView,
    pub quit: bool,
    pub exit_code: i32,
    profiles: HashMap<String, String>,
    /// Live typing indicators, per channel: pubkey -> the second the entry
    /// expires. Ephemeral state, rebuilt from the current connection only.
    typing: HashMap<Uuid, HashMap<String, TypingEntry>>,
    seen_aux: HashSet<String>,
    /// Auxiliary events applied to each conversation. Reloading one history
    /// invalidates only that conversation's overlay deduplication.
    aux_seen: HashMap<Uuid, HashSet<String>>,
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
            help_scroll: 0,
            filter: Filter::All,
            views: HashMap::new(),
            shortcuts: Vec::new(),
            picker: None,
            roster_complete: false,
            inbox_failed: HashSet::new(),
            history_failed: HashSet::new(),
            marker_complete: false,
            marker_read: false,
            read_failed: None,
            catch_up_pending: Vec::new(),
            conn: ConnState::Connecting,
            status: "connecting".to_owned(),
            composer: Composer::new(),
            agents: AgentView::default(),
            quit: false,
            exit_code: 0,
            profiles: HashMap::new(),
            typing: HashMap::new(),
            seen_aux: HashSet::new(),
            aux_seen: HashMap::new(),
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

    /// Which overlay is on screen, for the key map.
    pub fn overlay(&self) -> keys::Overlay {
        if self.help {
            keys::Overlay::Help
        } else if self.picker.is_some() {
            keys::Overlay::Picker
        } else if self.agents.open {
            keys::Overlay::Agents
        } else {
            keys::Overlay::None
        }
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

    /// Retire turns no frame has refreshed inside the freshness bound. Work
    /// has no terminating event when its host dies, so the frame tick is what
    /// ends it, exactly like a typing indicator nobody refreshed.
    pub fn expire_agents(&mut self, now: u64) {
        if self.agents.work.expire(now) {
            self.clamp_agents();
        }
    }

    /// What one Agent's row says.
    pub fn agent_status(&self, agent: &str, now: u64) -> AgentStatus {
        if !self.agents.observed {
            return AgentStatus::Unknown;
        }
        let working = self.agents.work.working(agent).len();
        if working > 0 {
            return AgentStatus::Working(working);
        }
        match self.typing_channel_of(agent, now) {
            Some(channel) => AgentStatus::Typing(channel),
            None => AgentStatus::NoTurn,
        }
    }

    /// The selected Agent's observed working contexts, ordered the way the
    /// frames arrived: newest activity first. A context outside this identity's
    /// channels keeps its place in the list without giving up its name.
    pub fn agent_contexts(&self, agent: &str) -> Vec<Context> {
        self.agents
            .work
            .working(agent)
            .into_iter()
            .map(|(_, turn)| match turn.channel.as_deref() {
                Some(id) => match Uuid::parse_str(id)
                    .ok()
                    .and_then(|id| self.channels.iter().find(|entry| entry.id == id))
                {
                    Some(entry) => Context::Channel {
                        id: entry.id,
                        name: entry.name.clone(),
                    },
                    None => Context::Unavailable,
                },
                None => Context::Unknown,
            })
            .collect()
    }

    /// When the last frame from this Agent arrived, on this machine's clock.
    pub fn agent_last_signal(&self, agent: &str) -> Option<u64> {
        self.agents.work.last_signal(agent)
    }

    /// When this Agent's last observed turn failed, if any did.
    pub fn agent_last_failure(&self, agent: &str) -> Option<u64> {
        self.agents.work.last_failure(agent)
    }

    /// How many Agents the loaded roster holds.
    pub fn agent_count(&self) -> usize {
        match &self.agents.roster {
            agents::Load::Loaded(roster) => roster.agents.len(),
            _ => 0,
        }
    }

    /// The roster entry the cursor is on.
    pub fn selected_agent(&self) -> Option<agents::OwnedAgent> {
        match &self.agents.roster {
            agents::Load::Loaded(roster) => roster.agents.get(self.agents.cursor.cursor).cloned(),
            _ => None,
        }
    }

    /// The channel one identity is composing in, if it composes at all.
    fn typing_channel_of(&self, pubkey: &str, now: u64) -> Option<Uuid> {
        self.channels
            .iter()
            .find(|entry| {
                self.typing.get(&entry.id).is_some_and(|entries| {
                    entries
                        .get(pubkey)
                        .is_some_and(|entry| entry.expires_at > now)
                })
            })
            .map(|entry| entry.id)
    }

    /// Keep the overlay's cursor on an entry that still exists. The list is
    /// loaded once and the work under it churns, so both levels can shrink.
    fn clamp_agents(&mut self) {
        let agents = self.agent_count();
        let contexts = self
            .selected_agent()
            .map(|agent| self.agent_contexts(&agent.pubkey).len())
            .unwrap_or(0);
        self.agents.cursor.clamp(agents, contexts);
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
        // Focus is the scroll position, so landing on the newest row is what
        // presenting the latest message means.
        self.note_presented();
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
            .flat_map(|entry| {
                entry
                    .rows
                    .iter()
                    .map(|row| row.pubkey.clone())
                    // A DM's label is its participants, and that label is
                    // needed before any of its history is loaded: the roster
                    // alone has to ask for them, or an unopened DM shows a
                    // short key where a name belongs.
                    .chain(entry.participants.iter().cloned())
            })
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
                self.inbox_failed.clear();
                self.conn = ConnState::Connected;
                // The observer feed has to be established on this connection
                // before a quiet Agent means anything: until then this client
                // has not been told the relay is listening, and work seen on
                // the previous connection went with it.
                self.agents.observed = false;
                self.agents.work.clear();
                self.status = format!("connected {}", self.relay_label);
                if !self.opened_once {
                    self.outbox.push(SessionCommand::LoadChannels);
                } else {
                    // A reconnect is a full resubscribe: refresh membership,
                    // replace the selected channel's history, and check every
                    // listed conversation for what it received meanwhile. A
                    // live feed alone would only see what it was connected
                    // for.
                    self.catch_up_pending = self.channels.iter().map(|entry| entry.id).collect();
                    let opening = self.selected_entry().map(|entry| entry.id);
                    self.outbox.push(SessionCommand::LoadChannels);
                    if let Some(id) = opening {
                        self.outbox.push(SessionCommand::OpenChannel(id));
                    }
                    // The roster read is a plain query, and an owner can add
                    // an Agent while this client is away.
                    if self.agents.open {
                        self.outbox.push(SessionCommand::LoadAgents);
                    }
                }
            }
            ChatEvent::Disconnected(reason) => {
                self.conn = ConnState::Reconnecting;
                // Typing and Inbox feeds both belonged to the connection that
                // ended. Until catch-up completes again, a quiet row is not a
                // claim that no message arrived while it was away.
                self.typing.clear();
                // The observer feed ended with the same connection: with no
                // live feed, an Agent that was working is unknown, not idle.
                self.agents.observed = false;
                self.agents.work.clear();
                for entry in &mut self.channels {
                    entry.read.coverage = Coverage::Failed;
                }
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
                // A DM is labelled by its participants, and that label is on
                // screen before the conversation is ever opened, so the names
                // are wanted as soon as the roster names the people.
                self.request_profiles();
            }
            ChatEvent::ReadState { contexts, complete } => {
                self.apply_read_state(&contexts, complete);
            }
            ChatEvent::CatchUp {
                channel,
                events,
                complete,
            } => {
                self.apply_catch_up(channel, events, complete);
            }
            ChatEvent::Seed {
                channel,
                latest,
                complete,
            } => {
                self.apply_seed(channel, latest, complete);
            }
            ChatEvent::InboxTimeline { channel, event } => {
                self.apply_inbox_timeline(channel, event);
            }
            ChatEvent::InboxClosed { channel, reason } => {
                self.inbox_failed.insert(channel);
                self.mark_failed(channel);
                self.status = format!("inbox feed closed: {reason}");
            }
            ChatEvent::ReadPublished { ok, reason } => {
                // Success is the only thing that clears the note: a failure
                // that is retried later is still a relay that was not told.
                self.read_failed = if ok { None } else { Some(reason) };
            }
            ChatEvent::History { channel, events } => {
                self.load_history(channel, events, now);
            }
            ChatEvent::HistoryFailed { channel, reason } => {
                self.history_failed(channel, reason);
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
                let at = self
                    .channels
                    .iter()
                    .position(|entry| entry.id == channel)
                    .unwrap_or(self.selected);
                self.channels.retain(|c| c.id != channel);
                self.typing.remove(&channel);
                // Its cached content went with it, and so did its draft: the
                // composer takes what the surviving conversation has, so no
                // text can cross over.
                self.selected = at.min(self.channels.len().saturating_sub(1));
                self.adopt_draft();
                self.refresh_views();
                self.set_focus(self.focus);
                self.clamp_picker();
                self.status = format!("channel closed: {reason}");
                if let Some(entry) = self.channels.get(self.selected) {
                    let id = entry.id;
                    self.outbox.push(SessionCommand::OpenChannel(id));
                }
            }
            ChatEvent::Agents(load) => {
                self.agents.roster = load;
                self.clamp_agents();
            }
            ChatEvent::ObserverReady => self.agents.observed = true,
            ChatEvent::ObserverFrame(frame) => {
                // A frame is the feed working too: it is the same observation,
                // and it stands on its own if the relay's confirmation is lost.
                self.agents.observed = true;
                self.agents.work.apply(&frame, now);
                self.clamp_agents();
            }
            ChatEvent::ObserverClosed { reason } => {
                // No frame observed after this point is evidence that an Agent
                // is idle, so nothing observed before it is current either.
                self.agents.observed = false;
                self.agents.work.clear();
                self.clamp_agents();
                self.status = format!("agent observer feed closed: {reason}");
            }
            ChatEvent::Status(message) => self.status = message,
            ChatEvent::WriteOk { local, event_id } => self.complete_write(local, event_id),
            ChatEvent::WriteFailed { local, reason } => self.fail_write(local, reason),
            ChatEvent::WriteUncertain { local, reason } => self.uncertain_write(local, reason),
        }
    }

    /// Fold the marker answer in. A frontier the relay already holds is a read
    /// claim; a conversation the answer does not name has no marker, and
    /// starts from a local baseline instead of from nothing. An incomplete
    /// answer leaves every conversation unknown rather than read.
    fn apply_read_state(&mut self, contexts: &HashMap<String, u64>, complete: bool) {
        self.marker_read = true;
        self.marker_complete = complete;
        for entry in &mut self.channels {
            let waiting = self.catch_up_pending.contains(&entry.id);
            let remote = contexts.get(&entry.id.to_string()).copied();
            if self.inbox_failed.contains(&entry.id) || self.history_failed.contains(&entry.id) {
                continue;
            }
            match remote.filter(|_| complete) {
                None if !complete => {
                    // A failed lookup is unknown, not absence: no seed stands
                    // in for it, and the row keeps saying it does not know.
                    entry.read.coverage = Coverage::UnknownMarker;
                }
                None => {
                    // No marker: the baseline comes from the newest message.
                    // Keep it pending until that answer arrives.
                    if waiting {
                        entry.read.coverage = Coverage::Pending;
                    } else if entry.read.coverage == Coverage::UnknownMarker {
                        entry.read.coverage = Coverage::Complete;
                    }
                }
                Some(at) => {
                    entry.read.frontier = entry.read.frontier.max(at);
                    entry.read.marked = true;
                    entry.read.coverage = if waiting {
                        Coverage::Pending
                    } else {
                        Coverage::Complete
                    };
                    entry.read.known = true;
                    // A marker has second resolution: the messages of its own
                    // second are unread here until this terminal shows them.
                    entry.read.presented.clear();
                    let frontier = entry.read.frontier;
                    entry
                        .read
                        .unread
                        .retain(|_, candidate| candidate.at >= frontier);
                }
            }
        }
        self.request_catch_up();
        self.refresh_views();
        // The answer that makes a frontier claimable can arrive after the
        // conversation was already shown, which is the normal cold start: the
        // screen did not change, but what may be claimed about it did.
        self.note_presented();
    }

    /// Fold one conversation's catch-up in. What this identity wrote, and what
    /// it has already been shown, is not unread work.
    fn apply_catch_up(&mut self, channel: Uuid, events: Vec<nostr::Event>, complete: bool) {
        let me = self.me.clone();
        let feed_failed =
            self.inbox_failed.contains(&channel) || self.history_failed.contains(&channel);
        let Some(entry) = self.entry_mut(&channel) else {
            return;
        };
        entry.read.known = true;
        entry.read.coverage = if complete && !feed_failed {
            Coverage::Complete
        } else {
            Coverage::Failed
        };
        for event in events {
            if event.pubkey.to_hex() == me {
                continue;
            }
            let id = event.id.to_hex();
            let at = event.created_at.as_secs();
            if !entry.read.unread_at(&id, at)
                || entry.seen.contains(&id)
                || entry.read.unread.contains_key(&id)
            {
                continue;
            }
            let candidate = Candidate {
                at,
                mention: content::mentions_me(&event, &me),
            };
            entry.read.unread.insert(id, candidate);
        }
        self.refresh_views();
        // The catch-up answer is what turns a conversation's coverage into
        // something a frontier may rest on, and it can land after the
        // conversation is already on screen.
        self.note_presented();
    }

    /// Start tracking a conversation that has no marker yet: the newest
    /// message is the baseline, not unread work. The seed is local and is
    /// never uploaded as proof of reading.
    fn apply_seed(&mut self, channel: Uuid, latest: Option<nostr::Event>, complete: bool) {
        let feed_failed =
            self.inbox_failed.contains(&channel) || self.history_failed.contains(&channel);
        let marker_unknown = !self.marker_read || !self.marker_complete;
        let Some(entry) = self.entry_mut(&channel) else {
            return;
        };
        entry.read.coverage = if feed_failed {
            Coverage::Failed
        } else if marker_unknown {
            Coverage::UnknownMarker
        } else if complete {
            Coverage::Complete
        } else {
            Coverage::Failed
        };
        if !complete || feed_failed || marker_unknown {
            return;
        }
        entry.read.known = true;
        entry.read.marked = false;
        let Some(event) = latest else {
            // An empty conversation starts tracking what comes next, and
            // invents nothing.
            return;
        };
        let at = event.created_at.as_secs();
        entry.read.frontier = entry.read.frontier.max(at);
        entry.read.presented.clear();
        entry.read.presented.insert(event.id.to_hex());
        let frontier = entry.read.frontier;
        let presented = entry.read.presented.clone();
        // A live event can arrive before the seed answer. Drop candidates the
        // seed proves were already present, but retain same-second events that
        // the one-message baseline cannot distinguish.
        entry
            .read
            .unread
            .retain(|id, candidate| candidate.at >= frontier && !presented.contains(id));
        self.refresh_views();
        // A conversation with no marker starts from this seed, and the
        // conversation may already be the one on screen when it lands.
        self.note_presented();
    }

    /// One live message for a conversation nobody has open. The conversation on
    /// screen is owned by its own timeline feed: counting the same event here
    /// as well would mark unread what the reader is looking at.
    fn apply_inbox_timeline(&mut self, channel: Uuid, event: nostr::Event) {
        if self.selected_entry().map(|entry| entry.id) == Some(channel) {
            return;
        }
        let me = self.me.clone();
        match u32::from(event.kind.as_u16()) {
            content::EDIT_KIND => {
                self.apply_candidate_edit(channel, &event, &me);
                return;
            }
            content::DELETE_KIND | content::TOMBSTONE_KIND => {
                let classified = content::overlay_of(&event, &|_: &str| None);
                if let Some((target, content::Overlay::Delete)) = classified
                    && let Some(entry) = self.entry_mut(&channel)
                {
                    entry.read.unread.remove(&target);
                    self.refresh_views();
                }
                return;
            }
            kind if content::inbox_kinds().contains(&kind) => {}
            // The feed asked for these kinds and no others. A relay that sends
            // something else does not get to invent an unread message: a
            // reaction is not a timeline event and never becomes a candidate.
            _ => return,
        }
        if event.pubkey.to_hex() == me {
            return;
        }
        let id = event.id.to_hex();
        let at = event.created_at.as_secs();
        let mention = content::mentions_me(&event, &me);
        let Some(entry) = self.entry_mut(&channel) else {
            return;
        };
        if entry.seen.contains(&id) || entry.read.unread.contains_key(&id) {
            return;
        }
        if !entry.read.unread_at(&id, at) {
            return;
        }
        entry.read.unread.insert(id, Candidate { at, mention });
        self.refresh_views();
    }

    /// An edit can change whether an already unread message mentions this
    /// identity. An edit that carries no `p` tag says nothing about mentions,
    /// and editing a message that is already read creates no new work.
    fn apply_candidate_edit(&mut self, channel: Uuid, event: &nostr::Event, me: &str) {
        let Some(mention) = edit_mention(event, me) else {
            return;
        };
        let classified = content::overlay_of(event, &|_: &str| None);
        let Some((target, content::Overlay::Edit { .. })) = classified else {
            return;
        };
        if let Some(candidate) = self
            .entry_mut(&channel)
            .and_then(|entry| entry.read.unread.get_mut(&target))
        {
            candidate.mention = mention;
        }
    }

    /// A query this conversation depends on failed: its unread state may be
    /// missing rather than empty, and it says so instead of claiming zero.
    fn mark_failed(&mut self, channel: Uuid) {
        if let Some(entry) = self.entry_mut(&channel) {
            entry.read.coverage = Coverage::Failed;
        }
    }

    /// Advance the read frontier when the conversation is on screen, loaded,
    /// and sitting at its latest message. Reading older history, previewing the
    /// picker and a failed load all leave it where it was.
    pub fn note_presented(&mut self) {
        if self.picker.is_some() || self.help || self.agents.open {
            // The list covers the conversation: nothing is being read.
            return;
        }
        let Some(entry) = self.channels.get(self.selected) else {
            return;
        };
        if !self.marker_read
            || !entry.read.known
            || !entry.read.coverage.known()
            || entry.loading
            || entry.rows.is_empty()
            || self.focus + 1 != entry.rows.len()
        {
            return;
        }
        let latest = entry
            .rows
            .iter()
            .filter(|row| !row.pending)
            .map(|row| row.created_at)
            .max();
        let Some(latest) = latest else {
            return;
        };
        let presented: Vec<String> = entry
            .rows
            .iter()
            .filter(|row| !row.pending && row.created_at == latest)
            .map(|row| row.event_id.clone())
            .collect();
        let entry = &mut self.channels[self.selected];
        let advanced = latest > entry.read.frontier;
        let claim = !entry.read.marked;
        // A candidate inside the frontier's own second was never vouched for by
        // the marker, so presenting it clears it even though the frontier does
        // not move.
        let pending = entry
            .read
            .unread
            .values()
            .any(|candidate| candidate.at >= entry.read.frontier);
        if !advanced && !claim && !pending {
            return;
        }
        if advanced {
            entry.read.frontier = latest;
            entry.read.presented.clear();
        } else if claim {
            // A conversation with no marker starts from a seed at the newest
            // message the feed named, which is not on screen until its own
            // history loads. A claim that starts from a seed therefore reaches
            // only as far as the newest row the reader was shown: anything the
            // baseline knows about and the timeline did not deliver stays
            // unread, because nothing presented it.
            entry.read.frontier = entry.read.frontier.min(latest);
        }
        for id in presented {
            if entry.read.unread_at(&id, latest) {
                entry.read.presented.insert(id);
            }
        }
        let frontier = entry.read.frontier;
        let presented = entry.read.presented.clone();
        entry
            .read
            .unread
            .retain(|id, candidate| candidate.at >= frontier && !presented.contains(id));
        // The messages were on screen, so this is a read claim now - even when
        // it started as a local seed.
        entry.read.marked = true;
        self.refresh_views();
        self.request_publish();
    }

    /// Ask the session to publish what this terminal has read. A frontier that
    /// is only a local seed is not a read claim, and stays out of it.
    fn request_publish(&mut self) {
        let contexts: HashMap<String, u64> = self
            .channels
            .iter()
            .filter(|entry| entry.read.marked && entry.read.frontier > 0)
            .map(|entry| (entry.id.to_string(), entry.read.frontier))
            .collect();
        if !contexts.is_empty() {
            self.outbox.push(SessionCommand::ReadProgress { contexts });
        }
    }

    /// Ask for what each listed conversation holds at or after its frontier. A
    /// conversation the marker answer did not describe starts from a local
    /// seed; one whose marker read failed waits for a real answer instead of
    /// pretending the marker was absent.
    fn request_catch_up(&mut self) {
        if !self.marker_read {
            return;
        }
        let pending = std::mem::take(&mut self.catch_up_pending);
        let mut retry = Vec::new();
        let requests: Vec<CatchUp> = pending
            .into_iter()
            .filter_map(|id| {
                let entry = self.channels.iter().find(|entry| entry.id == id)?;
                if entry.read.coverage == Coverage::UnknownMarker {
                    // Preserve this id until a later complete marker answer;
                    // otherwise a transient read-state failure strands it.
                    retry.push(id);
                    return None;
                }
                Some(if entry.read.known {
                    CatchUp::Since {
                        channel: id,
                        since: entry.read.frontier,
                    }
                } else {
                    CatchUp::Newest { channel: id }
                })
            })
            .collect();
        self.catch_up_pending.extend(retry);
        if !requests.is_empty() {
            self.outbox.push(SessionCommand::CatchUp(requests));
        }
    }

    /// Fold a new roster into the list. A conversation keeps its order, its
    /// loaded rows and its draft; a conversation the relay no longer lists -
    /// hidden, archived, or gone - takes its cached content with it. A
    /// conversation seen for the first time appends within its section.
    fn merge_channels(&mut self, roster: Roster) {
        let selection = self.channels.get(self.selected).map(|entry| entry.id);
        let at = self.selected;
        let listed: Vec<ChannelInfo> = roster
            .items
            .into_iter()
            .filter(|item| item.listed())
            .collect();
        let mut previous: HashMap<Uuid, ChannelEntry> = std::mem::take(&mut self.channels)
            .into_iter()
            .map(|entry| (entry.id, entry))
            .collect();
        let mut merged: Vec<ChannelEntry> = Vec::with_capacity(listed.len());
        // Section by section, and within a section in the order the list
        // already had: a row whose description arrives later moves to its
        // section's end rather than reordering the rest.
        for dm in [false, true] {
            for item in listed
                .iter()
                .filter(|item| (item.kind == ChannelKind::Dm) == dm)
            {
                let Some(mut entry) = previous.remove(&item.id) else {
                    // First sight: it has no read state and no history, so it
                    // waits for a marker answer before its catch-up is asked
                    // for.
                    self.catch_up_pending.push(item.id);
                    let coverage = if self.inbox_failed.contains(&item.id)
                        || self.history_failed.contains(&item.id)
                    {
                        Coverage::Failed
                    } else if self.marker_read {
                        if self.marker_complete {
                            Coverage::Pending
                        } else {
                            Coverage::UnknownMarker
                        }
                    } else {
                        Coverage::Complete
                    };
                    merged.push(ChannelEntry {
                        id: item.id,
                        name: item.name.clone(),
                        kind: item.kind,
                        participants: item.participants.clone(),
                        rows: Vec::new(),
                        seen: HashSet::new(),
                        live_ids: HashSet::new(),
                        loading: true,
                        draft: Composer::new(),
                        read: ReadTrack {
                            coverage,
                            ..ReadTrack::default()
                        },
                    });
                    continue;
                };
                entry.name = item.name.clone();
                entry.kind = item.kind;
                entry.participants = item.participants.clone();
                merged.push(entry);
            }
        }
        self.channels = merged;
        self.roster_complete = roster.complete;
        self.assign_shortcuts();
        self.refresh_views();
        // The selection is a conversation, not a row: the list can be rebuilt
        // under it, and a conversation that is gone hands its place to its
        // next surviving neighbor, or to the previous one at the end.
        match selection.and_then(|id| self.index_of(id)) {
            Some(found) => {
                let moved = found != self.selected;
                self.selected = found;
                if moved {
                    self.adopt_draft();
                }
            }
            None => {
                self.selected = at.min(self.channels.len().saturating_sub(1));
                self.adopt_draft();
            }
        }
        if self.selected < self.channels.len() {
            self.set_focus(self.focus);
        }
        self.clamp_picker();
    }

    /// Give the first conversations their numeric shortcut. A conversation
    /// keeps the number it was given for the whole session: a filter that
    /// hides it, or a roster that drops it, does not hand its number to
    /// someone else.
    fn assign_shortcuts(&mut self) {
        for entry in &self.channels {
            if self.shortcuts.len() >= SHORTCUTS {
                return;
            }
            if !self.shortcuts.contains(&entry.id) {
                self.shortcuts.push(entry.id);
            }
        }
    }

    /// Rebuild each view's order. Conversations that still belong keep their
    /// place; conversations that qualify now append to their section, so a new
    /// message never reorders the rows already on screen.
    pub fn refresh_views(&mut self) {
        let mut views: HashMap<Filter, Sections> = HashMap::new();
        for filter in Filter::CYCLE {
            let mut order = Sections::default();
            if filter == Filter::All {
                // All is the list: its order is the roster's.
                for entry in &self.channels {
                    order.push(entry.kind, entry.id);
                }
                views.insert(filter, order);
                continue;
            }
            let previous = self.views.remove(&filter).unwrap_or_default();
            for id in previous.all() {
                if let Some(entry) = self.channels.iter().find(|c| c.id == id)
                    && filter.matches(entry)
                {
                    order.push(entry.kind, id);
                }
            }
            for entry in &self.channels {
                if filter.matches(entry) && !order.contains(entry.id) {
                    order.push(entry.kind, entry.id);
                }
            }
            views.insert(filter, order);
        }
        self.views = views;
    }

    /// Number the conversations in list order and rebuild the views, the way
    /// the roster's arrival does, for tests that build the list by hand.
    #[cfg(test)]
    pub fn stub_roster(&mut self) {
        self.shortcuts = self.channels.iter().map(|entry| entry.id).collect();
        self.refresh_views();
    }

    /// A conversation with no history and a known read state, for tests
    /// outside this module that need a row and not a scenario.
    #[cfg(test)]
    pub fn stub_entry(id: Uuid, name: &str) -> ChannelEntry {
        ChannelEntry {
            id,
            name: name.to_owned(),
            kind: ChannelKind::Channel,
            participants: Vec::new(),
            rows: Vec::new(),
            seen: HashSet::new(),
            live_ids: HashSet::new(),
            loading: false,
            draft: Composer::default(),
            read: ReadTrack {
                known: true,
                ..ReadTrack::default()
            },
        }
    }

    /// One conversation by identity.
    pub fn entry(&self, id: Uuid) -> Option<&ChannelEntry> {
        self.channels.iter().find(|entry| entry.id == id)
    }

    /// The conversation the user is looking at: the picker's cursor when the
    /// picker is open, and the conversation on screen otherwise.
    pub fn focused_entry(&self) -> Option<&ChannelEntry> {
        match &self.picker {
            Some(picker) => self.entry(picker.cursor),
            None => self.channels.get(self.selected),
        }
    }

    /// Whether the Inbox can promise it is showing every conversation it could:
    /// a roster that could not be described, or a read state it could not read,
    /// makes the list a floor rather than an answer.
    pub fn inbox_incomplete(&self) -> bool {
        !self.marker_read
            || !self.roster_complete
            || self
                .channels
                .iter()
                .any(|entry| !entry.read.coverage.known())
    }

    /// The conversations the active filter shows, in list order.
    pub fn view(&self) -> &Sections {
        self.views.get(&self.filter).unwrap_or(&EMPTY_SECTIONS)
    }

    /// The conversations the picker shows: the filter's matches, plus the
    /// conversation under the cursor when it no longer matches, so the row the
    /// user is looking at never vanishes from under them. The row is inserted
    /// where the full list would put it.
    pub fn picker_view(&self) -> Sections {
        let mut view = self.view().clone();
        let Some(picker) = &self.picker else {
            return view;
        };
        if view.contains(picker.cursor) {
            return view;
        }
        let Some(entry) = self.channels.iter().find(|c| c.id == picker.cursor) else {
            return view;
        };
        let cursor_at = self.index_of(picker.cursor).unwrap_or(usize::MAX);
        let section = if entry.is_dm() {
            &mut view.dms
        } else {
            &mut view.channels
        };
        let at = section
            .iter()
            .position(|id| self.index_of(*id).unwrap_or(usize::MAX) > cursor_at)
            .unwrap_or(section.len());
        section.insert(at, picker.cursor);
        view
    }

    /// Where one conversation sits in the full list.
    pub fn index_of(&self, id: Uuid) -> Option<usize> {
        self.channels.iter().position(|entry| entry.id == id)
    }

    /// What the list calls one conversation. A DM whose own name says
    /// something keeps it; the relay's generic names become the people in it,
    /// and a participant without a profile falls back to its short key.
    pub fn label(&self, entry: &ChannelEntry) -> String {
        if !entry.is_dm() || !generic_dm_name(&entry.name) {
            return entry.name.clone();
        }
        let mut names: Vec<String> = entry
            .participants
            .iter()
            .filter(|pubkey| **pubkey != self.me)
            .map(|pubkey| author_name(&self.profiles, &self.me, pubkey))
            .collect();
        names.sort();
        names.dedup();
        if names.is_empty() {
            return entry.name.clone();
        }
        let extra = names.len().saturating_sub(DM_LABEL_LIMIT);
        let mut label = names
            .iter()
            .take(DM_LABEL_LIMIT)
            .cloned()
            .collect::<Vec<String>>()
            .join(", ");
        if extra > 0 {
            label.push_str(&format!(" +{extra} more"));
        }
        label
    }

    /// The numeric shortcut one conversation answers to, if it has one.
    pub fn shortcut(&self, id: Uuid) -> Option<usize> {
        self.shortcuts
            .iter()
            .position(|assigned| *assigned == id)
            .map(|at| at + 1)
    }

    /// The conversation one numeric shortcut names, whether or not the active
    /// filter shows it.
    fn shortcut_target(&self, number: usize) -> Option<Uuid> {
        self.shortcuts.get(number.saturating_sub(1)).copied()
    }

    /// The signal one conversation's row carries. An unknown conversation says
    /// so instead of claiming it has nothing unread.
    pub fn marker(&self, entry: &ChannelEntry) -> Marker {
        if !self.marker_read || !entry.read.coverage.known() {
            return Marker::Unknown;
        }
        if entry.read.mentioned() {
            return Marker::Mention;
        }
        if entry.read.has_unread() {
            return Marker::Unread;
        }
        if !self.filter.matches(entry) {
            // Kept in a filtered view it no longer matches. The row says it
            // was read only when a read claim exists: a conversation that was
            // never unread here is not called read.
            return if entry.read.marked {
                Marker::Read
            } else {
                Marker::None
            };
        }
        Marker::None
    }

    /// How many messages stand after the frontier. Zero for a row that is not
    /// claiming unread: a read row has no number to show.
    pub fn unread_count(&self, entry: &ChannelEntry) -> usize {
        match self.marker(entry) {
            Marker::Unread | Marker::Mention => entry.read.unread.len(),
            _ => 0,
        }
    }

    /// What an empty list means. An empty view is four different answers, and
    /// only one of them says there is nothing to read.
    pub fn empty_view(&self) -> &'static str {
        if !self.marker_read {
            return "Checking...";
        }
        if self.filter != Filter::All && self.unknown_may_qualify() {
            return "Checking...";
        }
        if self
            .channels
            .iter()
            .any(|entry| entry.read.coverage == Coverage::Failed)
        {
            return "Checking failed";
        }
        if self.channels.is_empty() {
            // Nothing to read is not the same answer as everything read.
            return "No conversations";
        }
        match self.filter {
            Filter::All => "No conversations",
            Filter::Unread => "All read",
            Filter::ForYou => "No unread mentions or DMs",
        }
    }

    /// Whether a conversation whose state is unknown could still belong in the
    /// active view. A filtered view may not claim to be complete while one
    /// could.
    fn unknown_may_qualify(&self) -> bool {
        self.filter != Filter::All
            && self
                .channels
                .iter()
                .any(|entry| !entry.read.coverage.known())
    }

    /// The line the Inbox carries under its list, when it has one: a read the
    /// relay has not been told about, a local baseline that says where tracking
    /// starts, or conversations it could not list at all.
    pub fn inbox_footer(&self) -> Option<String> {
        if let Some(reason) = &self.read_failed {
            return Some(format!("Read here; not synced ({reason})"));
        }
        if let Some(entry) = self.focused_entry()
            && entry.read.known
            && !entry.read.marked
        {
            return Some("Tracking new messages from this visit".to_owned());
        }
        if !self.roster_complete {
            return Some("Some conversations could not be listed".to_owned());
        }
        None
    }

    fn load_history(&mut self, channel: Uuid, events: Vec<nostr::Event>, now: u64) {
        let history_retry = self.history_failed.remove(&channel);
        if let Some(ids) = self.aux_seen.remove(&channel) {
            for id in ids {
                self.seen_aux.remove(&id);
            }
        }
        let me = self.me.clone();
        let profiles = self.profiles.clone();
        let Some(entry) = self.entry_mut(&channel) else {
            return;
        };
        let pending: Vec<Row> = entry.rows.iter().filter(|r| r.pending).cloned().collect();
        let fetched_ids: HashSet<String> = events.iter().map(|event| event.id.to_hex()).collect();
        let live: Vec<Row> = entry
            .rows
            .iter()
            .filter(|row| {
                !row.pending
                    && entry.live_ids.contains(&row.event_id)
                    && !fetched_ids.contains(&row.event_id)
            })
            .cloned()
            .collect();
        let mut rows: Vec<Row> = events
            .iter()
            .map(|e| content::row_from_event(e, &me))
            .collect();
        // The relay returns events in a stable order; the client keeps it,
        // sorted only by timestamp for the oldest-first view.
        rows.extend(live);
        for row in &mut rows {
            if !row.pending {
                row.author = author_name(&profiles, &me, &row.pubkey);
            }
        }
        rows.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then(a.event_id.cmp(&b.event_id))
        });
        rows.extend(pending);
        entry.seen = rows
            .iter()
            .filter(|r| !r.pending)
            .map(|r| r.event_id.clone())
            .collect();
        entry
            .live_ids
            .retain(|id| !fetched_ids.contains(id) && entry.seen.contains(id));
        let fetched: Vec<(String, u64)> = rows
            .iter()
            .filter(|r| !r.pending)
            .map(|r| (r.pubkey.clone(), r.created_at))
            .collect();
        entry.rows = rows;
        entry.loading = false;
        if history_retry {
            // The timeline is repaired, but an earlier failed load may have
            // consumed the outstanding Inbox catch-up. Request it again only
            // after the marker lookup has completed.
            let coverage = if !self.marker_read || !self.marker_complete {
                Coverage::UnknownMarker
            } else {
                Coverage::Pending
            };
            if let Some(entry) = self.entry_mut(&channel) {
                entry.read.coverage = coverage;
            }
            if !self.catch_up_pending.contains(&channel) {
                self.catch_up_pending.push(channel);
            }
            self.request_catch_up();
        }
        // History can land after the indicator it belongs to - the channel is
        // opened over HTTP while the live feed is already running - so the
        // fetched rows end indicators on the same rule as live ones.
        for (pubkey, at) in fetched {
            self.end_typing(channel, &pubkey, at);
        }
        if self.selected_entry().map(|e| e.id) == Some(channel) {
            self.set_focus(usize::MAX);
        }
        self.refresh_views();
        self.request_profiles();
        let _ = now;
    }
    fn history_failed(&mut self, channel: Uuid, reason: String) {
        self.history_failed.insert(channel);
        if let Some(entry) = self.entry_mut(&channel) {
            entry.loading = false;
            entry.read.coverage = Coverage::Failed;
        }
        if self.selected_entry().map(|entry| entry.id) == Some(channel) {
            self.status = format!("history failed: {reason}");
        }
        self.refresh_views();
    }

    fn apply_timeline(&mut self, channel: Uuid, event: nostr::Event, _now: u64) {
        let id = event.id.to_hex();
        let me = self.me.clone();
        let profiles = self.profiles.clone();
        let author_key = event.pubkey.to_hex();
        let author = author_name(&profiles, &me, &author_key);
        let known_author = profiles.contains_key(&author_key) || author_key == me;
        let selected = self.selected_entry().map(|e| e.id) == Some(channel);
        if !selected {
            // A previously opened conversation may still have a timeline feed
            // while its Inbox feed is also active. Treat that stale feed as
            // Inbox input so it cannot mark the event seen before unread logic
            // receives it.
            self.apply_inbox_timeline(channel, event);
            return;
        }
        let was_at_bottom = self.at_bottom();
        let Some(entry) = self.entry_mut(&channel) else {
            return;
        };
        if entry.seen.contains(&id) || entry.rows.iter().any(|row| row.event_id == id) {
            return;
        }
        entry.live_ids.insert(id.clone());
        entry.seen.insert(id.clone());
        let mut row = content::row_from_event(&event, &me);
        row.author = author;
        entry.rows.push(row);
        if was_at_bottom {
            self.focus = entry.rows.len() - 1;
        }
        self.outbox.push(SessionCommand::AddAux {
            channel,
            ids: vec![id],
        });
        // The message itself is the end of that author's indicator: typing is
        // a pre-message signal, so it never outlives the message it announced.
        // A replay of an older message leaves a live claim standing.
        self.end_typing(channel, &author_key, event.created_at.as_secs());
        if !known_author {
            self.outbox
                .push(SessionCommand::LoadProfiles(vec![author_key]));
        }
        if was_at_bottom {
            // The message was presented where the reader already sat.
            self.note_presented();
        }
    }
    fn channel_for_row(&self, target: &str) -> Option<Uuid> {
        self.channels
            .iter()
            .find(|entry| entry.rows.iter().any(|row| row.event_id == target))
            .map(|entry| entry.id)
    }

    fn remember_aux(&mut self, event_id: &str, target: &str) -> Option<Uuid> {
        let channel = self.channel_for_row(target)?;
        self.aux_seen
            .entry(channel)
            .or_default()
            .insert(event_id.to_owned());
        Some(channel)
    }
    fn remember_aux_channel(&mut self, event_id: &str, channel: Uuid) {
        self.seen_aux.insert(event_id.to_owned());
        self.aux_seen
            .entry(channel)
            .or_default()
            .insert(event_id.to_owned());
    }

    fn apply_overlay(&mut self, event: nostr::Event) {
        let id = event.id.to_hex();
        if self.seen_aux.contains(&id) {
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
            if self.remember_aux(&id, &row_id).is_none() {
                return;
            }
            self.seen_aux.insert(id.clone());
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
        if self.remember_aux(&id, &target).is_none() {
            return;
        }
        self.seen_aux.insert(id.clone());
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
                // An edit that carries `p` tags can change whether the message
                // mentions this identity; one that carries none says nothing
                // about mentions and leaves the message's own tags standing.
                let me = self.me.clone();
                let mention = edit_mention(&event, &me);
                self.apply_to_row(&target, |row| {
                    if let Some(mention) = mention {
                        row.mentions_me = mention;
                    }
                    content::apply_overlay(row, &content::Overlay::Edit { body: body.clone() })
                });
                if let Some(candidate) = self
                    .channels
                    .iter_mut()
                    .flat_map(|entry| entry.read.unread.get_mut(&target))
                    .next()
                    && let Some(mention) = mention
                {
                    candidate.mention = mention;
                }
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
                self.channels[index].live_ids.remove(target);
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
        // Every action works on the list as it is now: the views are derived
        // state, and a roster that changed since the last event must not leave
        // a key moving through a list that no longer exists.
        self.refresh_views();
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
                    // The help draws over the picker, so it replaces it, and
                    // it opens at its top: the full label of the selected
                    // conversation is part of it.
                    self.picker = None;
                    self.agents.open = false;
                    self.help_scroll = 0;
                }
            }
            Action::HelpScroll(step) => {
                self.help_scroll = self.help_scroll.saturating_add_signed(step as i16);
            }
            Action::Dismiss => {
                if self.help {
                    self.help = false;
                    self.note_presented();
                } else if self.picker.take().is_some() {
                    self.note_presented();
                } else {
                    self.dismiss_agents();
                }
            }
            Action::NextChannel => self.step_channel(1),
            Action::PrevChannel => self.step_channel(-1),
            Action::NextRow => self.set_focus(self.focus.saturating_add(1)),
            Action::PrevRow => self.set_focus(self.focus.saturating_sub(1)),
            Action::Channel(n) => {
                let target = self.shortcut_target(n).and_then(|id| self.index_of(id));
                self.picker = None;
                if let Some(index) = target {
                    self.switch_channel(index);
                }
            }
            Action::FilterNext => self.set_filter(self.filter.next()),
            Action::ToggleAgents => self.toggle_agents(),
            Action::AgentsNext => self.move_agents(1),
            Action::AgentsPrev => self.move_agents(-1),
            Action::AgentsConfirm => self.confirm_agents(),
            Action::TogglePicker => self.toggle_picker(),
            Action::PickerNext => self.move_picker(1),
            Action::PickerConfirm => {
                let confirmed = self
                    .picker
                    .take()
                    .and_then(|picker| self.index_of(picker.cursor));
                if let Some(index) = confirmed {
                    self.switch_channel(index);
                }
                self.note_presented();
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
        self.save_draft();
        let label = self.label(&self.channels[index]);
        let id = self.channels[index].id;
        self.selected = index;
        self.channels[index].loading = true;
        self.adopt_draft();
        self.set_focus(usize::MAX);
        self.status = format!("opened {label}");
        self.outbox.push(SessionCommand::OpenChannel(id));
    }

    /// `j`/`k`: walk the conversations the active filter shows, from wherever
    /// the selection stands. The selection is the conversation on screen, so a
    /// step opens the next one.
    fn step_channel(&mut self, step: isize) {
        let order: Vec<Uuid> = self.view().all().collect();
        if order.is_empty() {
            return;
        }
        let current = self.channels.get(self.selected).map(|entry| entry.id);
        let at = current.and_then(|id| order.iter().position(|candidate| *candidate == id));
        let next = match at {
            Some(at) => (at as isize + step).clamp(0, order.len() as isize - 1) as usize,
            // The selection is outside this view: the first step enters it.
            None if step > 0 => 0,
            None => order.len() - 1,
        };
        if let Some(index) = self.index_of(order[next]) {
            self.switch_channel(index);
        }
    }

    /// `f`/Tab: the next filter. The picker's cursor moves to the first
    /// conversation the new filter shows when the one it was on is not in it,
    /// so a filtered view never opens on something it does not show.
    fn set_filter(&mut self, filter: Filter) {
        self.filter = filter;
        self.refresh_views();
        if let Some(picker) = self.picker.clone() {
            let view = self.view().clone();
            if !view.contains(picker.cursor)
                && let Some(first) = view.all().next()
            {
                self.picker = Some(Picker {
                    cursor: first,
                    at: 0,
                });
            }
        }
    }

    /// `c`: the picker opens on the conversation on screen, and closes when it
    fn toggle_picker(&mut self) {
        if self.picker.take().is_some() {
            self.note_presented();
            return;
        }
        self.picker = if !self.channels.is_empty() {
            // Only one overlay is on screen at a time.
            self.help = false;
            self.agents.open = false;
            self.channels.get(self.selected).map(|entry| {
                let at = self.view().all().position(|id| id == entry.id).unwrap_or(0);
                Picker {
                    cursor: entry.id,
                    at,
                }
            })
        } else {
            None
        };
    }

    /// `a`: open the Agents overlay, or close it when it is open. Opening it
    /// asks for the roster: that read is one query, and a session that never
    /// opens the overlay never spends it.
    fn toggle_agents(&mut self) {
        if self.agents.open {
            self.agents.open = false;
            return;
        }
        // Only one overlay is on screen at a time.
        self.help = false;
        self.picker = None;
        self.agents.open = true;
        self.agents.cursor.level = agents::Level::List;
        self.outbox.push(SessionCommand::LoadAgents);
    }

    /// `j` and `k` inside the Agents overlay: the Agent list on the list
    /// level, the working contexts on the detail level.
    fn move_agents(&mut self, step: isize) {
        if !self.agents.open {
            return;
        }
        let len = match self.agents.cursor.level {
            agents::Level::List => self.agent_count(),
            agents::Level::Detail => self
                .selected_agent()
                .map(|agent| self.agent_contexts(&agent.pubkey).len())
                .unwrap_or(0),
        };
        self.agents.cursor.move_by(step, len);
    }

    /// Enter inside the Agents overlay: the list opens the selected Agent's
    /// detail, and the detail opens the selected working conversation. An
    /// Agent that cannot be read, or a context this identity is not in, opens
    /// nothing.
    fn confirm_agents(&mut self) {
        if !self.agents.open {
            return;
        }
        match self.agents.cursor.level {
            agents::Level::List => {
                if self.selected_agent().is_none() {
                    return;
                }
                self.agents.cursor.level = agents::Level::Detail;
                self.agents.cursor.context = 0;
            }
            agents::Level::Detail => {
                let Some(Context::Channel { id, .. }) = self.selected_context() else {
                    return;
                };
                self.agents.open = false;
                if let Some(index) = self.index_of(id) {
                    self.switch_channel(index);
                }
            }
        }
    }

    /// Esc inside the Agents overlay: the detail back to the list, and the
    /// list back to the timeline.
    fn dismiss_agents(&mut self) {
        if !self.agents.open {
            return;
        }
        if !self.agents.cursor.back() {
            self.agents.open = false;
        }
    }

    /// The context the detail level's cursor is on.
    fn selected_context(&self) -> Option<Context> {
        let agent = self.selected_agent()?;
        self.agent_contexts(&agent.pubkey)
            .into_iter()
            .nth(self.agents.cursor.context)
    }

    /// Move the picker's cursor one row through the view it shows.
    fn move_picker(&mut self, step: isize) {
        let Some(picker) = self.picker.clone() else {
            return;
        };
        let order: Vec<Uuid> = self.picker_view().all().collect();
        let Some(at) = order.iter().position(|id| *id == picker.cursor) else {
            return;
        };
        let next = (at as isize + step).clamp(0, order.len() as isize - 1) as usize;
        self.picker = Some(Picker {
            cursor: order[next],
            at: next,
        });
    }

    /// The membership can change under the picker; its cursor never points at
    /// a conversation that is gone. Its next surviving neighbor takes the row,
    /// or the previous one at the end.
    fn clamp_picker(&mut self) {
        let Some(picker) = self.picker.clone() else {
            return;
        };
        if picker.at == 0 && self.channels.iter().any(|entry| entry.id == picker.cursor) {
            return;
        }
        let view: Vec<Uuid> = self.view().all().collect();
        let Some(next) = view.get(picker.at.min(view.len().saturating_sub(1))) else {
            self.picker = None;
            return;
        };
        self.picker = Some(Picker {
            cursor: *next,
            at: picker.at,
        });
    }

    /// Keep the composer's text with the conversation it belongs to. A draft
    /// must never follow the user into another conversation.
    fn save_draft(&mut self) {
        if let Some(entry) = self.channels.get_mut(self.selected) {
            entry.draft = self.composer.clone();
        }
    }

    /// Put the selected conversation's own draft - and its reply or edit
    /// target - back in the composer.
    fn adopt_draft(&mut self) {
        if let Some(entry) = self.channels.get(self.selected) {
            self.composer = entry.draft.clone();
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
                if let Some(channel) = self.channel_for_row(&row_id) {
                    self.remember_aux_channel(&event_id, channel);
                } else {
                    self.seen_aux_insert(&event_id);
                }
                if let Some(row) = self.row_mut(&row_id) {
                    row.uncertain = false;
                }
                self.status = "edited".to_owned();
            }
            Some(PendingOp::Delete { channel, .. }) => {
                self.remember_aux_channel(&event_id, channel);
                self.status = "deleted".to_owned();
            }
            Some(PendingOp::React { row_id, emoji }) => {
                if let Some(channel) = self.channel_for_row(&row_id) {
                    self.remember_aux_channel(&event_id, channel);
                } else {
                    self.seen_aux_insert(&event_id);
                }
                self.reaction_of
                    .insert(event_id.clone(), (row_id.clone(), emoji.clone()));
                self.my_reaction.insert((row_id, emoji), event_id);
                self.status = "reacted".to_owned();
            }
            Some(PendingOp::ReactRemove {
                row_id,
                reaction_id,
                ..
            }) => {
                if let Some(channel) = self.channel_for_row(&row_id) {
                    self.remember_aux_channel(&event_id, channel);
                } else {
                    self.seen_aux_insert(&event_id);
                }
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
/// The mention flag an edit states, when it states one. An edit that carries
/// no `p` tag says nothing about mentions: the message's own tags stand.
fn edit_mention(event: &nostr::Event, me: &str) -> Option<bool> {
    let tagged = event
        .tags
        .iter()
        .any(|tag| tag.as_slice().first().map(String::as_str) == Some("p"));
    tagged.then(|| content::mentions_me(event, me))
}

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
            kind: ChannelKind::Channel,
            participants: Vec::new(),
            archived: false,
            hidden: false,
        }
    }

    fn roster(items: Vec<ChannelInfo>) -> Roster {
        Roster {
            items,
            complete: true,
        }
    }

    fn channel(id: u32) -> ChannelEntry {
        ChannelEntry {
            id: Uuid::from_u64_pair(id as u64, 0),
            name: format!("c{id}"),
            kind: ChannelKind::Channel,
            participants: Vec::new(),
            rows: Vec::new(),
            seen: HashSet::new(),
            live_ids: HashSet::new(),
            loading: false,
            draft: Composer::default(),
            read: ReadTrack {
                known: true,
                ..ReadTrack::default()
            },
        }
    }

    /// The conversation under the picker's cursor.
    fn picked(app: &App) -> Option<Uuid> {
        app.picker.as_ref().map(|picker| picker.cursor)
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

    /// The commands a test is about. Showing rows records read progress, so
    /// tests that are not about read state drop that one side effect.
    fn take_commands(app: &mut App) -> Vec<SessionCommand> {
        app.take_outbox()
            .into_iter()
            .filter(|command| !matches!(command, SessionCommand::ReadProgress { .. }))
            .collect()
    }

    #[test]
    fn a_connected_session_loads_channels_and_opens_the_first() {
        let mut app = app();
        let id = channel(1).id;
        app.apply(ChatEvent::Connected, 0);
        assert!(matches!(
            take_commands(&mut app)[..],
            [SessionCommand::LoadChannels]
        ));
        app.apply(
            ChatEvent::Channels(roster(vec![ChannelInfo {
                id,
                name: "general".into(),
                ..channel_info(1)
            }])),
            0,
        );
        match &take_commands(&mut app)[..] {
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
    fn a_stale_timeline_feed_cannot_hide_inbox_unread() {
        let mut app = app();
        app.channels = vec![channel(1), channel(2)];
        app.stub_roster();
        let id = app.channels[1].id;
        let event = message_event(&keys(), id, "arrived elsewhere", 10);

        app.apply(
            ChatEvent::Timeline {
                channel: id,
                event: event.clone(),
            },
            10,
        );
        assert!(app.channels[1].rows.is_empty());
        assert_eq!(app.channels[1].read.unread.len(), 1);

        app.apply(ChatEvent::InboxTimeline { channel: id, event }, 10);
        assert_eq!(
            app.channels[1].read.unread.len(),
            1,
            "the matching Inbox event must be a harmless duplicate"
        );
    }

    #[test]
    fn a_live_timeline_row_extends_the_aux_feed() {
        let mut app = app();
        app.channels = vec![channel(1)];
        let id = app.channels[0].id;
        let event = message_event(&keys(), id, "needs overlays", 10);
        let event_id = event.id.to_hex();

        app.apply(ChatEvent::Timeline { channel: id, event }, 10);

        assert!(take_commands(&mut app).into_iter().any(|command| {
            matches!(
                command,
                SessionCommand::AddAux { channel, ids }
                    if channel == id && ids == vec![event_id.clone()]
            )
        }));
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
        app.apply(ChatEvent::Channels(roster(vec![channel_info(1)])), 101);
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
        let commands = take_commands(&mut app);
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
        let commands = take_commands(&mut app);
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
        let commands = take_commands(&mut app);
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
            take_commands(&mut app).is_empty(),
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
        match &take_commands(&mut app)[..] {
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

        let commands = take_commands(&mut app);
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
        let commands = take_commands(&mut app);
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
        let commands = take_commands(&mut app);
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
        match &take_commands(&mut app)[..] {
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
    fn reloading_history_allows_aux_overlay_to_be_applied_again() {
        let mut app = app();
        let id = channel(1).id;
        app.channels = vec![channel(1)];
        let original = message_event(&keys(), id, "original", 1);
        let row_id = original.id.to_hex();
        app.apply(
            ChatEvent::History {
                channel: id,
                events: vec![original.clone()],
            },
            0,
        );

        let edit = edit_event(&row_id, "rewritten");
        app.apply(ChatEvent::Overlay(edit.clone()), 0);
        assert_eq!(app.channels[0].rows[0].body, "rewritten");

        app.apply(
            ChatEvent::History {
                channel: id,
                events: vec![original],
            },
            0,
        );
        assert_eq!(app.channels[0].rows[0].body, "original");
        app.apply(ChatEvent::Overlay(edit), 0);
        assert_eq!(
            app.channels[0].rows[0].body, "rewritten",
            "the aux query after a reload must reapply the edit"
        );
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
        match &take_commands(&mut app)[..] {
            [SessionCommand::OpenChannel(id)] => assert_eq!(*id, second_id),
            other => panic!("expected one open, got {other:?}"),
        }
    }

    #[test]
    fn digits_jump_to_channels_and_clamp_to_the_last() {
        let mut app = app();
        app.channels = vec![channel(1), channel(2), channel(3)];
        app.stub_roster();
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
        assert_eq!(
            picked(&app),
            Some(ids[1]),
            "the picker opens on the selection"
        );
        app.handle(Action::PickerNext, 0);
        app.handle(Action::PickerNext, 0);
        app.handle(Action::PickerNext, 0);
        assert_eq!(
            picked(&app),
            Some(ids[2]),
            "the cursor clamps at the last row"
        );
        app.handle(Action::PickerConfirm, 0);
        assert_eq!(app.picker, None, "confirming closes the picker");
        assert_eq!(app.channels[app.selected].id, ids[2]);
        assert!(
            matches!(
                take_commands(&mut app)[..],
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
        let only = channel(1).id;
        app.handle(Action::TogglePicker, 0);
        assert_eq!(picked(&app), Some(only));
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
        app.picker = Some(Picker {
            cursor: channel(3).id,
            at: 2,
        });
        app.apply(
            ChatEvent::ChannelGone {
                channel: channel(3).id,
                reason: "restricted".into(),
            },
            0,
        );
        assert_eq!(picked(&app), Some(channel(2).id));
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
            take_commands(&mut app)[..],
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
        match &take_commands(&mut app)[..] {
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
        match &take_commands(&mut app)[0] {
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

    fn mention_event(keys: &Keys, channel_id: Uuid, body: &str, at: u64, me: &str) -> nostr::Event {
        EventBuilder::new(Kind::Custom(9), body)
            .tags(vec![
                nostr::Tag::parse(["h", &channel_id.to_string()]).unwrap(),
                nostr::Tag::parse(["p", me]).unwrap(),
            ])
            .custom_created_at(Timestamp::from(at))
            .sign_with_keys(keys)
            .unwrap()
    }

    fn edit_mention_event(target: &str, body: &str, me: &str) -> nostr::Event {
        let keys = keys();
        EventBuilder::new(Kind::Custom(40003), body)
            .tags(vec![
                nostr::Tag::parse(["e", target]).unwrap(),
                nostr::Tag::parse(["p", me]).unwrap(),
            ])
            .custom_created_at(Timestamp::from(30))
            .sign_with_keys(&keys)
            .unwrap()
    }

    fn dm_info(id: u32, participants: Vec<String>) -> ChannelInfo {
        ChannelInfo {
            kind: ChannelKind::Dm,
            name: "DM".to_owned(),
            participants,
            ..channel_info(id)
        }
    }

    /// A conversation whose read state is known and empty, so a test can state
    /// one unread message without arguing with a seed.
    fn caught_up(app: &mut App) {
        app.apply(
            ChatEvent::ReadState {
                contexts: HashMap::new(),
                complete: true,
            },
            0,
        );
    }

    fn published(app: &mut App) -> Option<HashMap<String, u64>> {
        app.take_outbox()
            .into_iter()
            .find_map(|command| match command {
                SessionCommand::ReadProgress { contexts } => Some(contexts),
                _ => None,
            })
    }

    #[test]
    fn a_conversation_on_screen_claims_its_read_state_when_the_answer_lands() {
        let mut app = app();
        let keys = keys();
        app.channels = vec![channel(1)];
        app.stub_roster();
        let id = app.channels[0].id;
        let event = message_event(&keys, id, "hello", 10);
        app.channels[0].rows = vec![Row {
            event_id: event.id.to_hex(),
            ..row("a", 10, "p")
        }];
        app.channels[0].loading = false;
        app.focus = 0;
        // The cold start: the newest message is on screen before the relay has
        // said what this identity already read.
        app.note_presented();
        assert_eq!(
            published(&mut app),
            None,
            "nothing is claimed before the relay's answer"
        );

        app.apply(
            ChatEvent::ReadState {
                contexts: HashMap::new(),
                complete: true,
            },
            0,
        );
        app.apply(
            ChatEvent::CatchUp {
                channel: id,
                events: Vec::new(),
                complete: true,
            },
            0,
        );
        let claim = published(&mut app).expect("the answer closes the gate on the conversation");
        assert_eq!(
            claim.get(&id.to_string()),
            Some(&10),
            "the frontier is the newest message that was on screen"
        );
    }

    #[test]
    fn a_seed_claim_reaches_only_as_far_as_the_rows_that_were_shown() {
        let mut app = app();
        let keys = keys();
        app.channels = vec![channel(1)];
        app.stub_roster();
        let id = app.channels[0].id;
        caught_up(&mut app);
        // The feed names the conversation's newest message, which is all a
        // conversation without a marker has: a baseline at second 100.
        app.apply(
            ChatEvent::Seed {
                channel: id,
                latest: Some(message_event(&keys, id, "newest", 100)),
                complete: true,
            },
            0,
        );
        // The history this terminal loaded reaches second 91, and that row is
        // the one on screen.
        app.channels[0].rows = vec![row("a", 90, "p"), row("b", 91, "p")];
        app.channels[0].loading = false;
        app.focus = 1;
        app.note_presented();
        let claim = published(&mut app).expect("the presented rows are a read claim");
        assert_eq!(
            claim.get(&id.to_string()),
            Some(&91),
            "a baseline reaches no further than the newest row the reader saw"
        );
    }

    #[test]
    fn a_kind_the_inbox_feed_did_not_ask_for_is_not_an_unread_message() {
        let mut app = app();
        let keys = keys();
        app.channels = vec![channel(1), channel(2)];
        app.stub_roster();
        let (open, other) = (app.channels[0].id, app.channels[1].id);
        caught_up(&mut app);
        app.focus = 0;
        let reaction = EventBuilder::new(Kind::Custom(content::AUX_KINDS[0] as u16), "+")
            .tags(vec![nostr::Tag::parse(["h", &other.to_string()]).unwrap()])
            .sign_with_keys(&keys)
            .unwrap();
        app.apply(
            ChatEvent::InboxTimeline {
                channel: other,
                event: reaction,
            },
            0,
        );
        assert!(
            app.channels[1].read.unread.is_empty(),
            "a reaction on the feed is not a message"
        );
        app.apply(
            ChatEvent::InboxTimeline {
                channel: other,
                event: message_event(&keys, other, "hello", 30),
            },
            0,
        );
        assert_eq!(app.channels[1].read.unread.len(), 1);
        assert_ne!(open, other);
    }

    #[test]
    fn an_unread_message_and_a_mention_raise_their_rows_and_the_filters_agree() {
        let mut app = app();
        app.channels = vec![channel(1), channel(2)];
        app.stub_roster();
        let (one, two) = (app.channels[0].id, app.channels[1].id);
        let me = app.me.clone();
        caught_up(&mut app);
        app.apply(
            ChatEvent::CatchUp {
                channel: one,
                events: vec![message_event(&keys(), one, "hello", 20)],
                complete: true,
            },
            0,
        );
        app.apply(
            ChatEvent::CatchUp {
                channel: two,
                events: vec![mention_event(&keys(), two, "for you", 21, &me)],
                complete: true,
            },
            0,
        );
        assert_eq!(app.marker(&app.channels[0]), Marker::Unread);
        assert_eq!(app.marker(&app.channels[1]), Marker::Mention);

        app.filter = Filter::Unread;
        app.refresh_views();
        assert_eq!(app.view().channels, vec![one, two], "both hold unread");
        app.filter = Filter::ForYou;
        app.refresh_views();
        assert_eq!(
            app.view().channels,
            vec![two],
            "an ordinary channel message is not personal attention"
        );
    }

    #[test]
    fn for_you_holds_a_dm_only_while_it_has_something_to_read() {
        let mut app = app();
        let mut conversation = channel(2);
        conversation.kind = crate::client::ChannelKind::Dm;
        app.channels = vec![channel(1), conversation];
        app.stub_roster();
        let dm_id = app.channels[1].id;
        caught_up(&mut app);
        let ping = message_event(&keys(), dm_id, "ping", 20);
        app.apply(
            ChatEvent::CatchUp {
                channel: dm_id,
                events: vec![ping.clone()],
                complete: true,
            },
            0,
        );
        app.filter = Filter::ForYou;
        app.refresh_views();
        assert_eq!(app.view().dms, vec![dm_id], "an unread DM is for you");

        // Reading it leaves the view: what was read is not what needs reading.
        app.selected = 1;
        app.apply(
            ChatEvent::History {
                channel: dm_id,
                events: vec![ping],
            },
            30,
        );
        // Read here, and the row no longer matches this view: the list says
        // why it was dropped rather than calling it unread.
        assert_eq!(app.marker(&app.channels[1]), Marker::Read, "read now");
        app.refresh_views();
        assert!(
            app.view().dms.is_empty(),
            "a read DM is a conversation, not a task"
        );
    }

    #[test]
    fn a_failed_marker_lookup_is_unknown_and_never_read() {
        let mut app = app();
        app.channels = vec![channel(1)];
        app.stub_roster();
        app.apply(
            ChatEvent::ReadState {
                contexts: HashMap::new(),
                complete: false,
            },
            0,
        );
        assert_eq!(app.marker(&app.channels[0]), Marker::Unknown);
        assert_eq!(app.empty_view(), "No conversations");
        app.filter = Filter::Unread;
        assert_eq!(
            app.empty_view(),
            "Checking...",
            "a filtered view may not claim there is nothing unread"
        );
        assert!(
            take_commands(&mut app).is_empty(),
            "a failed lookup asks for no seed: absence is unknown, not empty"
        );
    }

    #[test]
    fn a_seed_tracks_new_messages_without_claiming_to_have_read() {
        let mut app = app();
        app.channels = vec![channel(1), channel(2)];
        app.stub_roster();
        // Channel 1 is on screen; channel 2 is the one the Inbox feed watches.
        let id = app.channels[1].id;
        let keys = keys();
        caught_up(&mut app);
        let newest = message_event(&keys, id, "yesterday", 20);
        app.apply(
            ChatEvent::Seed {
                channel: id,
                latest: Some(newest),
                complete: true,
            },
            0,
        );
        assert_eq!(
            app.marker(&app.channels[1]),
            Marker::None,
            "a baseline is not unread work"
        );
        assert!(
            published(&mut app).is_none(),
            "a local seed is never uploaded as proof of reading"
        );

        app.apply(
            ChatEvent::InboxTimeline {
                channel: id,
                event: message_event(&keys, id, "today", 30),
            },
            130,
        );
        assert_eq!(app.marker(&app.channels[1]), Marker::Unread);
        // The row says where its tracking starts, and the note is about the
        // conversation under the cursor.
        app.picker = Some(Picker { cursor: id, at: 1 });
        assert_eq!(
            app.inbox_footer().as_deref(),
            Some("Tracking new messages from this visit")
        );
    }

    #[test]
    fn opening_a_conversation_publishes_its_frontier_and_a_failed_publish_says_so() {
        let mut app = app();
        // A real roster, so the footer's only possible note is about the read.
        app.apply(ChatEvent::Channels(roster(vec![channel_info(1)])), 0);
        app.apply(
            ChatEvent::ReadState {
                contexts: HashMap::new(),
                complete: true,
            },
            0,
        );
        let id = app.channels[0].id;
        let event = message_event(&keys(), id, "hello", 20);
        app.apply(
            ChatEvent::CatchUp {
                channel: id,
                events: vec![event.clone()],
                complete: true,
            },
            0,
        );
        app.apply(
            ChatEvent::History {
                channel: id,
                events: vec![event],
            },
            30,
        );
        let contexts =
            published(&mut app).expect("showing the newest message publishes the frontier");
        assert_eq!(contexts.get(&id.to_string()), Some(&20));
        assert_eq!(
            app.marker(&app.channels[0]),
            Marker::None,
            "read here already"
        );

        app.apply(
            ChatEvent::ReadPublished {
                ok: false,
                reason: "relay refused".into(),
            },
            31,
        );
        assert_eq!(
            app.inbox_footer().as_deref(),
            Some("Read here; not synced (relay refused)")
        );
        // Another read advances the frontier again, so a fresh publish is on
        // its way. The relay still has not been told, and the note says so
        // until one of those publishes succeeds.
        app.apply(
            ChatEvent::History {
                channel: id,
                events: vec![message_event(&keys(), id, "and another", 40)],
            },
            41,
        );
        assert_eq!(
            app.inbox_footer().as_deref(),
            Some("Read here; not synced (relay refused)"),
            "a publish in flight does not clear a failure the relay still has"
        );
        app.apply(
            ChatEvent::ReadPublished {
                ok: true,
                reason: String::new(),
            },
            32,
        );
        assert_eq!(
            app.inbox_footer(),
            None,
            "a synced terminal has nothing to explain"
        );
    }

    #[test]
    fn previewing_the_picker_or_reading_older_history_does_not_clear_unread() {
        let mut app = app();
        app.channels = vec![channel(1)];
        app.stub_roster();
        let id = app.channels[0].id;
        let keys = keys();
        caught_up(&mut app);
        let older = message_event(&keys, id, "a", 10);
        let newer = message_event(&keys, id, "b", 20);
        let newest = newer.id.to_hex();
        app.apply(
            ChatEvent::CatchUp {
                channel: id,
                events: vec![older.clone(), newer],
                complete: true,
            },
            0,
        );
        app.channels[0].rows = vec![
            Row {
                event_id: older.id.to_hex(),
                ..row("a", 10, "p")
            },
            Row {
                event_id: newest,
                ..row("b", 20, "p")
            },
        ];

        app.channels[0].loading = true;
        app.focus = 1;
        app.note_presented();
        assert_eq!(
            app.marker(&app.channels[0]),
            Marker::Unread,
            "a load still in flight is not a read"
        );

        app.channels[0].loading = false;
        app.focus = 0;
        app.note_presented();
        assert_eq!(
            app.marker(&app.channels[0]),
            Marker::Unread,
            "older history is not the latest position"
        );

        app.focus = 1;
        app.picker = Some(Picker { cursor: id, at: 0 });
        app.note_presented();
        assert_eq!(
            app.marker(&app.channels[0]),
            Marker::Unread,
            "a picker preview is not a read"
        );

        app.picker = None;
        app.note_presented();
        assert_eq!(app.marker(&app.channels[0]), Marker::None);
        let contexts = published(&mut app).expect("the read is published");
        assert_eq!(contexts.get(&id.to_string()), Some(&20));
    }

    #[test]
    fn a_truncated_catch_up_leaves_the_conversation_unknown_rather_than_read() {
        let mut app = app();
        app.channels = vec![channel(1)];
        app.stub_roster();
        let id = app.channels[0].id;
        caught_up(&mut app);
        app.apply(
            ChatEvent::CatchUp {
                channel: id,
                events: vec![message_event(&keys(), id, "maybe more", 20)],
                complete: false,
            },
            0,
        );
        assert_eq!(app.marker(&app.channels[0]), Marker::Unknown);
        app.filter = Filter::Unread;
        assert_eq!(app.empty_view(), "Checking...");
        app.filter = Filter::All;
        assert_eq!(app.empty_view(), "Checking failed");
    }

    #[test]
    fn a_message_delivered_late_inside_the_frontiers_second_stays_unread() {
        let mut app = app();
        app.channels = vec![channel(1), channel(2)];
        app.stub_roster();
        let two = app.channels[1].id;
        let keys = keys();
        // Another terminal read up to second 20, and this one is handed an
        // event from that same second only now.
        app.apply(
            ChatEvent::ReadState {
                contexts: HashMap::from([(two.to_string(), 20)]),
                complete: true,
            },
            0,
        );
        let event = message_event(&keys, two, "same second", 20);
        let id = event.id.to_hex();
        app.apply(
            ChatEvent::CatchUp {
                channel: two,
                events: vec![event],
                complete: true,
            },
            0,
        );
        assert_eq!(
            app.marker(&app.channels[1]),
            Marker::Unread,
            "a marker cannot vouch for the events inside its own second"
        );

        app.selected = 1;
        app.channels[1].rows = vec![Row {
            event_id: id,
            ..row("x", 20, "p")
        }];
        app.focus = 0;
        app.note_presented();
        assert_eq!(app.marker(&app.channels[1]), Marker::None);
        let contexts = published(&mut app).expect("presenting it publishes the read");
        assert_eq!(
            contexts.get(&two.to_string()),
            Some(&20),
            "the frontier never moves backward"
        );
    }

    #[test]
    fn a_duplicate_or_own_message_adds_no_unread_and_typing_never_does() {
        let own = Keys::generate();
        let mut app = App::new(&own, "http://relay.test");
        app.channels = vec![channel(1), channel(2)];
        app.stub_roster();
        let (one, two) = (app.channels[0].id, app.channels[1].id);
        caught_up(&mut app);
        let event = message_event(&keys(), one, "hello", 20);
        app.apply(
            ChatEvent::InboxTimeline {
                channel: one,
                event: event.clone(),
            },
            20,
        );
        app.apply(
            ChatEvent::InboxTimeline {
                channel: one,
                event: event.clone(),
            },
            21,
        );
        app.apply(
            ChatEvent::CatchUp {
                channel: one,
                events: vec![event],
                complete: true,
            },
            21,
        );
        assert_eq!(
            app.channels[0].read.unread.len(),
            1,
            "one event is one candidate however it arrives"
        );

        app.apply(
            ChatEvent::InboxTimeline {
                channel: two,
                event: message_event(&own, two, "mine", 20),
            },
            20,
        );
        app.apply(
            ChatEvent::Typing {
                channel: two,
                pubkey: "agent-a".into(),
                at: 20,
            },
            20,
        );
        assert!(
            app.channels[1].read.unread.is_empty(),
            "own messages and typing add no unread work"
        );
    }

    #[test]
    fn the_count_is_the_messages_the_conversation_has_not_shown() {
        let mut app = app();
        app.channels = vec![channel(1), channel(2)];
        app.stub_roster();
        let id = app.channels[1].id;
        caught_up(&mut app);
        for body in ["one", "two", "three"] {
            let event = message_event(&keys(), id, body, 20);
            app.apply(ChatEvent::InboxTimeline { channel: id, event }, 20);
        }
        assert_eq!(app.marker(&app.channels[1]), Marker::Unread);
        assert_eq!(app.unread_count(&app.channels[1]), 3);
        // A conversation that has shown everything counts nothing, whatever
        // its marker says.
        assert_eq!(app.unread_count(&app.channels[0]), 0);
    }

    #[test]
    fn an_edit_can_raise_a_candidate_to_a_mention_but_never_creates_one() {
        let mut app = app();
        app.channels = vec![channel(1), channel(2)];
        app.stub_roster();
        let id = app.channels[1].id;
        let me = app.me.clone();
        let event = message_event(&keys(), id, "hi", 20);
        let target = event.id.to_hex();
        caught_up(&mut app);
        app.apply(ChatEvent::InboxTimeline { channel: id, event }, 20);
        assert_eq!(app.marker(&app.channels[1]), Marker::Unread);

        app.apply(
            ChatEvent::InboxTimeline {
                channel: id,
                event: edit_mention_event(&target, "hi", &me),
            },
            21,
        );
        assert_eq!(app.marker(&app.channels[1]), Marker::Mention);
        assert_eq!(
            app.channels[1].read.unread.len(),
            1,
            "the edit raises the candidate, it does not add one"
        );

        app.apply(
            ChatEvent::InboxTimeline {
                channel: id,
                event: edit_mention_event("0000000000000000", "other", &me),
            },
            22,
        );
        assert_eq!(
            app.channels[1].read.unread.len(),
            1,
            "editing a message that is not unread here creates no work"
        );
    }

    #[test]
    fn a_deletion_removes_the_unread_candidate() {
        let mut app = app();
        app.channels = vec![channel(1), channel(2)];
        app.stub_roster();
        let id = app.channels[1].id;
        let event = message_event(&keys(), id, "gone soon", 20);
        let target = event.id.to_hex();
        caught_up(&mut app);
        app.apply(ChatEvent::InboxTimeline { channel: id, event }, 20);
        assert_eq!(app.marker(&app.channels[1]), Marker::Unread);

        app.apply(
            ChatEvent::InboxTimeline {
                channel: id,
                event: delete_event(&target),
            },
            21,
        );
        assert!(app.channels[1].read.unread.is_empty());
        assert_eq!(app.marker(&app.channels[1]), Marker::None);
    }

    #[test]
    fn shortcuts_keep_their_conversations_across_filters_and_removals() {
        let mut app = app();
        app.apply(
            ChatEvent::Channels(roster(vec![
                channel_info(1),
                channel_info(2),
                channel_info(3),
            ])),
            0,
        );
        let (one, two, three) = (app.channels[0].id, app.channels[1].id, app.channels[2].id);
        assert_eq!(app.shortcut(one), Some(1));
        assert_eq!(app.shortcut(three), Some(3));

        caught_up(&mut app);
        app.apply(
            ChatEvent::CatchUp {
                channel: three,
                events: vec![message_event(&keys(), three, "new", 20)],
                complete: true,
            },
            0,
        );
        app.filter = Filter::Unread;
        app.refresh_views();
        assert_eq!(
            app.shortcut(three),
            Some(3),
            "a filter shows a subset, it does not renumber"
        );

        app.apply(
            ChatEvent::ChannelGone {
                channel: two,
                reason: "restricted".into(),
            },
            0,
        );
        assert_eq!(app.shortcut(one), Some(1));
        assert_eq!(app.shortcut(three), Some(3));

        app.apply(
            ChatEvent::Channels(roster(vec![
                channel_info(1),
                channel_info(3),
                channel_info(4),
            ])),
            0,
        );
        assert_eq!(
            app.shortcut(channel(4).id),
            Some(4),
            "a freed number is not handed to a new conversation"
        );
    }

    #[test]
    fn a_dm_asks_for_its_participants_before_any_of_its_history_lands() {
        let mut app = app();
        let other = Keys::generate().public_key().to_hex();
        app.apply(
            ChatEvent::Channels(roster(vec![
                channel_info(1),
                dm_info(2, vec![other.clone()]),
            ])),
            0,
        );
        assert_eq!(app.channels[1].rows.len(), 0, "the DM has no history yet");
        let asked: Vec<String> = take_commands(&mut app)
            .into_iter()
            .filter_map(|command| match command {
                SessionCommand::LoadProfiles(pubkeys) => Some(pubkeys),
                _ => None,
            })
            .flatten()
            .collect();
        assert!(
            asked.contains(&other),
            "an unopened DM is labelled by its participants, so their profiles are wanted \
             without waiting for its history: {asked:?}"
        );
        app.apply(
            ChatEvent::Profiles(vec![(other.clone(), "Direct Person".into())]),
            0,
        );
        assert_eq!(app.label(&app.channels[1]), "Direct Person");
    }

    #[test]
    fn a_dm_is_grouped_labelled_and_personal_without_pretending_to_be_a_channel() {
        let mut app = app();
        let other = Keys::generate().public_key().to_hex();
        app.apply(
            ChatEvent::Channels(roster(vec![
                channel_info(1),
                dm_info(2, vec![other.clone()]),
            ])),
            0,
        );
        app.apply(
            ChatEvent::Profiles(vec![(other.clone(), "Agent A".into())]),
            0,
        );
        assert!(app.channels[1].is_dm());
        assert_eq!(
            app.label(&app.channels[1]),
            "Agent A",
            "a generic DM name becomes the people in it"
        );
        let view = app.view().clone();
        assert_eq!(view.channels.len(), 1, "a DM is not a channel");
        assert_eq!(
            view.dms,
            vec![app.channels[1].id],
            "DMs have their own section"
        );

        let id = app.channels[1].id;
        caught_up(&mut app);
        app.apply(
            ChatEvent::CatchUp {
                channel: id,
                events: vec![message_event(&keys(), id, "ping", 20)],
                complete: true,
            },
            0,
        );
        app.filter = Filter::ForYou;
        app.refresh_views();
        assert_eq!(
            app.view().dms,
            vec![id],
            "any unread message in a DM is personal attention"
        );
        assert!(app.view().channels.is_empty());
        assert_eq!(app.marker(&app.channels[1]), Marker::Unread);
    }

    #[test]
    fn a_filter_change_resets_the_picker_cursor_to_the_first_match() {
        let mut app = app();
        app.channels = vec![channel(1), channel(2)];
        app.stub_roster();
        let (one, two) = (app.channels[0].id, app.channels[1].id);
        caught_up(&mut app);
        app.apply(
            ChatEvent::CatchUp {
                channel: two,
                events: vec![message_event(&keys(), two, "new", 20)],
                complete: true,
            },
            0,
        );
        app.picker = Some(Picker { cursor: one, at: 0 });
        app.handle(Action::FilterNext, 0);
        assert_eq!(app.filter, Filter::Unread);
        assert_eq!(
            picked(&app),
            Some(two),
            "the filter hands the cursor its first match"
        );
    }

    #[test]
    fn a_retained_row_is_only_called_read_when_a_read_claim_exists() {
        let mut app = app();
        app.channels = vec![channel(1)];
        app.stub_roster();
        let id = app.channels[0].id;
        caught_up(&mut app);
        // A conversation that was never unread, kept under the picker's cursor
        // in a filter it does not match.
        app.filter = Filter::Unread;
        app.refresh_views();
        app.picker = Some(Picker { cursor: id, at: 0 });
        assert_eq!(app.picker_view().all().collect::<Vec<Uuid>>(), vec![id]);
        assert_eq!(
            app.marker(&app.channels[0]),
            Marker::None,
            "nothing was read here, so the row claims nothing"
        );

        // A seed is still not a read claim.
        app.apply(
            ChatEvent::Seed {
                channel: id,
                latest: Some(message_event(&keys(), id, "yesterday", 20)),
                complete: true,
            },
            0,
        );
        assert_eq!(app.marker(&app.channels[0]), Marker::None);
    }

    #[test]
    fn a_row_that_becomes_read_keeps_the_cursor_until_the_picker_closes() {
        let mut app = app();
        app.channels = vec![channel(1)];
        app.stub_roster();
        let id = app.channels[0].id;
        caught_up(&mut app);
        let event = message_event(&keys(), id, "unread", 20);
        let event_id = event.id.to_hex();
        app.apply(
            ChatEvent::CatchUp {
                channel: id,
                events: vec![event],
                complete: true,
            },
            0,
        );
        app.filter = Filter::Unread;
        app.refresh_views();
        app.selected = 0;

        // Reading it drops it from the filter it no longer matches...
        app.channels[0].rows = vec![Row {
            event_id,
            ..row("x", 20, "p")
        }];
        app.focus = 0;
        app.note_presented();
        app.refresh_views();
        assert!(app.view().all().next().is_none());

        // ...and the picker still opens on the conversation on screen, keeps
        // its row where it is, and says why it is still there.
        app.handle(Action::TogglePicker, 0);
        assert_eq!(picked(&app), Some(id));
        assert_eq!(
            app.picker_view().all().collect::<Vec<Uuid>>(),
            vec![id],
            "the row the cursor stands on does not vanish"
        );
        assert_eq!(app.marker(&app.channels[0]), Marker::Read);

        app.handle(Action::Dismiss, 0);
        assert_eq!(app.picker, None);
        assert!(
            app.view().all().next().is_none(),
            "closing the picker lets a filter drop what no longer matches"
        );
    }

    #[test]
    fn a_failed_marker_lookup_keeps_catch_up_pending_for_a_later_success() {
        let mut app = app();
        app.apply(ChatEvent::Channels(roster(vec![channel_info(1)])), 0);
        let id = app.channels[0].id;
        // The roster also asks to open the first conversation; it is unrelated
        // to the read-state retry being checked here.
        let _ = take_commands(&mut app);

        app.apply(
            ChatEvent::ReadState {
                contexts: HashMap::new(),
                complete: false,
            },
            0,
        );
        assert_eq!(app.marker(&app.channels[0]), Marker::Unknown);
        assert!(
            take_commands(&mut app)
                .into_iter()
                .all(|command| !matches!(command, SessionCommand::CatchUp(_)))
        );

        app.apply(
            ChatEvent::ReadState {
                contexts: HashMap::new(),
                complete: true,
            },
            0,
        );
        assert!(take_commands(&mut app).into_iter().any(|command| {
            matches!(
                command,
                SessionCommand::CatchUp(requests)
                    if requests.len() == 1 && requests[0].channel() == id
            )
        }));
    }

    #[test]
    fn a_seed_discards_live_candidates_that_precede_the_baseline() {
        let mut app = app();
        app.channels = vec![channel(1), channel(2)];
        app.stub_roster();
        caught_up(&mut app);
        let id = app.channels[1].id;
        app.apply(
            ChatEvent::InboxTimeline {
                channel: id,
                event: message_event(&keys(), id, "arrived before seed", 10),
            },
            10,
        );
        assert_eq!(app.channels[1].read.unread.len(), 1);

        app.apply(
            ChatEvent::Seed {
                channel: id,
                latest: Some(message_event(&keys(), id, "baseline", 20)),
                complete: true,
            },
            20,
        );
        assert!(
            app.channels[1].read.unread.is_empty(),
            "the seed proves the earlier live event was already present"
        );
    }

    #[test]
    fn closing_the_picker_presents_a_newest_row_that_arrived_while_it_was_open() {
        let mut app = app();
        app.channels = vec![channel(1)];
        app.stub_roster();
        caught_up(&mut app);
        let id = app.channels[0].id;
        let event = message_event(&keys(), id, "unread", 20);
        app.apply(
            ChatEvent::CatchUp {
                channel: id,
                events: vec![event.clone()],
                complete: true,
            },
            20,
        );
        app.channels[0].rows = vec![Row {
            event_id: event.id.to_hex(),
            ..row("unread", 20, "p")
        }];
        app.focus = 0;
        app.handle(Action::TogglePicker, 20);
        assert!(app.picker.is_some());

        app.handle(Action::Dismiss, 20);
        assert_eq!(app.marker(&app.channels[0]), Marker::None);
        assert!(published(&mut app).is_some());
    }

    #[test]
    fn disconnecting_marks_quiet_conversations_unknown_until_catch_up() {
        let mut app = app();
        app.channels = vec![channel(1)];
        app.stub_roster();
        caught_up(&mut app);
        app.apply(ChatEvent::Disconnected("socket closed".into()), 20);
        assert_eq!(app.marker(&app.channels[0]), Marker::Unknown);
        app.filter = Filter::Unread;
        assert_eq!(app.empty_view(), "Checking...");
    }

    #[test]
    fn failed_history_clears_loading_and_keeps_read_unknown() {
        let mut app = app();
        app.channels = vec![channel(1)];
        let id = app.channels[0].id;
        app.apply(
            ChatEvent::ReadState {
                contexts: HashMap::new(),
                complete: true,
            },
            0,
        );
        app.channels[0].loading = true;

        app.apply(
            ChatEvent::HistoryFailed {
                channel: id,
                reason: "relay refused".into(),
            },
            0,
        );

        assert!(!app.channels[0].loading);
        assert_eq!(app.channels[0].read.coverage, Coverage::Failed);
        assert_eq!(app.marker(&app.channels[0]), Marker::Unknown);
        assert!(app.status.contains("history failed"));
        app.apply(
            ChatEvent::Seed {
                channel: id,
                latest: Some(message_event(&keys(), id, "baseline", 10)),
                complete: true,
            },
            10,
        );
        assert_eq!(
            app.channels[0].read.coverage,
            Coverage::Failed,
            "a successful seed cannot replace a failed history load"
        );
        app.apply(
            ChatEvent::CatchUp {
                channel: id,
                events: Vec::new(),
                complete: true,
            },
            10,
        );
        assert_eq!(app.channels[0].read.coverage, Coverage::Failed);
        app.apply(
            ChatEvent::History {
                channel: id,
                events: vec![message_event(&keys(), id, "now loaded", 20)],
            },
            20,
        );
        assert_eq!(app.channels[0].read.coverage, Coverage::Pending);
        let requests = take_commands(&mut app);
        assert!(requests.into_iter().any(|command| {
            matches!(command, SessionCommand::CatchUp(batch)
                if batch.iter().any(|request| request.channel() == id))
        }));
        app.apply(
            ChatEvent::Seed {
                channel: id,
                latest: Some(message_event(&keys(), id, "now loaded", 20)),
                complete: true,
            },
            20,
        );
        assert_eq!(app.channels[0].read.coverage, Coverage::Complete);
    }
    #[test]
    fn a_history_retry_does_not_seed_through_an_unknown_marker() {
        let mut app = app();
        app.channels = vec![channel(1)];
        let id = app.channels[0].id;
        app.apply(
            ChatEvent::ReadState {
                contexts: HashMap::new(),
                complete: false,
            },
            0,
        );
        app.apply(
            ChatEvent::HistoryFailed {
                channel: id,
                reason: "relay refused".into(),
            },
            0,
        );
        app.apply(
            ChatEvent::History {
                channel: id,
                events: vec![message_event(&keys(), id, "loaded", 20)],
            },
            20,
        );
        assert_eq!(app.channels[0].read.coverage, Coverage::UnknownMarker);
        assert!(
            !take_commands(&mut app)
                .into_iter()
                .any(|command| matches!(command, SessionCommand::CatchUp(_)))
        );

        app.apply(
            ChatEvent::Seed {
                channel: id,
                latest: Some(message_event(&keys(), id, "must wait", 20)),
                complete: true,
            },
            20,
        );
        assert_eq!(app.channels[0].read.coverage, Coverage::UnknownMarker);

        app.apply(
            ChatEvent::ReadState {
                contexts: HashMap::new(),
                complete: true,
            },
            20,
        );
        assert_eq!(app.channels[0].read.coverage, Coverage::Pending);
        assert!(take_commands(&mut app).into_iter().any(|command| {
            matches!(command, SessionCommand::CatchUp(batch)
                if batch.iter().any(|request| request.channel() == id))
        }));
        app.apply(
            ChatEvent::Seed {
                channel: id,
                latest: Some(message_event(&keys(), id, "baseline", 20)),
                complete: true,
            },
            20,
        );
        assert_eq!(app.channels[0].read.coverage, Coverage::Complete);
    }
    #[test]
    fn an_inbox_failure_before_the_roster_stays_unknown() {
        let mut app = app();
        let id = channel_info(1).id;
        app.apply(ChatEvent::Connected, 0);
        let _ = take_commands(&mut app);
        app.apply(
            ChatEvent::InboxClosed {
                channel: id,
                reason: "refused".into(),
            },
            0,
        );
        app.apply(ChatEvent::Channels(roster(vec![channel_info(1)])), 0);

        assert_eq!(app.channels[0].read.coverage, Coverage::Failed);
        app.apply(
            ChatEvent::ReadState {
                contexts: HashMap::new(),
                complete: true,
            },
            0,
        );
        assert_eq!(app.channels[0].read.coverage, Coverage::Failed);
        assert_eq!(app.marker(&app.channels[0]), Marker::Unknown);
        app.apply(
            ChatEvent::Seed {
                channel: id,
                latest: Some(message_event(&keys(), id, "baseline", 10)),
                complete: true,
            },
            10,
        );
        assert_eq!(
            app.channels[0].read.coverage,
            Coverage::Failed,
            "a seed cannot turn a closed Inbox feed into a complete answer"
        );
    }

    #[test]
    fn history_keeps_a_live_event_that_arrived_before_the_http_answer() {
        let mut app = app();
        let id = channel(1).id;
        app.channels = vec![channel(1)];
        let live = message_event(&keys(), id, "live", 20);
        app.apply(
            ChatEvent::Timeline {
                channel: id,
                event: live.clone(),
            },
            20,
        );
        app.apply(
            ChatEvent::History {
                channel: id,
                events: vec![message_event(&keys(), id, "old", 10)],
            },
            21,
        );
        let bodies: Vec<&str> = app.channels[0]
            .rows
            .iter()
            .map(|row| row.body.as_str())
            .collect();
        assert_eq!(bodies, vec!["old", "live"]);
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

    const ADA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const BUILD: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn owned(names: &[(&str, &str)]) -> agents::Load {
        agents::Load::Loaded(agents::Roster {
            agents: names
                .iter()
                .map(|(name, pubkey)| agents::OwnedAgent {
                    pubkey: (*pubkey).to_owned(),
                    name: (*name).to_owned(),
                })
                .collect(),
            ..agents::Roster::default()
        })
    }

    fn frame(kind: agents::Kind, agent: &str, channel: Option<Uuid>, turn: &str) -> agents::Frame {
        agents::Frame {
            agent: agent.to_owned(),
            kind,
            channel: channel.map(|id| id.to_string()),
            turn: Some(turn.to_owned()),
            seq: 1,
        }
    }

    /// An open overlay with a roster and a live observer feed.
    fn watching(names: &[(&str, &str)]) -> App {
        let mut app = app();
        app.handle(Action::ToggleAgents, 0);
        app.apply(ChatEvent::Agents(owned(names)), 0);
        app.apply(ChatEvent::Connected, 0);
        app.apply(ChatEvent::ObserverReady, 0);
        // Opening the overlay and the first connection both ask; the tests
        // here are about what the keys and events do after that.
        let _ = app.take_outbox();
        app
    }

    #[test]
    fn a_opens_the_agents_overlay_and_asks_for_the_roster_once() {
        let mut app = app();
        app.handle(Action::ToggleAgents, 0);
        assert!(app.agents.open);
        assert!(matches!(
            take_commands(&mut app)[..],
            [SessionCommand::LoadAgents]
        ));
        app.handle(Action::ToggleAgents, 1);
        assert!(!app.agents.open, "a closes it again");
        assert!(take_commands(&mut app).is_empty());
    }

    #[test]
    fn the_agents_overlay_leaves_the_composer_alone_until_enter() {
        let mut app = watching(&[("Ada", ADA)]);
        app.handle(Action::AgentsConfirm, 1);
        assert_eq!(app.agents.cursor.level, agents::Level::Detail);
        assert!(
            take_commands(&mut app).is_empty(),
            "detail opens no conversation"
        );
        app.handle(Action::Dismiss, 2);
        assert_eq!(app.agents.cursor.level, agents::Level::List);
        assert!(app.agents.open, "the list stays open behind the detail");
    }

    #[test]
    fn the_agents_list_moves_and_clamps_to_the_roster_it_has() {
        let mut app = watching(&[("Ada", ADA), ("build", BUILD)]);
        app.handle(Action::AgentsNext, 1);
        assert_eq!(app.agents.cursor.cursor, 1);
        app.handle(Action::AgentsNext, 2);
        assert_eq!(app.agents.cursor.cursor, 1, "the cursor stops at the end");
        app.apply(ChatEvent::Agents(owned(&[("Ada", ADA)])), 3);
        assert_eq!(app.agents.cursor.cursor, 0, "a shorter roster clamps it");
    }

    #[test]
    fn enter_on_a_working_context_opens_that_conversation() {
        let mut app = watching(&[("Ada", ADA)]);
        let target = channel(2);
        let id = target.id;
        app.channels = vec![channel(1), target];
        app.apply(
            ChatEvent::ObserverFrame(frame(agents::Kind::Started, ADA, Some(id), "t1")),
            1,
        );
        assert_eq!(app.agent_status(ADA, 1), AgentStatus::Working(1));
        app.handle(Action::AgentsConfirm, 2);
        app.handle(Action::AgentsConfirm, 3);
        assert!(!app.agents.open, "opening a channel leaves the overlay");
        assert_eq!(app.channels[app.selected].id, id);
        assert!(matches!(
            take_commands(&mut app)[..],
            [SessionCommand::OpenChannel(opened)] if opened == id
        ));
    }

    #[test]
    fn work_in_a_conversation_the_user_is_not_in_is_not_a_destination() {
        let mut app = watching(&[("Ada", ADA)]);
        let elsewhere = Uuid::from_u64_pair(9, 0);
        app.channels = vec![channel(1)];
        app.apply(
            ChatEvent::ObserverFrame(frame(agents::Kind::Started, ADA, Some(elsewhere), "t1")),
            1,
        );
        assert_eq!(app.agent_contexts(ADA), vec![Context::Unavailable]);
        app.handle(Action::AgentsConfirm, 2);
        app.handle(Action::AgentsConfirm, 3);
        assert!(app.agents.open, "an unreachable context opens nothing");
        assert!(take_commands(&mut app).is_empty());
    }

    #[test]
    fn the_four_work_states_stay_separate() {
        let mut app = app();
        app.handle(Action::ToggleAgents, 0);
        app.apply(ChatEvent::Agents(owned(&[("Ada", ADA)])), 0);
        assert_eq!(
            app.agent_status(ADA, 0),
            AgentStatus::Unknown,
            "no connection has said anything yet"
        );
        app.apply(ChatEvent::Connected, 0);
        assert_eq!(
            app.agent_status(ADA, 0),
            AgentStatus::Unknown,
            "connected is not yet listening"
        );
        app.apply(ChatEvent::ObserverReady, 0);
        assert_eq!(app.agent_status(ADA, 0), AgentStatus::NoTurn);
        let id = channel(1).id;
        app.channels = vec![channel(1)];
        app.apply(
            ChatEvent::Typing {
                channel: id,
                pubkey: ADA.to_owned(),
                at: 0,
            },
            0,
        );
        let _ = app.take_outbox();
        assert_eq!(app.agent_status(ADA, 0), AgentStatus::Typing(id));
        app.apply(
            ChatEvent::ObserverFrame(frame(agents::Kind::Started, ADA, Some(id), "t1")),
            0,
        );
        assert_eq!(
            app.agent_status(ADA, 0),
            AgentStatus::Working(1),
            "a fresh turn outranks typing"
        );
        app.apply(ChatEvent::Disconnected("socket".into()), 0);
        assert_eq!(
            app.agent_status(ADA, 0),
            AgentStatus::Unknown,
            "a lost connection is not a quiet Agent"
        );
    }

    #[test]
    fn a_turn_that_stops_reporting_retires_and_says_so() {
        let mut app = watching(&[("Ada", ADA)]);
        let id = channel(1).id;
        app.apply(
            ChatEvent::ObserverFrame(frame(agents::Kind::Started, ADA, Some(id), "t1")),
            100,
        );
        app.expire_agents(100 + crate::agents::TURN_FRESH_SECS);
        assert_eq!(
            app.agent_status(ADA, 100 + crate::agents::TURN_FRESH_SECS),
            AgentStatus::Working(1),
            "the boundary second is still fresh"
        );
        app.expire_agents(101 + crate::agents::TURN_FRESH_SECS);
        assert_eq!(
            app.agent_status(ADA, 101 + crate::agents::TURN_FRESH_SECS),
            AgentStatus::NoTurn
        );
    }

    #[test]
    fn a_closed_observer_feed_clears_the_work_and_says_why() {
        let mut app = watching(&[("Ada", ADA)]);
        let id = channel(1).id;
        app.apply(
            ChatEvent::ObserverFrame(frame(agents::Kind::Started, ADA, Some(id), "t1")),
            1,
        );
        app.apply(
            ChatEvent::ObserverClosed {
                reason: "restricted".into(),
            },
            2,
        );
        assert_eq!(app.agent_status(ADA, 2), AgentStatus::Unknown);
        assert!(app.agent_last_signal(ADA).is_none());
        assert!(app.status.contains("observer feed closed"));
        assert!(app.status.contains("restricted"));
    }

    #[test]
    fn a_failed_roster_read_is_not_an_empty_roster() {
        let mut app = app();
        app.handle(Action::ToggleAgents, 0);
        app.apply(
            ChatEvent::Agents(agents::Load::Unavailable("no owner roster".into())),
            0,
        );
        assert_eq!(app.agent_count(), 0);
        assert!(app.selected_agent().is_none());
    }
}
