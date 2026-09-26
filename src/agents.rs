//! The owner's Agents: which Agents the login identity owns, and the minimal
//! projection of owner-private observer frames that says which of them are
//! working, and where. Pure: events in, state out, and the clock arrives as an
//! argument.
//!
//! The boundary is design/decisions/0006-agent-summary.md. A frame is reduced
//! to identity, channel, turn and terminal state, and everything else it
//! carried is dropped here. Nothing in this module renders, persists, logs, or
//! forwards a payload.

use std::collections::{BTreeMap, HashMap, VecDeque};

use nostr::{Event, PublicKey};
use serde_json::Value;

/// The owner-authored managed-agent record. Its `d` tag names the Agent.
pub const KIND_MANAGED_AGENT: u16 = 30177;
/// The owner-scoped observer frame, encrypted to the owner with NIP-44.
pub const KIND_OBSERVER_FRAME: u16 = 24200;

/// How long an observed turn stays working with no frame behind it.
///
/// The harness emits `turn_liveness` every ten seconds
/// (`BUZZ_ACP_TURN_LIVENESS_SECS`, default 10), so this bound tolerates one
/// dropped ping plus slack. A turn whose host was killed without a terminal
/// frame must not stay working forever, and a live one must not blink off
/// between two pings.
pub const TURN_FRESH_SECS: u64 = 25;

/// How many managed-agent records one roster read carries. An owner with more
/// Agents than this hears `List incomplete` rather than a truncated list
/// presented as whole.
pub const ROSTER_LIMIT: u64 = 200;

/// Turns one Agent may have observed at once. The harness runs at most 32
/// Agent processes per pool, so this bound never evicts a live turn.
const TURNS_PER_AGENT: usize = 32;
/// Terminal turn ids remembered per Agent. A frame for a finished turn can
/// only arrive a moment late, so a short history is enough to refuse it.
const ENDED_PER_AGENT: usize = 32;
/// Agents the projection keeps state for. The observer stream is owner-scoped,
/// so this is the owner's own Agent count; the cap only bounds a relay that
/// answers with more than the deployment can run.
const AGENTS: usize = 64;

/// One Agent the login identity owns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedAgent {
    /// The Agent's identity, from the record's `d` tag.
    pub pubkey: String,
    /// The name the owner gave it.
    pub name: String,
}

/// The owned roster, as read from the relay.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Roster {
    /// Verified records: authored by the login identity, with a name and a
    /// usable Agent identity. Sorted by name, then key, so the list never
    /// reshuffles between reads.
    pub agents: Vec<OwnedAgent>,
    /// Records the relay answered with that this identity did not sign. They
    /// are not claimed as owned.
    pub foreign: usize,
    /// Records that name this identity but do not verify. Ownership is not
    /// claimed for a record that cannot be authenticated.
    pub unverified: usize,
    /// Records that cannot be read as an Agent: no name, or no Agent identity.
    pub unreadable: usize,
    /// True when the read hit its own page limit, so more records may exist.
    pub truncated: bool,
}

impl Roster {
    /// Read the roster out of the relay's answer. `me` is the login identity:
    /// the record's author is what makes the Agent the user's own, so a record
    /// signed by anyone else is counted and dropped.
    pub fn from_events(events: &[Event], me: &str) -> Self {
        let mut roster = Roster {
            truncated: events.len() as u64 >= ROSTER_LIMIT,
            ..Roster::default()
        };
        for event in events {
            if event.kind.as_u16() as u32 != KIND_MANAGED_AGENT as u32 {
                continue;
            }
            if event.pubkey.to_hex() != me {
                roster.foreign += 1;
                continue;
            }
            if event.verify().is_err() {
                // A record the relay attributes to this identity that does not
                // verify is not an owned Agent: it is counted, never claimed.
                roster.unverified += 1;
                continue;
            }
            let Some(pubkey) = d_tag(event) else {
                roster.unreadable += 1;
                continue;
            };
            if PublicKey::parse(&pubkey).is_err() {
                roster.unreadable += 1;
                continue;
            }
            let Some(name) = name_of(event) else {
                roster.unreadable += 1;
                continue;
            };
            roster.agents.push(OwnedAgent { pubkey, name });
        }
        roster.agents.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.pubkey.cmp(&b.pubkey))
        });
        roster
    }

    /// Whether the answer was not the whole roster.
    pub fn incomplete(&self) -> bool {
        self.foreign > 0 || self.unverified > 0 || self.unreadable > 0 || self.truncated
    }
}

/// What the client knows about the roster. The four states are separate
/// because they call for four different sentences: an empty roster, a refusal,
/// a failed read, and a read this identity is not allowed to make.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Load {
    /// Never asked.
    #[default]
    Unknown,
    /// The relay refused the read: this identity cannot see an owner roster.
    Unavailable(String),
    /// The read failed, or its answer could not be read at all.
    Failed(String),
    /// An answer arrived.
    Loaded(Roster),
}

fn d_tag(event: &Event) -> Option<String> {
    event.tags.iter().find_map(|tag| {
        let parts = tag.as_slice();
        (parts.first().map(String::as_str) == Some("d"))
            .then(|| parts.get(1).cloned())
            .flatten()
    })
}

fn name_of(event: &Event) -> Option<String> {
    let content: Value = serde_json::from_str(&event.content).ok()?;
    let name = content.get("name")?.as_str()?.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

/// What one observer frame says about one Agent's work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A turn began.
    Started,
    /// A live turn reported that it is still running.
    Liveness,
    /// A turn ended without a failure.
    Completed,
    /// A turn ended with an error.
    Failed,
    /// The Agent's process died.
    Panic,
    /// Any other frame from inside a turn: it is evidence of work, nothing
    /// more.
    Activity,
}

/// The fields of one observer frame this client keeps. Everything else the
/// frame carried is dropped when it is decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// The Agent that emitted it.
    pub agent: String,
    pub kind: Kind,
    /// The conversation the turn belongs to, when it is channel-scoped.
    pub channel: Option<String>,
    /// The turn's identity. Upstream assigns it; this client never invents
    /// one except for a frame that has none, where the producer's own sequence
    /// number is the best identity available.
    pub turn: Option<String>,
    /// The producer's process-local sequence number. It is only an identity
    /// fallback within one producer, never an ordering across producers.
    pub seq: u64,
}

/// Read a decrypted observer payload into frames. A batch envelope carries the
/// events it packed; a single frame is one event. A row that is not a frame at
/// all is dropped: a summary is not worth guessing at.
pub fn frames(payload: &Value, agent: &str) -> Vec<Frame> {
    if payload.get("kind").and_then(Value::as_str) == Some("batch") {
        let Some(inner) = payload.get("events").and_then(Value::as_array) else {
            return Vec::new();
        };
        return inner
            .iter()
            .filter_map(|event| frame_of(event, agent))
            .collect();
    }
    frame_of(payload, agent).into_iter().collect()
}

fn frame_of(value: &Value, agent: &str) -> Option<Frame> {
    let label = value.get("kind")?.as_str()?;
    let kind = match label {
        "turn_started" => Kind::Started,
        "turn_liveness" => Kind::Liveness,
        "turn_completed" => Kind::Completed,
        "turn_error" => Kind::Failed,
        "agent_panic" => Kind::Panic,
        _ => Kind::Activity,
    };
    Some(Frame {
        agent: agent.to_owned(),
        kind,
        channel: string_field(value, "channelId"),
        turn: string_field(value, "turnId"),
        seq: value.get("seq").and_then(Value::as_u64).unwrap_or(0),
    })
}

fn string_field(value: &Value, name: &str) -> Option<String> {
    value
        .get(name)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// One observed turn of one Agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    /// The conversation it runs in, when the frames carried one.
    pub channel: Option<String>,
    /// The client's clock at the last frame for this turn.
    pub last_activity: u64,
}

#[derive(Debug, Default)]
struct AgentWork {
    turns: BTreeMap<String, Turn>,
    ended: VecDeque<String>,
    last_signal: Option<u64>,
    failed_at: Option<u64>,
}

impl AgentWork {
    fn key(frame: &Frame) -> String {
        frame
            .turn
            .clone()
            .unwrap_or_else(|| format!("seq-{}", frame.seq))
    }

    fn insert(&mut self, key: String, frame: &Frame, now: u64) {
        if self.turns.len() >= TURNS_PER_AGENT
            && !self.turns.contains_key(&key)
            && let Some(oldest) = self
                .turns
                .iter()
                .min_by_key(|(_, turn)| turn.last_activity)
                .map(|(key, _)| key.clone())
        {
            self.turns.remove(&oldest);
        }
        self.turns.insert(
            key,
            Turn {
                channel: frame.channel.clone(),
                last_activity: now,
            },
        );
    }

    fn tombstone(&mut self, key: String) {
        if self.ended.contains(&key) {
            return;
        }
        while self.ended.len() >= ENDED_PER_AGENT {
            self.ended.pop_front();
        }
        self.ended.push_back(key);
    }

    /// End the turn a terminal frame names: its own turn, or the newest turn
    /// in the channel it names. A terminal frame that identifies neither is
    /// not evidence about any other turn, so it ends nothing.
    fn end(&mut self, frame: &Frame) -> Option<String> {
        if let Some(turn) = &frame.turn {
            self.turns.remove(turn);
            return Some(turn.clone());
        }
        let channel = frame.channel.as_deref()?;
        let key = self
            .turns
            .iter()
            .filter(|(_, turn)| turn.channel.as_deref() == Some(channel))
            .max_by_key(|(_, turn)| turn.last_activity)
            .map(|(key, _)| key.clone())?;
        self.turns.remove(&key);
        Some(key)
    }

    /// Drop turns no frame has refreshed inside the freshness bound. Returns
    /// whether anything went.
    fn expire_stale(&mut self, now: u64) -> bool {
        let stale = |turn: &Turn| now.saturating_sub(turn.last_activity) > TURN_FRESH_SECS;
        if !self.turns.values().any(stale) {
            return false;
        }
        self.turns.retain(|_, turn| !stale(turn));
        true
    }
}

/// Every Agent's observed work, keyed by Agent identity.
#[derive(Debug, Default)]
pub struct Work {
    agents: HashMap<String, AgentWork>,
}

impl Work {
    /// Apply one frame. `now` is this client's clock: the producer's timestamps
    /// are never trusted for freshness, because the producer's clock and this
    /// one are not the same clock.
    pub fn apply(&mut self, frame: &Frame, now: u64) {
        if !self.agents.contains_key(&frame.agent)
            && self.agents.len() >= AGENTS
            && let Some(oldest) = self
                .agents
                .iter()
                .min_by_key(|(_, work)| work.last_signal.unwrap_or(0))
                .map(|(agent, _)| agent.clone())
        {
            self.agents.remove(&oldest);
        }
        let work = self.agents.entry(frame.agent.clone()).or_default();
        work.last_signal = Some(now);
        match frame.kind {
            Kind::Started => {
                let key = AgentWork::key(frame);
                if work.ended.contains(&key) {
                    // A replay of a turn that already ended is not a new turn.
                    return;
                }
                work.insert(key, frame, now);
            }
            Kind::Liveness | Kind::Activity => {
                let key = AgentWork::key(frame);
                if work.ended.contains(&key) {
                    return;
                }
                match work.turns.get_mut(&key) {
                    Some(turn) => turn.last_activity = now,
                    // A live turn this client never saw start: joining late is
                    // normal, and a fresh liveness signal is what recovers it.
                    None if frame.channel.is_some() => work.insert(key, frame, now),
                    None => {}
                }
            }
            Kind::Completed | Kind::Failed => {
                if let Some(key) = work.end(frame) {
                    work.tombstone(key);
                }
                if frame.kind == Kind::Failed {
                    work.failed_at = Some(now);
                }
            }
            Kind::Panic => {
                work.failed_at = Some(now);
                if frame.turn.is_none() && frame.channel.is_none() {
                    // The process died with no turn named: none of its work is
                    // still running, and no tombstone can be keyed.
                    work.turns.clear();
                } else if let Some(key) = work.end(frame) {
                    work.tombstone(key);
                }
            }
        }
    }

    /// Retire turns no frame has refreshed inside the freshness bound. Returns
    /// whether anything changed, so the caller can decide to redraw.
    pub fn expire(&mut self, now: u64) -> bool {
        let mut changed = false;
        for work in self.agents.values_mut() {
            changed |= work.expire_stale(now);
        }
        changed
    }

    /// Drop everything. Work is observed, not stored: a lost observation, a
    /// new identity, or a lost subscription leaves nothing to show.
    pub fn clear(&mut self) {
        self.agents.clear();
    }

    /// The Agent's working turns, newest activity first, then by turn key so
    /// the order is stable.
    pub fn working(&self, agent: &str) -> Vec<(&str, &Turn)> {
        let Some(work) = self.agents.get(agent) else {
            return Vec::new();
        };
        let mut turns: Vec<(&str, &Turn)> = work
            .turns
            .iter()
            .map(|(key, turn)| (key.as_str(), turn))
            .collect();
        turns.sort_by(|a, b| {
            b.1.last_activity
                .cmp(&a.1.last_activity)
                .then_with(|| a.0.cmp(b.0))
        });
        turns
    }

    /// When a frame from this Agent last arrived, on this client's clock.
    pub fn last_signal(&self, agent: &str) -> Option<u64> {
        self.agents.get(agent).and_then(|work| work.last_signal)
    }

    /// When this Agent's last observed turn failed, if any did.
    pub fn last_failure(&self, agent: &str) -> Option<u64> {
        self.agents.get(agent).and_then(|work| work.failed_at)
    }
}

/// The Agent list's cursor and which level it is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    List,
    Detail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct View {
    pub level: Level,
    pub cursor: usize,
    /// The selected working context on the detail level.
    pub context: usize,
}

impl Default for View {
    fn default() -> Self {
        Self {
            level: Level::List,
            cursor: 0,
            context: 0,
        }
    }
}

impl View {
    /// Move the cursor by `step`, inside a list of `len` entries.
    pub fn move_by(&mut self, step: isize, len: usize) {
        let target = if self.level == Level::List {
            &mut self.cursor
        } else {
            &mut self.context
        };
        let last = len.saturating_sub(1) as isize;
        *target = (*target as isize + step).clamp(0, last) as usize;
    }

    /// Keep the cursor inside its list after the data changed under it.
    pub fn clamp(&mut self, agents: usize, contexts: usize) {
        self.cursor = self.cursor.min(agents.saturating_sub(1));
        self.context = self.context.min(contexts.saturating_sub(1));
    }

    /// Back one level, or out of the overlay.
    pub fn back(&mut self) -> bool {
        match self.level {
            Level::List => false,
            Level::Detail => {
                self.level = Level::List;
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::{EventBuilder, Keys, Kind as NostrKind, Tag};

    const AGENT: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const OTHER: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

    fn record(keys: &Keys, agent: &str, name: &str) -> Event {
        EventBuilder::new(
            NostrKind::Custom(KIND_MANAGED_AGENT),
            serde_json::json!({ "name": name }).to_string(),
        )
        .tags([Tag::parse(["d", agent]).unwrap()])
        .sign_with_keys(keys)
        .unwrap()
    }

    #[test]
    fn the_roster_holds_only_records_this_identity_signed() {
        let me = Keys::generate();
        let someone_else = Keys::generate();
        let mine = record(&me, AGENT, "Build");
        let theirs = record(&someone_else, OTHER, "Not mine");
        let roster = Roster::from_events(&[theirs, mine], &me.public_key().to_hex());
        assert_eq!(roster.agents.len(), 1);
        assert_eq!(roster.agents[0].name, "Build");
        assert_eq!(roster.agents[0].pubkey, AGENT);
        assert_eq!(roster.foreign, 1);
        assert!(roster.incomplete());
    }

    #[test]
    fn a_record_without_a_name_or_an_identity_is_counted_not_shown() {
        let me = Keys::generate();
        let nameless = EventBuilder::new(
            NostrKind::Custom(KIND_MANAGED_AGENT),
            serde_json::json!({ "name": "   " }).to_string(),
        )
        .tags([Tag::parse(["d", AGENT]).unwrap()])
        .sign_with_keys(&me)
        .unwrap();
        let keyless = EventBuilder::new(
            NostrKind::Custom(KIND_MANAGED_AGENT),
            serde_json::json!({ "name": "Build" }).to_string(),
        )
        .tags([Tag::parse(["d", "not-a-pubkey"]).unwrap()])
        .sign_with_keys(&me)
        .unwrap();
        let roster = Roster::from_events(&[nameless, keyless], &me.public_key().to_hex());
        assert!(roster.agents.is_empty());
        assert_eq!(roster.unreadable, 2);
        assert!(roster.incomplete());
    }

    #[test]
    fn a_record_that_does_not_verify_is_counted_never_owned() {
        let me = Keys::generate();
        let mut tampered = record(&me, AGENT, "Build");
        // The id no longer matches the content it was signed over.
        tampered.id = nostr::EventId::all_zeros();
        let roster = Roster::from_events(&[tampered], &me.public_key().to_hex());
        assert!(roster.agents.is_empty());
        assert_eq!(roster.unverified, 1);
        assert!(roster.incomplete());
    }

    #[test]
    fn the_roster_is_ordered_by_name_and_never_reshuffles() {
        let me = Keys::generate();
        let b = record(&me, AGENT, "build");
        let a = record(&me, OTHER, "Ada");
        let roster = Roster::from_events(&[b, a], &me.public_key().to_hex());
        let names: Vec<&str> = roster.agents.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["Ada", "build"]);
    }

    fn frame(kind: Kind, channel: Option<&str>, turn: Option<&str>) -> Frame {
        Frame {
            agent: AGENT.to_owned(),
            kind,
            channel: channel.map(str::to_owned),
            turn: turn.map(str::to_owned),
            seq: 1,
        }
    }

    #[test]
    fn a_started_turn_is_working_and_a_completion_ends_only_it() {
        let mut work = Work::default();
        work.apply(&frame(Kind::Started, Some("chan-a"), Some("t1")), 100);
        work.apply(&frame(Kind::Started, Some("chan-b"), Some("t2")), 101);
        assert_eq!(work.working(AGENT).len(), 2);
        work.apply(&frame(Kind::Completed, Some("chan-a"), Some("t1")), 102);
        let left = work.working(AGENT);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].1.channel.as_deref(), Some("chan-b"));
    }

    #[test]
    fn a_late_liveness_recovers_a_turn_that_was_never_seen_to_start() {
        let mut work = Work::default();
        work.apply(&frame(Kind::Liveness, Some("chan-a"), Some("t9")), 500);
        assert_eq!(work.working(AGENT).len(), 1);
    }

    #[test]
    fn a_terminal_frame_ends_a_turn_a_late_liveness_may_not_revive() {
        let mut work = Work::default();
        work.apply(&frame(Kind::Started, Some("chan-a"), Some("t1")), 100);
        work.apply(&frame(Kind::Failed, Some("chan-a"), Some("t1")), 110);
        assert!(work.working(AGENT).is_empty());
        assert_eq!(work.last_failure(AGENT), Some(110));
        // The frame stream replays the turn's start after the terminal.
        work.apply(&frame(Kind::Started, Some("chan-a"), Some("t1")), 120);
        assert!(work.working(AGENT).is_empty());
        work.apply(&frame(Kind::Liveness, Some("chan-a"), Some("t1")), 125);
        assert!(work.working(AGENT).is_empty());
        // A different turn of the same Agent is unaffected.
        work.apply(&frame(Kind::Started, Some("chan-b"), Some("t2")), 130);
        assert_eq!(work.working(AGENT).len(), 1);
    }

    #[test]
    fn a_turn_no_frame_refreshes_expires_at_the_bound_and_not_before() {
        let mut work = Work::default();
        work.apply(&frame(Kind::Started, Some("chan-a"), Some("t1")), 1_000);
        assert!(!work.expire(1_000 + TURN_FRESH_SECS));
        assert_eq!(work.working(AGENT).len(), 1);
        assert!(work.expire(1_000 + TURN_FRESH_SECS + 1));
        assert!(work.working(AGENT).is_empty());
    }

    #[test]
    fn a_liveness_refresh_moves_the_bound() {
        let mut work = Work::default();
        work.apply(&frame(Kind::Started, Some("chan-a"), Some("t1")), 0);
        work.apply(&frame(Kind::Liveness, Some("chan-a"), Some("t1")), 20);
        assert!(!work.expire(40));
        assert_eq!(work.working(AGENT).len(), 1);
        assert!(work.expire(46));
    }

    #[test]
    fn activity_frames_of_a_running_tool_count_as_evidence_of_work() {
        let mut work = Work::default();
        work.apply(&frame(Kind::Activity, Some("chan-a"), Some("t1")), 10);
        assert_eq!(work.working(AGENT).len(), 1);
        work.apply(&frame(Kind::Activity, Some("chan-a"), Some("t1")), 30);
        assert!(!work.expire(54));
    }

    #[test]
    fn a_panic_with_no_turn_clears_every_observed_turn() {
        let mut work = Work::default();
        work.apply(&frame(Kind::Started, Some("chan-a"), Some("t1")), 10);
        work.apply(&frame(Kind::Started, Some("chan-b"), Some("t2")), 11);
        work.apply(&frame(Kind::Panic, None, None), 12);
        assert!(work.working(AGENT).is_empty());
        assert_eq!(work.last_failure(AGENT), Some(12));
    }

    #[test]
    fn a_terminal_with_a_channel_but_no_turn_ends_that_channel_only() {
        let mut work = Work::default();
        work.apply(&frame(Kind::Started, Some("chan-a"), Some("t1")), 10);
        work.apply(&frame(Kind::Started, Some("chan-b"), Some("t2")), 11);
        work.apply(&frame(Kind::Completed, Some("chan-a"), None), 12);
        let left = work.working(AGENT);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].1.channel.as_deref(), Some("chan-b"));
    }

    #[test]
    fn one_agents_work_never_touches_another() {
        let mut work = Work::default();
        work.apply(&frame(Kind::Started, Some("chan-a"), Some("t1")), 10);
        let mut other = frame(Kind::Completed, Some("chan-a"), Some("t1"));
        other.agent = OTHER.to_owned();
        work.apply(&other, 11);
        assert_eq!(work.working(AGENT).len(), 1);
        assert!(work.working(OTHER).is_empty());
    }

    #[test]
    fn clearing_leaves_nothing_to_describe_as_current() {
        let mut work = Work::default();
        work.apply(&frame(Kind::Started, Some("chan-a"), Some("t1")), 10);
        work.clear();
        assert!(work.working(AGENT).is_empty());
        assert_eq!(work.last_signal(AGENT), None);
    }

    #[test]
    fn the_projection_bounds_the_turns_it_keeps_per_agent() {
        let mut work = Work::default();
        for index in 0..(TURNS_PER_AGENT + 8) {
            work.apply(
                &frame(Kind::Started, Some("chan-a"), Some(&format!("t{index}"))),
                index as u64,
            );
        }
        assert_eq!(work.working(AGENT).len(), TURNS_PER_AGENT);
        // The newest turn is what a reader will look for; the oldest is gone.
        assert!(work.working(AGENT).iter().all(|(key, _)| *key != "t0"));
    }

    #[test]
    fn the_projection_bounds_the_agents_it_keeps() {
        let mut work = Work::default();
        for index in 0..(AGENTS + 4) {
            let mut event = frame(Kind::Started, Some("chan-a"), Some("t1"));
            event.agent = format!("{index:064x}");
            work.apply(&event, index as u64);
        }
        let kept = (0..(AGENTS + 4))
            .filter(|index| !work.working(&format!("{index:064x}")).is_empty())
            .count();
        assert_eq!(kept, AGENTS);
    }

    #[test]
    fn a_batch_envelope_yields_every_inner_frame() {
        let payload = serde_json::json!({
            "kind": "batch",
            "seq": 3,
            "events": [
                { "kind": "turn_started", "seq": 1, "channelId": "chan-a", "turnId": "t1" },
                { "kind": "acp_read", "seq": 2, "channelId": "chan-a", "turnId": "t1" },
                { "kind": "turn_completed", "seq": 3, "channelId": "chan-a", "turnId": "t1" },
            ],
        });
        let frames = frames(&payload, AGENT);
        let kinds: Vec<Kind> = frames.iter().map(|frame| frame.kind).collect();
        assert_eq!(kinds, vec![Kind::Started, Kind::Activity, Kind::Completed]);
        let mut work = Work::default();
        for frame in &frames {
            work.apply(frame, 10);
        }
        assert!(work.working(AGENT).is_empty());
    }

    #[test]
    fn a_frame_without_a_turn_uses_the_producer_sequence_as_its_identity() {
        let payload = serde_json::json!({
            "kind": "turn_liveness",
            "seq": 7,
            "channelId": "chan-a",
        });
        let frames = frames(&payload, AGENT);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].turn, None);
        let mut work = Work::default();
        work.apply(&frames[0], 10);
        assert_eq!(work.working(AGENT).len(), 1);
    }

    #[test]
    fn an_unreadable_payload_yields_no_frames() {
        assert!(frames(&serde_json::json!({ "kind": "batch" }), AGENT).is_empty());
        assert!(frames(&serde_json::json!({ "seq": 3 }), AGENT).is_empty());
    }

    #[test]
    fn the_view_backs_out_of_the_detail_level_once_but_not_out_of_the_list() {
        let mut view = View {
            level: Level::Detail,
            ..View::default()
        };
        assert!(view.back(), "the detail level returns to the list");
        assert_eq!(view.level, Level::List);
        assert!(!view.back(), "the list level closes the overlay");
    }

    #[test]
    fn the_view_cursor_stays_inside_the_list_it_moves_in() {
        let mut view = View {
            level: Level::Detail,
            context: 5,
            ..View::default()
        };
        // A cursor left over from a longer list lands on the last entry.
        view.move_by(-1, 3);
        assert_eq!(view.context, 2);
        view.move_by(-1, 3);
        assert_eq!(view.context, 1);
        view.context = 9;
        view.clamp(2, 3);
        assert_eq!(view.context, 2);
        // Nothing to point at is not a cursor past the end.
        view.clamp(0, 0);
        assert_eq!((view.cursor, view.context), (0, 0));
    }
}
